//! Offline image processing with explicit format, fidelity and memory contracts.

pub mod batch;
pub mod error;
pub mod gif_codec;
pub mod import;
pub mod model;
pub mod pipeline;
pub mod probe;

pub(crate) mod color;
pub(crate) mod metadata;
pub(crate) mod pixels;
pub(crate) mod static_codecs;

pub use error::{CoreError, Result};
pub use model::{
    Compression, Format, ImageInfo, MIB, ProcessOptions, ProcessReport, ResizeMode, ResizeSpec,
};
pub use pipeline::{estimate_memory, process_to_path};
pub use probe::probe;
