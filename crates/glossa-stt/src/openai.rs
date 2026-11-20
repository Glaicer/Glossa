use std::sync::Arc;

use glossa_app::ports::{ApiKeyProvider, SttClient};
use glossa_core::ProviderConfig;

use crate::client::build_http_client;

#[must_use]
pub fn build_openai_client(
    config: &ProviderConfig,
    dictionary_terms: &[String],
    api_key: Arc<dyn ApiKeyProvider>,
) -> Arc<dyn SttClient> {
    build_http_client(config, dictionary_terms, api_key, "openai")
}
