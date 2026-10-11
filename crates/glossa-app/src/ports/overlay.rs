use async_trait::async_trait;

use crate::AppError;

/// On-screen overlay indicator shown while recording.
#[async_trait]
pub trait OverlayPort: Send + Sync {
    async fn show_recording(&self) -> Result<(), AppError>;
    async fn hide(&self) -> Result<(), AppError>;
}

/// No-op overlay implementation used when the overlay is disabled or unavailable.
#[derive(Debug, Default)]
pub struct NullOverlayPort;

#[async_trait]
impl OverlayPort for NullOverlayPort {
    async fn show_recording(&self) -> Result<(), AppError> {
        Ok(())
    }

    async fn hide(&self) -> Result<(), AppError> {
        Ok(())
    }
}
