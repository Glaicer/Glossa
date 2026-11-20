use std::sync::Arc;

use glossa_app::ports::{ApiKeyProvider, SttClient};
use glossa_core::ProviderConfig;

use crate::client::build_http_client;

#[must_use]
pub fn build_openrouter_client(
    config: &ProviderConfig,
    dictionary_terms: &[String],
    api_key: Arc<dyn ApiKeyProvider>,
) -> Arc<dyn SttClient> {
    build_http_client(config, dictionary_terms, api_key, "openrouter")
}

#[cfg(test)]
mod tests {
    use glossa_core::{AppConfig, AudioFormat, ProviderKind};

    use crate::test_support::transcribe_once;

    #[tokio::test]
    async fn openrouter_should_upload_multipart_and_decode_text_with_usage() {
        check_upload(AudioFormat::Wav, "audio/wav").await;
    }

    #[tokio::test]
    async fn compressed_formats_should_upload_with_correct_filename_and_mime() {
        for (format, mime) in [
            (AudioFormat::Mp3, "audio/mpeg"),
            (AudioFormat::M4a, "audio/mp4"),
            (AudioFormat::Flac, "audio/flac"),
            (AudioFormat::Ogg, "audio/ogg"),
            (AudioFormat::Aac, "audio/aac"),
        ] {
            check_upload(format, mime).await;
        }
    }

    #[tokio::test]
    async fn uppercase_file_extension_should_keep_audio_mime() {
        check_upload_with_extension("MP3", "audio/mpeg").await;
    }

    async fn check_upload(format: AudioFormat, mime: &str) {
        check_upload_with_extension(format.extension(), mime).await;
    }

    async fn check_upload_with_extension(extension: &str, mime: &str) {
        let mut config = AppConfig::default();
        config.provider.kind = ProviderKind::OpenRouter;
        config.provider.model = "openai/whisper-large-v3".into();
        let exchange = transcribe_once(config, "/api/v1/", extension).await;
        assert_eq!(exchange.provider_name, "openrouter");
        assert_eq!(exchange.result.expect("transcribe"), "Привет, мир!");

        let request = exchange.request;
        let session_id = exchange.session_id;
        assert!(request.starts_with("POST /api/v1/audio/transcriptions HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key\r\n"));
        assert!(request.contains("multipart/form-data; boundary="));
        assert!(request.contains("name=\"model\"\r\n\r\nopenai/whisper-large-v3"));
        assert!(request.contains("name=\"file\"; filename=\"glossa-stt-"));
        assert!(request.contains(&format!("filename=\"glossa-stt-{session_id}.{extension}\"")));
        assert!(request.contains(&format!("Content-Type: {mime}")));
        assert!(request.contains("RIFF-test-audio"));
        assert!(!request.contains("name=\"prompt\""));
        assert!(!request.contains("keywords"));
        assert!(!request.contains("keyterms"));
    }
}
