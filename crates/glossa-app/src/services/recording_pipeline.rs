use std::{borrow::Cow, sync::Arc, time::Duration};

use tokio::{sync::mpsc, task, time::sleep};
use tracing::{info, warn};

use glossa_core::{AppConfig, CapturedAudio, SessionId};

use crate::{
    ports::{ClipboardWriter, PasteBackend, SilenceTrimmer, SttClient, TempStore, TextEnhancer},
    AppError,
};

const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(250);

/// Messages emitted back to the actor from background tasks.
#[derive(Debug)]
pub enum InternalEvent {
    ProcessingReady {
        session_id: SessionId,
        text: String,
    },
    ProcessingFinished {
        session_id: SessionId,
        outcome: CycleOutcome,
    },
    PasteFinished {
        session_id: SessionId,
        outcome: CycleOutcome,
    },
    IdleStreamStatusRefresh,
}

/// Final outcome of a capture/transcription/paste cycle.
#[derive(Debug, Clone)]
pub enum CycleOutcome {
    Completed,
    CompletedWithWarning(String),
    Failed(String),
}

/// Shared dependencies for processing and paste tasks.
#[derive(Clone)]
pub struct PipelineDependencies {
    pub config: Arc<AppConfig>,
    pub trimmer: Arc<dyn SilenceTrimmer>,
    pub stt_client: Arc<dyn SttClient>,
    pub text_enhancer: Arc<dyn TextEnhancer>,
    pub clipboard: Arc<dyn ClipboardWriter>,
    pub paste: Arc<dyn PasteBackend>,
    pub temp_store: Arc<dyn TempStore>,
}

/// Spawns the non-paste processing steps for a captured recording.
pub fn spawn_processing_task(
    tx: mpsc::UnboundedSender<InternalEvent>,
    deps: PipelineDependencies,
    audio: CapturedAudio,
) {
    task::spawn(async move {
        let session_id = audio.session_id;
        let processed_audio = if deps.config.audio.trim_silence {
            match deps.trimmer.trim(&audio).await {
                Ok(trimmed) => trimmed,
                Err(error) => {
                    warn!(%session_id, error = %error, "silence trimming failed; continuing with the original file");
                    audio
                }
            }
        } else {
            audio
        };

        if processed_audio.duration_ms == 0 {
            let _ = deps.temp_store.cleanup_session(session_id).await;
            let _ = tx.send(InternalEvent::ProcessingFinished {
                session_id,
                outcome: CycleOutcome::CompletedWithWarning("recording was empty".into()),
            });
            return;
        }

        if processed_audio.duration_ms < deps.config.audio.min_duration_ms {
            let _ = deps.temp_store.cleanup_session(session_id).await;
            let _ = tx.send(InternalEvent::ProcessingFinished {
                session_id,
                outcome: CycleOutcome::CompletedWithWarning(
                    "recording was shorter than the configured minimum duration".into(),
                ),
            });
            return;
        }

        info!(%session_id, provider = deps.stt_client.provider_name(), "starting transcription");
        match deps.stt_client.transcribe(&processed_audio).await {
            Ok(text) => {
                let normalized = text.trim().replace("\r\n", "\n");
                if normalized.is_empty() {
                    let _ = deps.temp_store.cleanup_session(session_id).await;
                    let _ = tx.send(InternalEvent::ProcessingFinished {
                        session_id,
                        outcome: CycleOutcome::CompletedWithWarning(
                            "transcription returned empty text".into(),
                        ),
                    });
                } else {
                    info!(
                        %session_id,
                        provider = deps.stt_client.provider_name(),
                        text = %normalized,
                        "transcription completed"
                    );
                    if deps.config.llm.enabled {
                        info!(%session_id, enhancer = deps.text_enhancer.name(), "starting text enhancement");
                        match deps.text_enhancer.enhance(&normalized).await {
                            Ok(enhanced) => {
                                let enhanced_normalized = enhanced.trim().replace("\r\n", "\n");
                                if enhanced_normalized.is_empty() {
                                    let _ = deps.temp_store.cleanup_session(session_id).await;
                                    let _ = tx.send(InternalEvent::ProcessingFinished {
                                        session_id,
                                        outcome: CycleOutcome::Failed(
                                            "LLM enhancement returned empty text".into(),
                                        ),
                                    });
                                } else {
                                    info!(%session_id, "text enhancement completed");
                                    let _ = tx.send(InternalEvent::ProcessingReady {
                                        session_id,
                                        text: enhanced_normalized,
                                    });
                                }
                            }
                            Err(error) => {
                                let _ = deps.temp_store.cleanup_session(session_id).await;
                                let _ = tx.send(InternalEvent::ProcessingFinished {
                                    session_id,
                                    outcome: CycleOutcome::Failed(format!(
                                        "LLM enhancement failed: {error}"
                                    )),
                                });
                            }
                        }
                    } else {
                        let _ = tx.send(InternalEvent::ProcessingReady {
                            session_id,
                            text: normalized,
                        });
                    }
                }
            }
            Err(error) => {
                let _ = deps.temp_store.cleanup_session(session_id).await;
                let _ = tx.send(InternalEvent::ProcessingFinished {
                    session_id,
                    outcome: CycleOutcome::Failed(error.to_string()),
                });
            }
        }
    });
}

