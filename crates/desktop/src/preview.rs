//! A bounded, demand-driven image cache. The UI never decodes image pixels.
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

const THUMBNAILS: usize = 64;
const THUMB_EDGE: u32 = 128;
const PREVIEW_EDGE: u32 = 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    edge: u32,
    epoch: u64,
}
struct Decoded {
    key: Key,
    pixels: Result<ColorImage, String>,
    deferred: bool,
}
pub struct PreviewCache {
    requests: SyncSender<Key>,
    results: Receiver<Decoded>,
    textures: HashMap<Key, TextureHandle>,
    errors: HashMap<Key, String>,
    pending: HashSet<Key>,
    order: VecDeque<Key>,
    epoch: u64,
    animation_generation: Arc<AtomicU64>,
    animation_results: Receiver<(u64, Result<ColorImage, String>)>,
    animation_sender: SyncSender<(u64, Result<ColorImage, String>)>,
    decode_gate: Arc<Mutex<()>>,
    processing: Arc<AtomicBool>,
    pub animation_texture: Option<TextureHandle>,
    pub animation_path: Option<PathBuf>,
    pub animation_error: Option<String>,
}

impl PreviewCache {
    pub fn new(ctx: egui::Context) -> Self {
        let (requests, incoming) = mpsc::sync_channel::<Key>(12);
        let (outgoing, results) = mpsc::sync_channel(3);
        let (animation_sender, animation_results) = mpsc::sync_channel(2);
        let decode_gate = Arc::new(Mutex::new(()));
        let static_gate = decode_gate.clone();
        let processing = Arc::new(AtomicBool::new(false));
        let static_processing = processing.clone();
        thread::Builder::new()
            .name("image-previews".into())
            .spawn(move || {
                while let Ok(key) = incoming.recv() {
                    let guard = static_gate.lock().unwrap_or_else(|p| p.into_inner());
                    let deferred = static_processing.load(Ordering::Acquire);
                    let pixels = if deferred {
                        Err(String::new())
                    } else {
                        decode(&key.path, key.edge)
                    };
                    drop(guard);
                    if outgoing
                        .send(Decoded {
                            key,
                            pixels,
                            deferred,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("preview worker");
        Self {
            requests,
            results,
            textures: HashMap::new(),
            errors: HashMap::new(),
            pending: HashSet::new(),
            order: VecDeque::new(),
            epoch: 0,
            animation_generation: Arc::new(AtomicU64::new(0)),
            animation_results,
            animation_sender,
            decode_gate,
            processing,
            animation_texture: None,
            animation_path: None,
            animation_error: None,
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.results.try_recv() {
            if result.key.epoch != self.epoch {
                continue;
            }
            self.pending.remove(&result.key);
            if result.deferred {
                continue;
            }
            match result.pixels {
                Ok(pixels) => {
                    self.textures.insert(
                        result.key.clone(),
                        ctx.load_texture(
                            format!("{}-{}", result.key.path.display(), result.key.edge),
                            pixels,
                            TextureOptions::LINEAR,
                        ),
                    );
                }
                Err(error) => {
                    self.errors.insert(result.key.clone(), error);
                }
            }
            self.order.push_back(result.key);
            while self.order.iter().filter(|k| k.edge == THUMB_EDGE).count() > THUMBNAILS
                || self.order.iter().filter(|k| k.edge == PREVIEW_EDGE).count() > 2
            {
                let over_preview = self.order.iter().filter(|k| k.edge == PREVIEW_EDGE).count() > 2;
                let index = self
                    .order
                    .iter()
                    .position(|k| (k.edge == PREVIEW_EDGE) == over_preview);
                if let Some(key) = index.and_then(|i| self.order.remove(i)) {
                    self.textures.remove(&key);
                    self.errors.remove(&key);
                } else {
                    break;
                }
            }
        }
        while let Ok((generation, pixels)) = self.animation_results.try_recv() {
            if generation != self.animation_generation.load(Ordering::Relaxed) {
                continue;
            }
            match pixels {
                Ok(pixels) => {
                    if let Some(texture) = &mut self.animation_texture {
                        texture.set(pixels, TextureOptions::LINEAR);
                    } else {
                        self.animation_texture =
                            Some(ctx.load_texture("gif-playback", pixels, TextureOptions::LINEAR));
                    }
                }
                Err(error) => {
                    self.animation_error = Some(error);
                    self.stop_animation();
                }
            }
        }
    }

    pub fn texture(&mut self, path: &Path, full: bool) -> Option<TextureHandle> {
        let key = Key {
            path: path.to_path_buf(),
            edge: if full { PREVIEW_EDGE } else { THUMB_EDGE },
            epoch: self.epoch,
        };
        if let Some(texture) = self.textures.get(&key) {
            self.order.retain(|k| k != &key);
            self.order.push_back(key.clone());
            return Some(texture.clone());
        }
        if !self.processing.load(Ordering::Relaxed)
            && !self.pending.contains(&key)
            && !self.errors.contains_key(&key)
            && self.requests.try_send(key.clone()).is_ok()
        {
            self.pending.insert(key);
        }
        None
    }

    pub fn error(&self, path: &Path, full: bool) -> Option<&str> {
        self.errors
            .get(&Key {
                path: path.to_path_buf(),
                edge: if full { PREVIEW_EDGE } else { THUMB_EDGE },
                epoch: self.epoch,
            })
            .map(String::as_str)
    }

    pub fn stop_animation(&mut self) {
        self.animation_generation.fetch_add(1, Ordering::Relaxed);
        self.animation_path = None;
        self.animation_texture = None;
    }

    pub fn processing(&self, active: bool) {
        self.processing.store(active, Ordering::Release);
    }
    pub fn decode_gate(&self) -> Arc<Mutex<()>> {
        self.decode_gate.clone()
    }

    pub fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.textures.clear();
        self.errors.clear();
        self.order.clear();
        self.pending.clear();
        self.stop_animation();
    }

    pub fn play(&mut self, path: PathBuf, ctx: egui::Context) {
        self.stop_animation();
        self.animation_error = None;
        self.animation_path = Some(path.clone());
        let generation = self.animation_generation.load(Ordering::Relaxed);
        let state = self.animation_generation.clone();
        let sender = self.animation_sender.clone();
        let gate = self.decode_gate.clone();
        thread::Builder::new()
            .name("gif-preview".into())
            .spawn(move || {
                let _guard = gate.lock().unwrap_or_else(|p| p.into_inner());
                let result = play_animation(&path, generation, &state, &sender, &ctx);
                if let Err(error) = result {
                    let _ = sender.try_send((generation, Err(error)));
                    ctx.request_repaint();
                }
            })
            .expect("GIF preview worker");
    }
}

impl Drop for PreviewCache {
    fn drop(&mut self) {
        self.animation_generation.fetch_add(1, Ordering::Relaxed);
    }
}

fn decode(path: &Path, edge: u32) -> Result<ColorImage, String> {
    let rgba = local_image_core::pipeline::preview(path, edge).map_err(|e| e.to_string())?;
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

fn play_animation(
    path: &Path,
    generation: u64,
    state: &AtomicU64,
    sender: &SyncSender<(u64, Result<ColorImage, String>)>,
    ctx: &egui::Context,
) -> Result<(), String> {
    // Decode and composite one frame at a time. No animation frame history is retained.
    while generation == state.load(Ordering::Relaxed) {
        let mut count = 0;
        let mut disconnected = false;
        local_image_core::gif_codec::preview_frames(
            path,
            PREVIEW_EDGE,
            128 * 1024 * 1024,
            |rgba, delay| {
                if generation != state.load(Ordering::Relaxed) {
                    return false;
                }
                let pixels = ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    rgba.as_raw(),
                );
                if sender.send((generation, Ok(pixels))).is_err() {
                    disconnected = true;
                    return false;
                }
                ctx.request_repaint();
                let mut remaining = delay.clamp(20, 60_000);
                while remaining > 0 && generation == state.load(Ordering::Relaxed) {
                    let step = remaining.min(100);
                    thread::sleep(Duration::from_millis(u64::from(step)));
                    remaining -= step;
                }
                count += 1;
                generation == state.load(Ordering::Relaxed)
            },
        )
        .map_err(|e| e.to_string())?;
        if count <= 1 || disconnected {
            return Ok(());
        }
    }
    Ok(())
}
