use async_trait::async_trait;

use crate::AppError;
use glossa_core::{AudioFormat, CapturedAudio};

/// Encodes a processed WAV recording into the configured upload format.
#[async_trait]
pub trait AudioEncoder: Send + Sync {
    async fn encode(
        &self,
        input: &CapturedAudio,
        format: AudioFormat,
    ) -> Result<CapturedAudio, AppError>;
}