/// Spawns the final clipboard + paste phase for a transcription result.
pub fn spawn_paste_task(
    tx: mpsc::UnboundedSender<InternalEvent>,
    deps: PipelineDependencies,
    session_id: SessionId,
    text: String,
) {
    task::spawn(async move {
        let result = paste_cycle(&deps, session_id, &text).await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => CycleOutcome::Failed(error.to_string()),
        };

        let _ = deps.temp_store.cleanup_session(session_id).await;
        let _ = tx.send(InternalEvent::PasteFinished {
            session_id,
            outcome,
        });
    });
}

async fn paste_cycle(
    deps: &PipelineDependencies,
    session_id: SessionId,
    text: &str,
) -> Result<CycleOutcome, AppError> {
    let mut warnings = Vec::new();
    let previous_clipboard = match deps.clipboard.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warnings.push(format!(
                "failed to save previous clipboard for session {session_id}: {error}"
            ));
            None
        }
    };

    let clipboard_text = if deps.config.paste.append_space && !text.is_empty() {
        Cow::Owned(format!("{text} "))
    } else {
        Cow::Borrowed(text)
    };

    info!(
        %session_id,
        text_len = clipboard_text.len(),
        "writing transcription to clipboard"
    );
    deps.clipboard.set_text(clipboard_text.as_ref()).await?;
    info!(
        %session_id,
        paste_mode = ?deps.config.paste.mode,
        "attempting paste via configured backend"
    );
    match deps.paste.paste(deps.config.paste.mode).await {
        Ok(()) => {
            if let Some(snapshot) = previous_clipboard {
                // The paste backend only confirms that key events were sent; the target app
                // may consume the clipboard shortly after handling those events.
                sleep(CLIPBOARD_RESTORE_DELAY).await;
                if let Err(error) = deps.clipboard.restore(snapshot).await {
                    warnings.push(format!(
                        "failed to restore previous clipboard for session {session_id}: {error}"
                    ));
                }
            }
            Ok(warning_outcome(warnings))
        }
        Err(error) => {
            warnings.push(format!(
                "paste failed after clipboard write for session {session_id}: {error}"
            ));
            Ok(warning_outcome(warnings))
        }
    }
}

