use clap::{Args, Parser, Subcommand, ValueEnum};
use local_image_core::batch::{BatchConfig, BatchEvent, BatchReport, TaskStatus, start_batch};
use local_image_core::import::{ImportOptions, import_paths};
use local_image_core::{Compression, Format, MIB, ProcessOptions, ResizeMode, ResizeSpec};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "lit",
    version,
    about = "Offline batch image tools. Originals are never overwritten."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compress, resize or convert files using the same engine as the desktop app.
    Process(ProcessArgs),
    /// Inspect image headers without decoding all image pixels.
    Inspect {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
    /// Generate reproducible synthetic images for local tests and measurements.
    Fixtures(FixtureArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Keep,
    #[value(alias = "jpg")]
    Jpeg,
    Png,
    Webp,
    Gif,
}

#[derive(Args)]
struct ProcessArgs {
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(short, long)]
    recursive: bool,
    #[arg(long, value_enum, default_value = "keep")]
    format: OutputFormat,
    #[arg(long)]
    lossless: bool,
    #[arg(long, default_value_t = 85, value_parser = clap::value_parser!(u8).range(1..=100))]
    jpeg_quality: u8,
    #[arg(long, default_value_t = 80, value_parser = clap::value_parser!(u8).range(1..=100))]
    webp_quality: u8,
    #[arg(long, requires = "height", conflicts_with_all = ["long_edge", "percent"])]
    width: Option<u32>,
    #[arg(long, requires = "width", conflicts_with_all = ["long_edge", "percent"])]
    height: Option<u32>,
    #[arg(long, conflicts_with_all = ["width", "height", "percent"])]
    long_edge: Option<u32>,
    #[arg(long, conflicts_with_all = ["width", "height", "long_edge"])]
    percent: Option<f64>,
    #[arg(long)]
    allow_upscale: bool,
    #[arg(long)]
    keep_metadata: bool,
    #[arg(long, default_value = "ffffff", value_parser = parse_matte)]
    matte: [u8; 3],
    #[arg(long)]
    gif_first_frame: bool,
    #[arg(long, default_value_t = 4)]
    workers: usize,
    /// Shared image-processing reservation; leave memory for the GUI/OS.
    #[arg(long, default_value_t = 1536)]
    memory_mib: u64,
    /// Write a new JSON report. An existing report is not overwritten.
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(short, long)]
    quiet: bool,
    /// Diagnostic: request normal cancellation after this many milliseconds.
    #[arg(long, hide = true)]
    cancel_after_ms: Option<u64>,
}

#[derive(Args)]
struct FixtureArgs {
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, default_value_t = 32)]
    count: usize,
    #[arg(long, default_value_t = 960)]
    width: u32,
    #[arg(long, default_value_t = 640)]
    height: u32,
    /// Include a 6000x4000 image at every Nth position; zero disables it.
    #[arg(long, default_value_t = 0)]
    large_every: usize,
    #[arg(long, default_value_t = 48)]
    gif_frames: usize,
}

fn main() {
    let code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32, Box<dyn std::error::Error>> {
    match cli.command {
        Command::Process(args) => process(args),
        Command::Inspect { inputs } => {
            let mut failed = false;
            let values: Vec<_> = inputs
                .iter()
                .map(|path| match local_image_core::probe(path) {
                    Ok(info) => json!({"path": path, "info": info}),
                    Err(error) => {
                        failed = true;
                        json!({"path": path, "error": error.to_string()})
                    }
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&values)?);
            Ok(if failed { 2 } else { 0 })
        }
        Command::Fixtures(args) => {
            fixtures(args)?;
            Ok(0)
        }
    }
}

fn parse_matte(value: &str) -> Result<[u8; 3], String> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.is_ascii() {
        return Err("use six hexadecimal digits, for example ffffff".into());
    }
    let mut rgb = [0; 3];
    for (index, component) in rgb.iter_mut().enumerate() {
        *component = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid hexadecimal color")?;
    }
    Ok(rgb)
}

