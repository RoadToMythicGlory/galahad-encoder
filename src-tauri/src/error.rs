//! Shared error type for Galahad Encoder.

use serde::Serialize;

pub type Result<T> = std::result::Result<T, EncoderError>;

#[derive(Debug, thiserror::Error)]
pub enum EncoderError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("capability error: {0}")]
    Capability(String),

    #[error("capture error: {0}")]
    Capture(String),

    #[error("audio error: {0}")]
    Audio(String),

    #[error("encoder error: {0}")]
    Encoder(String),

    #[error("pipeline error: {0}")]
    Pipeline(String),

    #[error("control channel error: {0}")]
    Control(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Tauri commands must return errors that serialize to the frontend. We flatten
/// to a string payload with a stable `kind` tag for diagnostics.
impl Serialize for EncoderError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let kind = match self {
            EncoderError::Config(_) => "config",
            EncoderError::Capability(_) => "capability",
            EncoderError::Capture(_) => "capture",
            EncoderError::Audio(_) => "audio",
            EncoderError::Encoder(_) => "encoder",
            EncoderError::Pipeline(_) => "pipeline",
            EncoderError::Control(_) => "control",
            EncoderError::Io(_) => "io",
            EncoderError::Serde(_) => "serde",
        };
        let mut s = serializer.serialize_struct("EncoderError", 2)?;
        s.serialize_field("kind", kind)?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}
