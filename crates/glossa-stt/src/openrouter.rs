use std::sync::Arc;

use glossa_app::ports::{ApiKeyProvider, SttClient};
use glossa_core::ProviderConfig;

use crate::client::build_http_client;

#[must_use]
pub fn build_openrouter_client(
    config: &ProviderConfig,
    api_key: Arc<dyn ApiKeyProvider>,
) -> Arc<dyn SttClient> {
    build_http_client(config, api_key, "openrouter")
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use glossa_app::ports::StaticApiKey;
    use glossa_core::{AppConfig, CapturedAudio, ProviderKind, SessionId};

    use super::*;

    #[tokio::test]
    async fn openrouter_should_upload_multipart_and_decode_text_with_usage() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.expect("read request");
                assert!(count > 0, "request ended before its body arrived");
                request.extend_from_slice(&buffer[..count]);
                let text = String::from_utf8_lossy(&request);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("content length"))
                        })
                        .expect("content-length header");
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let body = r#"{"text":"Привет, мир!","usage":{"seconds":1.0,"cost":0.000008}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("respond");
            String::from_utf8(request).expect("request is utf8")
        });

        let session_id = SessionId::new();
        let path = std::env::temp_dir().join(format!("glossa-openrouter-{session_id}.wav"));
        tokio::fs::write(&path, b"RIFF-test-audio")
            .await
            .expect("write audio");
        let audio = CapturedAudio {
            session_id,
            path: path.to_str().expect("utf8 path").into(),
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        };
        let mut config = AppConfig::default();
        config.provider.kind = ProviderKind::OpenRouter;
        config.provider.base_url = Some(format!("http://{address}/api/v1/"));
        config.provider.model = "openai/whisper-large-v3".into();
        let client = crate::build_client(&config, Arc::new(StaticApiKey::new("test-key".into())));
        assert_eq!(client.provider_name(), "openrouter");
        let result = client.transcribe(&audio).await;
        tokio::fs::remove_file(path).await.expect("remove audio");
        assert_eq!(result.expect("transcribe"), "Привет, мир!");

        let request = server.await.expect("server task");
        assert!(request.starts_with("POST /api/v1/audio/transcriptions HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key\r\n"));
        assert!(request.contains("multipart/form-data; boundary="));
        assert!(request.contains("name=\"model\"\r\n\r\nopenai/whisper-large-v3"));
        assert!(request.contains("name=\"file\"; filename=\"glossa-openrouter-"));
        assert!(request.contains("Content-Type: audio/wav"));
        assert!(request.contains("RIFF-test-audio"));
    }
}
