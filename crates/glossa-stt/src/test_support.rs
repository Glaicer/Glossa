use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use glossa_app::{ports::StaticApiKey, AppError};
use glossa_core::{AppConfig, CapturedAudio, SessionId};

pub(crate) struct Exchange {
    pub(crate) session_id: SessionId,
    pub(crate) provider_name: &'static str,
    pub(crate) result: Result<String, AppError>,
    pub(crate) request: String,
}

impl Exchange {
    pub(crate) fn body(&self) -> &str {
        let header_end = self.request.find("\r\n\r\n").expect("request has headers");
        &self.request[header_end + 4..]
    }
}

/// Sends one transcription through `build_client` to a local stub and returns the raw request.
/// `config.provider.base_url` is replaced with the stub address plus `base_path`.
pub(crate) async fn transcribe_once(
    mut config: AppConfig,
    base_path: &str,
    extension: &str,
) -> Exchange {
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
    let path = std::env::temp_dir().join(format!("glossa-stt-{session_id}.{extension}"));
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
    config.provider.base_url = Some(format!("http://{address}{base_path}"));
    let client = crate::build_client(&config, Arc::new(StaticApiKey::new("test-key".into())));
    let provider_name = client.provider_name();
    let result = client.transcribe(&audio).await;
    tokio::fs::remove_file(path).await.expect("remove audio");
    let request = server.await.expect("server task");
    Exchange {
        session_id,
        provider_name,
        result,
        request,
    }
}
