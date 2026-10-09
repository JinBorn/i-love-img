use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Cancelled")]
    Cancelled,
    #[error("Unsupported image: {0}")]
    Unsupported(String),
    #[error("Invalid input or settings: {0}")]
    Invalid(String),
    #[error("Estimated memory {estimated} bytes exceeds the {limit}-byte budget")]
    MemoryBudget { estimated: u64, limit: u64 },
    #[error("Input file changed or is unavailable: {0}")]
    SourceChanged(PathBuf),
    #[error("Image processing failed: {0}")]
    Image(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

impl From<image::ImageError> for CoreError {
    fn from(value: image::ImageError) -> Self {
        Self::Image(value.to_string())
    }
}

pub fn check_cancel(cancel: &std::sync::atomic::AtomicBool) -> Result<()> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(CoreError::Cancelled)
    } else {
        Ok(())
    }
}