fn process(args: ProcessArgs) -> Result<i32, Box<dyn std::error::Error>> {
    let mode = if let (Some(width), Some(height)) = (args.width, args.height) {
        ResizeMode::Bounds { width, height }
    } else if let Some(edge) = args.long_edge {
        ResizeMode::LongestEdge(edge)
    } else if let Some(percent) = args.percent {
        ResizeMode::Percent(percent)
    } else {
        ResizeMode::Keep
    };
    let memory = args
        .memory_mib
        .checked_mul(MIB)
        .ok_or("memory budget overflow")?;
    let options = ProcessOptions {
        format: match args.format {
            OutputFormat::Keep => None,
            OutputFormat::Jpeg => Some(Format::Jpeg),
            OutputFormat::Png => Some(Format::Png),
            OutputFormat::Webp => Some(Format::WebP),
            OutputFormat::Gif => Some(Format::Gif),
        },
        resize: ResizeSpec {
            mode,
            allow_upscale: args.allow_upscale,
        },
        compression: if args.lossless {
            Compression::Lossless
        } else {
            Compression::Auto
        },
        jpeg_quality: args.jpeg_quality,
        webp_quality: args.webp_quality,
        strip_metadata: !args.keep_metadata,
        matte: args.matte,
        gif_first_frame: args.gif_first_frame,
        memory_limit_bytes: memory,
    };
    options.validate()?;
    let import = import_paths(
        &args.inputs,
        &ImportOptions {
            recursive: args.recursive,
            output_directory: args.output.clone(),
        },
    );
    if import.items.is_empty() {
        for issue in &import.skipped {
            eprintln!("Import: {}: {:?}", issue.path.display(), issue.reason);
        }
        return Err("no readable input files were imported".into());
    }
    let imported = import.items.len();
    let skipped_imports: Vec<Value> = import
        .skipped
        .iter()
        .map(|issue| json!({"path": issue.path, "reason": format!("{:?}", issue.reason)}))
        .collect();
    let handle = start_batch(
        import.items,
        options,
        BatchConfig {
            output_directory: args.output,
            workers: args.workers,
            memory_budget_bytes: memory,
        },
    )?;
    let cancellation = handle.cancellation_token();
    let keyboard_cancel = cancellation.clone();
    ctrlc::set_handler(move || keyboard_cancel.cancel())?;
    if let Some(delay) = args.cancel_after_ms {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            cancellation.cancel();
        });
    }
    let mut completed = 0;
    let mut messages = 0;
    while let Ok(event) = handle.events.recv() {
        match event {
            BatchEvent::ItemFinished(result) => {
                completed += 1;
                if !args.quiet && (completed % 50 == 0 || completed == imported) {
                    eprintln!("Processed {completed}/{imported}");
                }
                match &result.status {
                    TaskStatus::Failed { reason } | TaskStatus::Skipped { reason }
                        if messages < 20 =>
                    {
                        eprintln!("{}: {reason}", result.item.input.path.display());
                        messages += 1;
                    }
                    _ => {}
                }
            }
            BatchEvent::Finished(_) => break,
            _ => {}
        }
    }
    let report = handle.join().map_err(|_| "batch coordinator panicked")?;
    let value = report_json(&report, skipped_imports);
    println!("{}", serde_json::to_string_pretty(&value["summary"])?);
    if let Some(path) = args.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.write_all(b"\n")?;
    }
    let failure = report
        .results
        .iter()
        .any(|result| matches!(result.status, TaskStatus::Failed { .. }));
    let skipped = report
        .results
        .iter()
        .any(|result| matches!(result.status, TaskStatus::Skipped { .. }));
    Ok(if report.cancelled {
        130
    } else if failure || skipped {
        2
    } else {
        0
    })
}

fn report_json(report: &BatchReport, import_skips: Vec<Value>) -> Value {
    let (mut success, mut failed, mut skipped, mut cancelled) = (0_u64, 0_u64, 0_u64, 0_u64);
    let (mut input_bytes, mut output_bytes) = (0_u64, 0_u64);
    let results: Vec<Value> = report
        .results
        .iter()
        .map(|result| {
            let mut value = json!({"id": result.item.id, "original": result.item.original.path,
            "input": result.item.input.path, "elapsed_ms": result.elapsed_ms});
            match &result.status {
                TaskStatus::Success {
                    output,
                    report,
                    change,
                } => {
                    success += 1;
                    input_bytes += report.input_bytes;
                    output_bytes += report.output_bytes;
                    value["status"] = json!("success");
                    value["output"] = json!(output.path);
                    value["change"] = json!(format!("{change:?}"));
                    value["processing"] = json!(report);
                }
                TaskStatus::Skipped { reason } => {
                    skipped += 1;
                    value["status"] = json!("skipped");
                    value["reason"] = json!(reason);
                }
                TaskStatus::Failed { reason } => {
                    failed += 1;
                    value["status"] = json!("failed");
                    value["reason"] = json!(reason);
                }
                TaskStatus::Cancelled => {
                    cancelled += 1;
                    value["status"] = json!("cancelled");
                }
            }
            value
        })
        .collect();
    json!({"application": "Local Image Tools", "version": env!("CARGO_PKG_VERSION"),
        "summary": {"total": report.results.len(), "success": success, "skipped": skipped,
            "failed": failed, "cancelled": cancelled, "elapsed_ms": report.elapsed_ms,
            "images_per_second": if report.elapsed_ms > 0 { success as f64 * 1000.0 / report.elapsed_ms as f64 } else { 0.0 },
            "input_bytes": input_bytes, "output_bytes": output_bytes},
        "options": report.options, "memory_reservation_bytes": report.config.memory_budget_bytes,
        "workers": report.config.workers, "results": results, "import_skips": import_skips})
}

