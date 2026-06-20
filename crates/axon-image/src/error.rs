use thiserror::Error;

#[derive(Error, Debug)]
pub enum ImageProcessorError {
    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),

    #[error("Font load error: {0}")]
    FontError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Exif error: {0}")]
    ExifError(String),

    #[error("Composite error: {0}")]
    CompositeError(String),

    #[error("Batch processing error on file '{file}': {source}")]
    BatchError {
        file: String,
        source: Box<ImageProcessorError>,
    },
}

pub type Result<T> = std::result::Result<T, ImageProcessorError>;
