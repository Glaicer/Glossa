use serde::{Deserialize, Serialize};

use super::SecretSource;
use crate::{CoreError, ProviderKind};

/// Provider settings for speech-to-text integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub model: String,
    pub api_key: SecretSource,
}

impl ProviderConfig {
    pub(crate) fn validate(&self) -> Result<(), CoreError> {
        if self.model.trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "provider.model must not be empty".into(),
            ));
        }
        if self.kind == ProviderKind::OpenAiCompatible
            && self.base_url.as_deref().is_none_or(str::is_empty)
        {
            return Err(CoreError::InvalidConfig(
                "provider.base_url is required for openai-compatible".into(),
            ));
        }
        if let Some(base_url) = &self.base_url {
            if base_url.trim().is_empty() {
                return Err(CoreError::InvalidConfig(
                    "provider.base_url must not be empty when provided".into(),
                ));
            }
        }
        if self.kind != ProviderKind::OpenAiCompatible
            && matches!(self.api_key, SecretSource::Empty)
        {
            return Err(CoreError::InvalidConfig(
                "provider.api_key must not be empty unless kind is openai-compatible".into(),
            ));
        }
        let _ = self.api_key.describe();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_should_allow_default_url_but_require_api_key() {
        let mut config = ProviderConfig {
            kind: ProviderKind::OpenRouter,
            base_url: None,
            model: "openai/whisper-large-v3".into(),
            api_key: SecretSource::Env("OPENROUTER_API_KEY".into()),
        };
        assert!(config.validate().is_ok());
        config.api_key = SecretSource::Empty;
        assert!(config.validate().is_err());
    }
}
