use async_trait::async_trait;

use crate::AppError;

/// A single clipboard payload captured before Glossa overwrites the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

impl ClipboardSnapshot {
    #[must_use]
    pub fn new(mime_type: String, bytes: Vec<u8>) -> Self {
        Self { mime_type, bytes }
    }
}

/// Clipboard writer used for the final transcription text.
#[async_trait]
pub trait ClipboardWriter: Send + Sync {
    async fn snapshot(&self) -> Result<Option<ClipboardSnapshot>, AppError> {
        Ok(None)
    }

    async fn set_text(&self, text: &str) -> Result<(), AppError>;

    async fn restore(&self, _snapshot: ClipboardSnapshot) -> Result<(), AppError> {
        Ok(())
    }
}
