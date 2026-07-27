use std::fmt;

use async_trait::async_trait;

use crate::AppError;

/// API-key supplier resolved on demand.
///
/// Resolution happens at first use rather than at construction so daemon
/// startup never blocks on secret storage that is not ready yet.
#[async_trait]
pub trait ApiKeyProvider: Send + Sync + fmt::Debug {
    async fn api_key(&self) -> Result<String, AppError>;
}

/// Provider for keys that are already known, such as literal config values.
#[derive(Clone)]
pub struct StaticApiKey(String);

impl StaticApiKey {
    #[must_use]
    pub fn new(key: String) -> Self {
        Self(key)
    }
}

impl fmt::Debug for StaticApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StaticApiKey(<redacted>)")
    }
}

#[async_trait]
impl ApiKeyProvider for StaticApiKey {
    async fn api_key(&self) -> Result<String, AppError> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_provider_should_return_the_configured_key() {
        let provider = StaticApiKey::new("test-key".into());
        let key = provider.api_key().await.expect("should succeed");
        assert_eq!(key, "test-key");
    }

    #[test]
    fn static_provider_debug_should_not_leak_the_key() {
        let rendered = format!("{:?}", StaticApiKey::new("super-secret".into()));
        assert!(!rendered.contains("super-secret"));
    }
}