fn warning_outcome(warnings: Vec<String>) -> CycleOutcome {
    if warnings.is_empty() {
        CycleOutcome::Completed
    } else {
        CycleOutcome::CompletedWithWarning(warnings.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use async_trait::async_trait;
    use camino::Utf8PathBuf;

    use glossa_core::{AudioFormat, CapturedAudio, PasteMode, SessionId};

    use super::{
        paste_cycle, spawn_processing_task, CycleOutcome, InternalEvent, PipelineDependencies,
    };
    use crate::{
        ports::{
            ClipboardSnapshot, ClipboardWriter, PasteBackend, SilenceTrimmer, SttClient, TempStore,
            TextEnhancer,
        },
        AppError,
    };

    struct RecordingClipboard {
        text: Mutex<Vec<String>>,
        restores: Mutex<Vec<ClipboardSnapshot>>,
        snapshot: Mutex<Result<Option<ClipboardSnapshot>, String>>,
        restore_result: Mutex<Result<(), String>>,
    }

    impl RecordingClipboard {
        fn new() -> Self {
            Self {
                text: Mutex::new(Vec::new()),
                restores: Mutex::new(Vec::new()),
                snapshot: Mutex::new(Ok(Some(ClipboardSnapshot::new(
                    "text/plain;charset=utf-8".to_owned(),
                    b"previous".to_vec(),
                )))),
                restore_result: Mutex::new(Ok(())),
            }
        }

        fn recorded(&self) -> Vec<String> {
            self.text.lock().expect("clipboard lock").clone()
        }

        fn restored(&self) -> Vec<ClipboardSnapshot> {
            self.restores.lock().expect("clipboard lock").clone()
        }

        fn fail_restore(&self, message: impl Into<String>) {
            *self.restore_result.lock().expect("clipboard lock") = Err(message.into());
        }
    }

    #[async_trait]
    impl ClipboardWriter for RecordingClipboard {
        async fn snapshot(&self) -> Result<Option<ClipboardSnapshot>, AppError> {
            self.snapshot
                .lock()
                .expect("clipboard lock")
                .clone()
                .map_err(AppError::message)
        }

        async fn set_text(&self, text: &str) -> Result<(), AppError> {
            self.text
                .lock()
                .expect("clipboard lock")
                .push(text.to_owned());
            Ok(())
        }

        async fn restore(&self, snapshot: ClipboardSnapshot) -> Result<(), AppError> {
            self.restores.lock().expect("clipboard lock").push(snapshot);
            self.restore_result
                .lock()
                .expect("clipboard lock")
                .clone()
                .map_err(AppError::message)
        }
    }

    struct RecordingPaste {
        modes: Mutex<Vec<PasteMode>>,
        result: Mutex<Result<(), String>>,
    }

    impl RecordingPaste {
        fn new() -> Self {
            Self {
                modes: Mutex::new(Vec::new()),
                result: Mutex::new(Ok(())),
            }
        }

        fn recorded(&self) -> Vec<PasteMode> {
            self.modes.lock().expect("paste lock").clone()
        }

        fn fail(&self, message: impl Into<String>) {
            *self.result.lock().expect("paste lock") = Err(message.into());
        }
    }

    #[async_trait]
    impl PasteBackend for RecordingPaste {
        async fn paste(&self, mode: PasteMode) -> Result<(), AppError> {
            self.modes.lock().expect("paste lock").push(mode);
            self.result
                .lock()
                .expect("paste lock")
                .clone()
                .map_err(AppError::message)
        }
    }

    struct NoopTrimmer;

    #[async_trait]
    impl SilenceTrimmer for NoopTrimmer {
        async fn trim(&self, input: &CapturedAudio) -> Result<CapturedAudio, AppError> {
            Ok(input.clone())
        }
    }

    struct NoopSttClient;

    #[async_trait]
    impl SttClient for NoopSttClient {
        fn provider_name(&self) -> &'static str {
            "test"
        }

        async fn transcribe(&self, _audio: &CapturedAudio) -> Result<String, AppError> {
            Ok(String::new())
        }
    }

    struct NoopTempStore;

    #[async_trait]
    impl TempStore for NoopTempStore {
        async fn create_recording_path(
            &self,
            _session_id: SessionId,
            _format: AudioFormat,
        ) -> Result<Utf8PathBuf, AppError> {
            Err(AppError::message("not used in paste_cycle tests"))
        }

        async fn cleanup_session(&self, _session_id: SessionId) -> Result<(), AppError> {
            Ok(())
        }

        async fn purge_session(&self, _session_id: SessionId) -> Result<(), AppError> {
            Ok(())
        }

        async fn cleanup_stale_files(&self) -> Result<(), AppError> {
            Ok(())
        }
    }

    fn test_dependencies(
        append_space: bool,
    ) -> (
        PipelineDependencies,
        Arc<RecordingClipboard>,
        Arc<RecordingPaste>,
    ) {
        let clipboard = Arc::new(RecordingClipboard::new());
        let paste = Arc::new(RecordingPaste::new());
        let mut config = glossa_core::AppConfig::default();
        config.paste.append_space = append_space;

        (
            PipelineDependencies {
                config: Arc::new(config),
                trimmer: Arc::new(NoopTrimmer),
                stt_client: Arc::new(NoopSttClient),
                text_enhancer: Arc::new(crate::ports::NoopTextEnhancer),
                clipboard: clipboard.clone(),
                paste: paste.clone(),
                temp_store: Arc::new(NoopTempStore),
            },
            clipboard,
            paste,
        )
    }

    #[tokio::test]
    async fn paste_cycle_should_append_a_space_before_writing_to_clipboard_when_enabled() {
        let (deps, clipboard, paste) = test_dependencies(true);
        let session_id = SessionId::new();

        let outcome = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed");

        assert!(matches!(outcome, CycleOutcome::Completed));
        assert_eq!(clipboard.recorded(), vec!["hello ".to_owned()]);
        assert_eq!(
            clipboard.restored(),
            vec![ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                b"previous".to_vec(),
            )]
        );
        assert_eq!(paste.recorded(), vec![PasteMode::CtrlV]);
    }

    #[tokio::test]
    async fn paste_cycle_should_leave_text_unchanged_when_append_space_is_disabled() {
        let (deps, clipboard, paste) = test_dependencies(false);
        let session_id = SessionId::new();

        let outcome = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed");

        assert!(matches!(outcome, CycleOutcome::Completed));
        assert_eq!(clipboard.recorded(), vec!["hello".to_owned()]);
        assert_eq!(
            clipboard.restored(),
            vec![ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                b"previous".to_vec(),
            )]
        );
        assert_eq!(paste.recorded(), vec![PasteMode::CtrlV]);
    }

    #[tokio::test]
    async fn paste_cycle_should_not_restore_clipboard_when_paste_fails() {
        let (deps, clipboard, paste) = test_dependencies(false);
        paste.fail("paste command failed");
        let session_id = SessionId::new();

        let outcome = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed with warning");

        assert!(matches!(outcome, CycleOutcome::CompletedWithWarning(_)));
        assert_eq!(clipboard.recorded(), vec!["hello".to_owned()]);
        assert!(clipboard.restored().is_empty());
    }

    #[tokio::test]
    async fn paste_cycle_should_warn_when_clipboard_restore_fails() {
        let (deps, clipboard, _paste) = test_dependencies(false);
        clipboard.fail_restore("restore failed");
        let session_id = SessionId::new();

        let outcome = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed with warning");

        assert!(matches!(outcome, CycleOutcome::CompletedWithWarning(_)));
        assert_eq!(clipboard.recorded(), vec!["hello".to_owned()]);
        assert_eq!(
            clipboard.restored(),
            vec![ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                b"previous".to_vec(),
            )]
        );
    }

    #[tokio::test]
    async fn paste_cycle_should_wait_briefly_before_restoring_clipboard_after_paste() {
        let (deps, clipboard, _paste) = test_dependencies(false);
        let session_id = SessionId::new();
        let started = Instant::now();

        let outcome = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed");

        assert!(matches!(outcome, CycleOutcome::Completed));
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "clipboard restore should be delayed long enough for the target app to consume the paste"
        );
        assert_eq!(
            clipboard.restored(),
            vec![ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                b"previous".to_vec(),
            )]
        );
    }

    struct RecordingTextEnhancer {
        calls: Mutex<Vec<String>>,
        result: Mutex<Result<String, String>>,
    }

    impl RecordingTextEnhancer {
        fn new(result: Result<String, String>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(result),
            }
        }

        fn recorded(&self) -> Vec<String> {
            self.calls.lock().expect("enhancer lock").clone()
        }
    }

    #[async_trait]
    impl TextEnhancer for RecordingTextEnhancer {
        fn name(&self) -> &'static str {
            "test-enhancer"
        }

        async fn enhance(&self, text: &str) -> Result<String, AppError> {
            self.calls
                .lock()
                .expect("enhancer lock")
                .push(text.to_owned());
            let result = self.result.lock().expect("enhancer lock").clone();
            result.map_err(AppError::message)
        }
    }

    struct FixedSttClient {
        text: String,
    }

    #[async_trait]
    impl SttClient for FixedSttClient {
        fn provider_name(&self) -> &'static str {
            "test"
        }

        async fn transcribe(&self, _audio: &CapturedAudio) -> Result<String, AppError> {
            Ok(self.text.clone())
        }
    }

    fn processing_dependencies(
        llm_enabled: bool,
        stt_text: String,
        enhancer_result: Result<String, String>,
    ) -> (PipelineDependencies, Arc<RecordingTextEnhancer>) {
        let mut config = glossa_core::AppConfig::default();
        config.llm.enabled = llm_enabled;
        let enhancer = Arc::new(RecordingTextEnhancer::new(enhancer_result));

        (
            PipelineDependencies {
                config: Arc::new(config),
                trimmer: Arc::new(NoopTrimmer),
                stt_client: Arc::new(FixedSttClient { text: stt_text }),
                text_enhancer: enhancer.clone(),
                clipboard: Arc::new(RecordingClipboard::new()),
                paste: Arc::new(RecordingPaste::new()),
                temp_store: Arc::new(NoopTempStore),
            },
            enhancer,
        )
    }

    #[tokio::test]
    async fn processing_task_should_enhance_text_when_llm_enabled() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (deps, enhancer) =
            processing_dependencies(true, "hello world".into(), Ok("Hello, world!".into()));
        let audio = CapturedAudio {
            session_id: SessionId::new(),
            path: Utf8PathBuf::from("/tmp/test.wav"),
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        };

        spawn_processing_task(tx, deps, audio);

        let event = rx.recv().await.expect("should receive event");
        match event {
            InternalEvent::ProcessingReady { text, .. } => {
                assert_eq!(text, "Hello, world!");
            }
            other => panic!("expected ProcessingReady, got {other:?}"),
        }
        assert_eq!(enhancer.recorded(), vec!["hello world"]);
    }

    #[tokio::test]
    async fn processing_task_should_skip_enhancement_when_llm_disabled() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (deps, enhancer) =
            processing_dependencies(false, "hello world".into(), Ok("Hello, world!".into()));
        let audio = CapturedAudio {
            session_id: SessionId::new(),
            path: Utf8PathBuf::from("/tmp/test.wav"),
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        };

        spawn_processing_task(tx, deps, audio);

        let event = rx.recv().await.expect("should receive event");
        match event {
            InternalEvent::ProcessingReady { text, .. } => {
                assert_eq!(text, "hello world");
            }
            other => panic!("expected ProcessingReady, got {other:?}"),
        }
        assert!(enhancer.recorded().is_empty());
    }

    #[tokio::test]
    async fn processing_task_should_fail_when_enhancer_returns_empty() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (deps, _enhancer) =
            processing_dependencies(true, "hello world".into(), Ok("   ".into()));
        let audio = CapturedAudio {
            session_id: SessionId::new(),
            path: Utf8PathBuf::from("/tmp/test.wav"),
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        };

        spawn_processing_task(tx, deps, audio);

        let event = rx.recv().await.expect("should receive event");
        match event {
            InternalEvent::ProcessingFinished { outcome, .. } => match outcome {
                CycleOutcome::Failed(msg) => {
                    assert!(msg.contains("empty text"));
                }
                other => panic!("expected Failed, got {other:?}"),
            },
            other => panic!("expected ProcessingFinished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn processing_task_should_fail_when_enhancer_errors() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (deps, _enhancer) =
            processing_dependencies(true, "hello world".into(), Err("network error".into()));
        let audio = CapturedAudio {
            session_id: SessionId::new(),
            path: Utf8PathBuf::from("/tmp/test.wav"),
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        };

        spawn_processing_task(tx, deps, audio);

        let event = rx.recv().await.expect("should receive event");
        match event {
            InternalEvent::ProcessingFinished { outcome, .. } => match outcome {
                CycleOutcome::Failed(msg) => {
                    assert!(msg.contains("network error"));
                }
                other => panic!("expected Failed, got {other:?}"),
            },
            other => panic!("expected ProcessingFinished, got {other:?}"),
        }
    }
}