fn fixtures(args: FixtureArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.count == 0 || args.width == 0 || args.height == 0 || args.gif_frames == 0 {
        return Err("fixture counts and dimensions must be positive".into());
    }
    if u64::from(args.width) * u64::from(args.height) > 24_000_000 {
        return Err("fixture dimensions are limited to 24 megapixels".into());
    }
    if args.output.exists() && fs::read_dir(&args.output)?.next().is_some() {
        return Err("fixture directory must be new or empty".into());
    }
    fs::create_dir_all(&args.output)?;
    let mut bases = std::collections::HashMap::<(usize, bool), PathBuf>::new();
    for index in 0..args.count {
        let large = args.large_every > 0 && index % args.large_every == 0;
        let kind = if large {
            index / args.large_every % 3
        } else {
            index % 4
        };
        let extension = ["jpg", "png", "webp", "gif"][kind];
        let group = args.output.join(format!("set-{:03}", index / 100));
        fs::create_dir_all(&group)?;
        let path = group.join(format!("image-{index:04}.{extension}"));
        if let Some(base) = bases.get(&(kind, large)) {
            fs::copy(base, &path)?;
        } else {
            if kind == 3 {
                write_gif_fixture(&path, args.gif_frames)?;
            } else {
                let (width, height) = if large {
                    (6000, 4000)
                } else {
                    (args.width, args.height)
                };
                let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
                    let noise =
                        ((x.wrapping_mul(1664525) ^ y.wrapping_mul(1013904223)) >> 22) as u8;
                    let tile = (x / 64 + y / 64) % 2;
                    let r = ((x as u64 * 200 / u64::from(width)) as u8).saturating_add(noise / 8);
                    let g = ((y as u64 * 210 / u64::from(height)) as u8).saturating_add(noise / 8);
                    let b = if tile == 0 { 185 } else { 45 };
                    let a = if kind == 1 && (x / 96) % 3 == 0 {
                        ((y * 255) / height) as u8
                    } else {
                        255
                    };
                    image::Rgba([r, g, b, a])
                });
                let image = image::DynamicImage::ImageRgba8(pixels);
                let mut file = BufWriter::new(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?,
                );
                match kind {
                    0 => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 92)
                        .encode_image(&image.to_rgb8())?,
                    1 => image.write_to(&mut file, image::ImageFormat::Png)?,
                    2 => image.write_to(&mut file, image::ImageFormat::WebP)?,
                    _ => unreachable!(),
                }
                file.flush()?;
            }
            bases.insert((kind, large), path);
        }
    }
    println!(
        "{}",
        json!({"directory": args.output,"images":args.count,"large_every":args.large_every,
        "large_dimensions":[6000,4000],"gif_frames":args.gif_frames,
        "note":"Synthetic repeated samples for reproducible resource measurements; not a photographic quality benchmark."})
    );
    Ok(())
}

fn write_gif_fixture(path: &Path, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = BufWriter::new(File::create(path)?);
    let palette = [0, 0, 0, 20, 160, 130, 240, 180, 60, 50, 100, 210];
    let mut encoder = gif::Encoder::new(&mut file, 160, 120, &palette)?;
    encoder.set_repeat(gif::Repeat::Finite(3))?;
    for index in 0..count {
        let mut data = vec![0_u8; 40 * 40];
        for y in 0..40 {
            for x in 0..40 {
                if (x as i32 - 20).pow(2) + (y as i32 - 20).pow(2) < 350 {
                    data[y * 40 + x] = (index % 3 + 1) as u8;
                }
            }
        }
        let frame = gif::Frame {
            left: ((index * 7) % 120) as u16,
            top: ((index * 3) % 80) as u16,
            width: 40,
            height: 40,
            delay: 4,
            transparent: Some(0),
            dispose: match index % 3 {
                0 => gif::DisposalMethod::Keep,
                1 => gif::DisposalMethod::Background,
                _ => gif::DisposalMethod::Previous,
            },
            buffer: Cow::Borrowed(&data),
            ..Default::default()
        };
        encoder.write_frame(&frame)?;
    }
    drop(encoder);
    file.flush()?;
    Ok(())
}
