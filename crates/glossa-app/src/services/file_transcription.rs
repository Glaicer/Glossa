use std::{path::Path, sync::Arc};

use camino::Utf8PathBuf;
use glossa_core::{AudioFormat, CapturedAudio, SessionId};

use crate::{
    ports::{SttClient, TextEnhancer},
    AppError,
};

pub const MAX_TRANSCRIPTION_FILE_BYTES: u64 = 20_000_000;

pub fn validate_transcription_file(path: &Path) -> Result<Utf8PathBuf, AppError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| AppError::io("failed to open audio file", error))?;
    if !metadata.is_file() {
        return Err(AppError::message("Choose an audio file, not a directory"));
    }
    if metadata.len() > MAX_TRANSCRIPTION_FILE_BYTES {
        return Err(AppError::message(
            "File is too large. Maximum size is 20 MB",
        ));
    }
    if metadata.len() == 0 {
        return Err(AppError::message("The audio file is empty"));
    }
    let path = Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| AppError::message("The file path must be valid UTF-8"))?;
    if path
        .extension()
        .and_then(|extension| AudioFormat::from_extension(&extension.to_ascii_lowercase()))
        .is_none()
    {
        return Err(AppError::message(
            "Choose a WAV, MP3, M4A, FLAC, OGG, or AAC audio file",
        ));
    }
    Ok(path)
}

pub struct FileTranscriptionService {
    stt_client: Arc<dyn SttClient>,
    text_enhancer: Arc<dyn TextEnhancer>,
    enhance: bool,
}

impl FileTranscriptionService {
    pub fn new(
        stt_client: Arc<dyn SttClient>,
        text_enhancer: Arc<dyn TextEnhancer>,
        enhance: bool,
    ) -> Self {
        Self {
            stt_client,
            text_enhancer,
            enhance,
        }
    }

    pub async fn transcribe(&self, path: &Path) -> Result<String, AppError> {
        let path = validate_transcription_file(path)?;
        let audio = CapturedAudio {
            session_id: SessionId::new(),
            path,
            duration_ms: 0,
            sample_rate_hz: 0,
            channels: 0,
        };
        let text = self.stt_client.transcribe(&audio).await?;
        let text = text.trim().replace("\r\n", "\n");
        if text.is_empty() {
            return Err(AppError::message("Transcription returned empty text"));
        }
        if !self.enhance {
            return Ok(text);
        }
        let enhanced = self
            .text_enhancer
            .enhance(&text)
            .await
            .map_err(|error| AppError::message(format!("LLM enhancement failed: {error}")))?;
        let enhanced = enhanced.trim().replace("\r\n", "\n");
        if enhanced.is_empty() {
            return Err(AppError::message("LLM enhancement returned empty text"));
        }
        Ok(enhanced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::{fs::File, path::PathBuf, sync::Mutex};

    struct AudioFile(PathBuf);

    impl AudioFile {
        fn new(size: u64, extension: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "glossa-file-{}.{}",
                SessionId::new(),
                extension
            ));
            File::create(&path)
                .expect("create file")
                .set_len(size)
                .expect("size file");
            Self(path)
        }
    }

