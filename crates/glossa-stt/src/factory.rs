use std::sync::Arc;

use glossa_app::ports::{ApiKeyProvider, SttClient, TextEnhancer};
use glossa_core::{AppConfig, ProviderKind};

use crate::{
    compatible::build_compatible_client, groq::build_groq_client, llm_enhancer::HttpTextEnhancer,
    openai::build_openai_client, openrouter::build_openrouter_client,
};

pub fn build_client(config: &AppConfig, api_key: Arc<dyn ApiKeyProvider>) -> Arc<dyn SttClient> {
    match config.provider.kind {
        ProviderKind::Groq => build_groq_client(&config.provider, api_key),
        ProviderKind::OpenAi => build_openai_client(&config.provider, api_key),
        ProviderKind::OpenRouter => build_openrouter_client(&config.provider, api_key),
        ProviderKind::OpenAiCompatible => build_compatible_client(&config.provider, api_key),
    }
}

/// Builds the text enhancer based on `[LLM]` configuration.
pub fn build_text_enhancer(
    config: &AppConfig,
    api_key: Arc<dyn ApiKeyProvider>,
) -> Arc<dyn TextEnhancer> {
    if !config.llm.enabled {
        return Arc::new(glossa_app::ports::NoopTextEnhancer);
    }
    Arc::new(HttpTextEnhancer::new(
        config.llm.base_url.clone(),
        config.llm.model.clone(),
        api_key,
    ))
}