    impl Drop for AudioFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).expect("remove test file");
        }
    }

    struct Probe {
        calls: Mutex<Vec<String>>,
        transcript: Result<String, String>,
        enhanced: Result<String, String>,
    }

    #[async_trait]
    impl SttClient for Probe {
        fn provider_name(&self) -> &'static str {
            "test"
        }
        async fn transcribe(&self, audio: &CapturedAudio) -> Result<String, AppError> {
            assert!(audio.path.exists());
            self.calls.lock().expect("calls").push("stt".into());
            self.transcript.clone().map_err(AppError::message)
        }
    }

    #[async_trait]
    impl TextEnhancer for Probe {
        fn name(&self) -> &'static str {
            "test"
        }
        async fn enhance(&self, text: &str) -> Result<String, AppError> {
            self.calls.lock().expect("calls").push(text.into());
            self.enhanced.clone().map_err(AppError::message)
        }
    }

    fn service(
        enhance: bool,
        transcript: Result<&str, &str>,
        enhanced: Result<&str, &str>,
    ) -> (FileTranscriptionService, Arc<Probe>) {
        let probe = Arc::new(Probe {
            calls: Mutex::new(Vec::new()),
            transcript: transcript.map(str::to_owned).map_err(str::to_owned),
            enhanced: enhanced.map(str::to_owned).map_err(str::to_owned),
        });
        (
            FileTranscriptionService::new(probe.clone(), probe.clone(), enhance),
            probe,
        )
    }

    #[test]
    fn file_size_limit_includes_exactly_twenty_mb() {
        let file = AudioFile::new(MAX_TRANSCRIPTION_FILE_BYTES, "MP3");
        assert!(validate_transcription_file(&file.0).is_ok());
        let oversized = AudioFile::new(MAX_TRANSCRIPTION_FILE_BYTES + 1, "wav");
        assert!(validate_transcription_file(&oversized.0)
            .expect_err("too large")
            .to_string()
            .contains("20 MB"));
    }

    #[test]
    fn rejects_empty_unsupported_missing_and_directory_inputs() {
        for (size, extension) in [(0, "wav"), (1, "txt")] {
            let file = AudioFile::new(size, extension);
            assert!(validate_transcription_file(&file.0).is_err());
        }
        assert!(validate_transcription_file(&std::env::temp_dir()).is_err());
        let missing = std::env::temp_dir().join(format!("{}.wav", SessionId::new()));
        assert!(validate_transcription_file(&missing).is_err());
    }

    #[tokio::test]
    async fn transcribes_then_enhances_without_modifying_the_source() {
        let file = AudioFile::new(16, "wav");
        let (service, probe) = service(true, Ok(" hello\r\nworld "), Ok(" Hello, world! "));
        assert_eq!(
            service.transcribe(&file.0).await.expect("transcribe"),
            "Hello, world!"
        );
        assert_eq!(*probe.calls.lock().expect("calls"), ["stt", "hello\nworld"]);
        assert_eq!(
            std::fs::metadata(&file.0).expect("source remains").len(),
            16
        );
    }

    #[tokio::test]
    async fn skips_disabled_enhancer() {
        let file = AudioFile::new(1, "mp3");
        let (service, probe) = service(false, Ok(" hello "), Err("must not run"));
        assert_eq!(
            service.transcribe(&file.0).await.expect("transcribe"),
            "hello"
        );
        assert_eq!(*probe.calls.lock().expect("calls"), ["stt"]);
    }

    #[tokio::test]
    async fn rechecks_size_before_sending_to_stt() {
        let file = AudioFile::new(1, "wav");
        validate_transcription_file(&file.0).expect("initial selection");
        File::options()
            .write(true)
            .open(&file.0)
            .expect("open")
            .set_len(MAX_TRANSCRIPTION_FILE_BYTES + 1)
            .expect("grow");
        let (service, probe) = service(true, Ok("hello"), Ok("Hello"));
        assert!(service.transcribe(&file.0).await.is_err());
        assert!(probe.calls.lock().expect("calls").is_empty());
    }

    #[tokio::test]
    async fn reports_stt_and_enhancement_failures_and_empty_results() {
        let file = AudioFile::new(1, "wav");
        for (transcript, enhanced, message) in [
            (Err("network error"), Ok("Hello"), "network error"),
            (Ok("  "), Ok("Hello"), "empty text"),
            (Ok("hello"), Err("unavailable"), "LLM enhancement failed"),
            (Ok("hello"), Ok(" \r\n "), "empty text"),
        ] {
            let (service, _) = service(true, transcript, enhanced);
            assert!(service
                .transcribe(&file.0)
                .await
                .expect_err("failure")
                .to_string()
                .contains(message));
        }
    }
}
