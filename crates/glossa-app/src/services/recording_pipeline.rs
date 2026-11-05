use std::{borrow::Cow, sync::Arc, time::Duration};

use tokio::{
    sync::mpsc,
    task,
    time::{sleep, timeout},
};
use tracing::{info, warn};

use glossa_core::{AppConfig, CapturedAudio, SessionId};

use crate::{
    ports::{
        AudioEncoder, ClipboardSnapshot, ClipboardWriter, PasteBackend, SilenceTrimmer, SttClient,
        TempStore, TextEnhancer,
    },
    AppError,
};

// shortcut: a blind timer, because `dotoolc` returns before the keys are pressed and nothing reports
// the target's read. A target busier than this pastes the previous clipboard; replace the timer if
// the compositor ever offers a way to observe that read without debug output.
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(1500);
// shortcut: on GNOME a `wl-copy`/`wl-paste` popup mapped while keys are being pressed may never get
// keyboard focus and then waits forever. The restore can run while the user types and the next paste
// waits for it, so retry. Drop this if the clipboard stops depending on focus popups.
const CLIPBOARD_RESTORE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const CLIPBOARD_RESTORE_ATTEMPTS: u32 = 5;

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
    pub encoder: Arc<dyn AudioEncoder>,
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

        let processed_audio = match deps
            .encoder
            .encode(&processed_audio, deps.config.audio.format)
            .await
        {
            Ok(encoded) => encoded,
            Err(error) => {
                let _ = deps.temp_store.cleanup_session(session_id).await;
                let _ = tx.send(InternalEvent::ProcessingFinished {
                    session_id,
                    outcome: CycleOutcome::Failed(error.to_string()),
                });
                return;
            }
        };

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

struct PendingRestore {
    previous: ClipboardSnapshot,
    pasted_text: String,
}

/// Spawns the final clipboard + paste phase for a transcription result.
///
/// The task sends `PasteFinished` once the keys are sent, then keeps running until the previous
/// clipboard is restored. Pass its handle to the next call so that paste waits for the restore.
pub fn spawn_paste_task(
    tx: mpsc::UnboundedSender<InternalEvent>,
    deps: PipelineDependencies,
    session_id: SessionId,
    text: String,
    previous_paste: Option<task::JoinHandle<()>>,
) -> task::JoinHandle<()> {
    task::spawn(async move {
        if let Some(previous_paste) = previous_paste {
            let _ = previous_paste.await;
        }

        let (outcome, pending_restore) = match paste_cycle(&deps, session_id, &text).await {
            Ok(pasted) => pasted,
            Err(error) => (CycleOutcome::Failed(error.to_string()), None),
        };

        let _ = deps.temp_store.cleanup_session(session_id).await;
        let _ = tx.send(InternalEvent::PasteFinished {
            session_id,
            outcome,
        });

        if let Some(pending_restore) = pending_restore {
            if let Err(error) = restore_after_paste(&deps, session_id, pending_restore).await {
                warn!(%session_id, %error, "failed to restore previous clipboard");
            }
        }
    })
}

async fn paste_cycle(
    deps: &PipelineDependencies,
    session_id: SessionId,
    text: &str,
) -> Result<(CycleOutcome, Option<PendingRestore>), AppError> {
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
            let pending_restore = previous_clipboard.map(|previous| PendingRestore {
                previous,
                pasted_text: clipboard_text.into_owned(),
            });
            Ok((warning_outcome(warnings), pending_restore))
        }
        Err(error) => {
            warnings.push(format!(
                "paste failed after clipboard write for session {session_id}: {error}"
            ));
            Ok((warning_outcome(warnings), None))
        }
    }
}

async fn restore_after_paste(
    deps: &PipelineDependencies,
    session_id: SessionId,
    pending: PendingRestore,
) -> Result<(), AppError> {
    sleep(CLIPBOARD_RESTORE_DELAY).await;
    for _ in 0..CLIPBOARD_RESTORE_ATTEMPTS {
        let attempt = restore_if_unchanged(deps, session_id, &pending);
        if let Ok(result) = timeout(CLIPBOARD_RESTORE_ATTEMPT_TIMEOUT, attempt).await {
            return result;
        }
        info!(%session_id, "clipboard restore attempt timed out");
    }
    Err(AppError::message("clipboard restore timed out"))
}

async fn restore_if_unchanged(
    deps: &PipelineDependencies,
    session_id: SessionId,
    pending: &PendingRestore,
) -> Result<(), AppError> {
    if deps.clipboard.holds_text(&pending.pasted_text).await? {
        deps.clipboard.restore(pending.previous.clone()).await
    } else {
        info!(%session_id, "clipboard changed after paste; keeping it");
        Ok(())
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
pub(crate) mod test_support {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::{
        ports::{ClipboardSnapshot, ClipboardWriter},
        AppError,
    };

    pub(crate) struct SharedSelection {
        current: Mutex<Vec<u8>>,
        hung_checks_left: Mutex<u32>,
    }

    impl SharedSelection {
        pub(crate) fn holding(text: &str) -> Self {
            Self {
                current: Mutex::new(text.as_bytes().to_vec()),
                hung_checks_left: Mutex::new(0),
            }
        }

        pub(crate) fn hang_next_checks(&self, count: u32) {
            *self.hung_checks_left.lock().expect("selection lock") = count;
        }

        pub(crate) fn read(&self) -> String {
            String::from_utf8_lossy(&self.current.lock().expect("selection lock")).into_owned()
        }

        pub(crate) fn copy(&self, text: &str) {
            *self.current.lock().expect("selection lock") = text.as_bytes().to_vec();
        }
    }

    #[async_trait]
    impl ClipboardWriter for SharedSelection {
        async fn snapshot(&self) -> Result<Option<ClipboardSnapshot>, AppError> {
            Ok(Some(ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                self.read().into_bytes(),
            )))
        }

        async fn set_text(&self, text: &str) -> Result<(), AppError> {
            self.copy(text);
            Ok(())
        }

        async fn holds_text(&self, text: &str) -> Result<bool, AppError> {
            let hangs = {
                let mut left = self.hung_checks_left.lock().expect("selection lock");
                let hangs = *left > 0;
                *left = left.saturating_sub(1);
                hangs
            };
            if hangs {
                std::future::pending::<()>().await;
            }
            Ok(self.read() == text)
        }

        async fn restore(&self, snapshot: ClipboardSnapshot) -> Result<(), AppError> {
            *self.current.lock().expect("selection lock") = snapshot.bytes;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use async_trait::async_trait;
    use camino::Utf8PathBuf;

    use glossa_core::{AudioFormat, CapturedAudio, PasteMode, SessionId};

    use super::{
        paste_cycle, restore_after_paste, spawn_paste_task, spawn_processing_task,
        test_support::SharedSelection, CycleOutcome, InternalEvent, PipelineDependencies,
    };
    use crate::{
        ports::{
            AudioEncoder, ClipboardSnapshot, ClipboardWriter, PasteBackend, SilenceTrimmer,
            SttClient, TempStore, TextEnhancer,
        },
        AppError,
    };

    struct RecordingClipboard {
        text: Mutex<Vec<String>>,
        restores: Mutex<Vec<ClipboardSnapshot>>,
        snapshot: Mutex<Result<Option<ClipboardSnapshot>, String>>,
        holds_text_result: Mutex<Result<bool, String>>,
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
                holds_text_result: Mutex::new(Ok(true)),
                restore_result: Mutex::new(Ok(())),
            }
        }

        fn recorded(&self) -> Vec<String> {
            self.text.lock().expect("clipboard lock").clone()
        }

        fn restored(&self) -> Vec<ClipboardSnapshot> {
            self.restores.lock().expect("clipboard lock").clone()
        }

        fn fail_holds_text(&self, message: impl Into<String>) {
            *self.holds_text_result.lock().expect("clipboard lock") = Err(message.into());
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

        async fn holds_text(&self, _text: &str) -> Result<bool, AppError> {
            self.holds_text_result
                .lock()
                .expect("clipboard lock")
                .clone()
                .map_err(AppError::message)
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

    #[async_trait]
    impl AudioEncoder for NoopTrimmer {
        async fn encode(
            &self,
            input: &CapturedAudio,
            _format: AudioFormat,
        ) -> Result<CapturedAudio, AppError> {
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
                encoder: Arc::new(NoopTrimmer),
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

    async fn paste_and_restore(
        deps: &PipelineDependencies,
        session_id: SessionId,
        text: &str,
    ) -> CycleOutcome {
        let (outcome, pending_restore) = paste_cycle(deps, session_id, text)
            .await
            .expect("paste cycle should succeed");
        if let Some(pending_restore) = pending_restore {
            restore_after_paste(deps, session_id, pending_restore)
                .await
                .expect("clipboard restore should succeed");
        }
        outcome
    }

    #[tokio::test(start_paused = true)]
    async fn paste_cycle_should_append_a_space_before_writing_to_clipboard_when_enabled() {
        let (deps, clipboard, paste) = test_dependencies(true);
        let session_id = SessionId::new();

        let outcome = paste_and_restore(&deps, session_id, "hello").await;

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

    #[tokio::test(start_paused = true)]
    async fn paste_cycle_should_leave_text_unchanged_when_append_space_is_disabled() {
        let (deps, clipboard, paste) = test_dependencies(false);
        let session_id = SessionId::new();

        let outcome = paste_and_restore(&deps, session_id, "hello").await;

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

        let outcome = paste_and_restore(&deps, session_id, "hello").await;

        assert!(matches!(outcome, CycleOutcome::CompletedWithWarning(_)));
        assert_eq!(clipboard.recorded(), vec!["hello".to_owned()]);
        assert!(clipboard.restored().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn restore_after_paste_should_report_a_failed_clipboard_restore() {
        let (deps, clipboard, _paste) = test_dependencies(false);
        clipboard.fail_restore("restore failed");
        let session_id = SessionId::new();

        let (_outcome, pending_restore) = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed");
        let restored = restore_after_paste(
            &deps,
            session_id,
            pending_restore.expect("previous clipboard should be pending"),
        )
        .await;

        assert!(restored.is_err());
        assert_eq!(clipboard.recorded(), vec!["hello".to_owned()]);
        assert_eq!(
            clipboard.restored(),
            vec![ClipboardSnapshot::new(
                "text/plain;charset=utf-8".to_owned(),
                b"previous".to_vec(),
            )]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn restore_after_paste_should_not_overwrite_a_clipboard_it_cannot_read() {
        let (deps, clipboard, _paste) = test_dependencies(false);
        clipboard.fail_holds_text("read failed");
        let session_id = SessionId::new();

        let (_outcome, pending_restore) = paste_cycle(&deps, session_id, "hello")
            .await
            .expect("paste cycle should succeed");
        let restored = restore_after_paste(
            &deps,
            session_id,
            pending_restore.expect("previous clipboard should be pending"),
        )
        .await;

        assert!(restored.is_err());
        assert!(clipboard.restored().is_empty());
    }

    struct LateReadingTarget {
        selection: Arc<SharedSelection>,
        reads_after: Duration,
        pasted: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl PasteBackend for LateReadingTarget {
        async fn paste(&self, _mode: PasteMode) -> Result<(), AppError> {
            let selection = Arc::clone(&self.selection);
            let pasted = Arc::clone(&self.pasted);
            let reads_after = self.reads_after;
            tokio::spawn(async move {
                tokio::time::sleep(reads_after).await;
                pasted.lock().expect("pasted lock").push(selection.read());
            });
            Ok(())
        }
    }

    fn live_dependencies(
        previous: &str,
        target_reads_after: Duration,
    ) -> (
        PipelineDependencies,
        Arc<SharedSelection>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let selection = Arc::new(SharedSelection::holding(previous));
        let pasted = Arc::new(Mutex::new(Vec::new()));
        let mut config = glossa_core::AppConfig::default();
        config.paste.append_space = false;

        (
            PipelineDependencies {
                config: Arc::new(config),
                trimmer: Arc::new(NoopTrimmer),
                encoder: Arc::new(NoopTrimmer),
                stt_client: Arc::new(NoopSttClient),
                text_enhancer: Arc::new(crate::ports::NoopTextEnhancer),
                clipboard: selection.clone(),
                paste: Arc::new(LateReadingTarget {
                    selection: selection.clone(),
                    reads_after: target_reads_after,
                    pasted: pasted.clone(),
                }),
                temp_store: Arc::new(NoopTempStore),
            },
            selection,
            pasted,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn slow_target_should_paste_the_transcription_instead_of_the_previous_clipboard() {
        let (deps, selection, pasted) = live_dependencies("previous", Duration::from_secs(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        spawn_paste_task(tx, deps, SessionId::new(), "hello".to_owned(), None);
        rx.recv().await.expect("paste should finish");
        tokio::time::sleep(Duration::from_secs(5)).await;

        assert_eq!(
            *pasted.lock().expect("pasted lock"),
            vec!["hello".to_owned()]
        );
        assert_eq!(selection.read(), "previous");
    }

    #[tokio::test(start_paused = true)]
    async fn paste_finished_should_not_wait_for_the_clipboard_restore() {
        let (deps, selection, _pasted) = live_dependencies("previous", Duration::ZERO);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        spawn_paste_task(tx, deps, SessionId::new(), "hello".to_owned(), None);
        let event = rx.recv().await.expect("paste should finish");

        assert!(matches!(event, InternalEvent::PasteFinished { .. }));
        assert_eq!(selection.read(), "hello");

        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(selection.read(), "previous");
    }

    #[tokio::test(start_paused = true)]
    async fn next_paste_should_wait_for_the_pending_clipboard_restore() {
        let (deps, selection, pasted) = live_dependencies("previous", Duration::from_secs(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let first = spawn_paste_task(
            tx.clone(),
            deps.clone(),
            SessionId::new(),
            "first".to_owned(),
            None,
        );
        rx.recv().await.expect("first paste should finish");
        spawn_paste_task(tx, deps, SessionId::new(), "second".to_owned(), Some(first));
        rx.recv().await.expect("second paste should finish");
        tokio::time::sleep(Duration::from_secs(5)).await;

        assert_eq!(
            *pasted.lock().expect("pasted lock"),
            vec!["first".to_owned(), "second".to_owned()]
        );
        assert_eq!(selection.read(), "previous");
    }

    #[tokio::test(start_paused = true)]
    async fn clipboard_restore_should_retry_after_a_hung_clipboard_command() {
        let (deps, selection, _pasted) = live_dependencies("previous", Duration::ZERO);
        selection.hang_next_checks(2);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        spawn_paste_task(tx, deps, SessionId::new(), "hello".to_owned(), None);
        rx.recv().await.expect("paste should finish");
        tokio::time::sleep(Duration::from_secs(10)).await;

        assert_eq!(selection.read(), "previous");
    }

    #[tokio::test(start_paused = true)]
    async fn next_paste_should_survive_a_clipboard_restore_that_never_finishes() {
        let (deps, selection, pasted) = live_dependencies("previous", Duration::ZERO);
        selection.hang_next_checks(u32::MAX);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let first = spawn_paste_task(
            tx.clone(),
            deps.clone(),
            SessionId::new(),
            "first".to_owned(),
            None,
        );
        rx.recv().await.expect("first paste should finish");
        spawn_paste_task(tx, deps, SessionId::new(), "second".to_owned(), Some(first));
        tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("second paste should not wait forever")
            .expect("second paste should finish");
        tokio::time::sleep(Duration::from_millis(1)).await;

        assert_eq!(
            *pasted.lock().expect("pasted lock"),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn clipboard_restore_should_keep_text_copied_after_the_paste() {
        let (deps, selection, _pasted) = live_dependencies("previous", Duration::ZERO);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        spawn_paste_task(tx, deps, SessionId::new(), "hello".to_owned(), None);
        rx.recv().await.expect("paste should finish");
        selection.copy("copied by the user");
        tokio::time::sleep(Duration::from_secs(5)).await;

        assert_eq!(selection.read(), "copied by the user");
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
                encoder: Arc::new(NoopTrimmer),
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

    struct PipelineProbe {
        events: Mutex<Vec<&'static str>>,
        fail_encoding: bool,
    }

    #[async_trait]
    impl SilenceTrimmer for PipelineProbe {
        async fn trim(&self, input: &CapturedAudio) -> Result<CapturedAudio, AppError> {
            assert_eq!(input.path.extension(), Some("wav"));
            self.events.lock().expect("events lock").push("trim");
            Ok(CapturedAudio {
                path: input.path.with_file_name("test.trimmed.wav"),
                duration_ms: 500,
                ..input.clone()
            })
        }
    }

    #[async_trait]
    impl AudioEncoder for PipelineProbe {
        async fn encode(
            &self,
            input: &CapturedAudio,
            format: AudioFormat,
        ) -> Result<CapturedAudio, AppError> {
            assert_eq!(input.path.file_name(), Some("test.trimmed.wav"));
            assert_eq!(input.duration_ms, 500);
            assert_eq!(format, AudioFormat::Mp3);
            self.events.lock().expect("events lock").push("encode");
            if self.fail_encoding {
                return Err(AppError::message("encoding failed"));
            }
            Ok(CapturedAudio {
                path: input.path.with_extension(format.extension()),
                ..input.clone()
            })
        }
    }

    #[async_trait]
    impl SttClient for PipelineProbe {
        fn provider_name(&self) -> &'static str {
            "test"
        }

        async fn transcribe(&self, audio: &CapturedAudio) -> Result<String, AppError> {
            assert_eq!(audio.path.file_name(), Some("test.trimmed.mp3"));
            self.events.lock().expect("events lock").push("transcribe");
            Ok("hello".into())
        }
    }

    #[async_trait]
    impl TempStore for PipelineProbe {
        async fn create_recording_path(
            &self,
            _session_id: SessionId,
            _format: AudioFormat,
        ) -> Result<Utf8PathBuf, AppError> {
            Err(AppError::message("unused"))
        }

        async fn cleanup_session(&self, _session_id: SessionId) -> Result<(), AppError> {
            self.events.lock().expect("events lock").push("cleanup");
            Ok(())
        }

        async fn purge_session(&self, _session_id: SessionId) -> Result<(), AppError> {
            Ok(())
        }

        async fn cleanup_stale_files(&self) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn processing_should_encode_after_trimming_and_skip_upload_on_encoding_failure() {
        for fail_encoding in [false, true] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut deps, _) =
                processing_dependencies(false, "unused".into(), Ok("unused".into()));
            Arc::make_mut(&mut deps.config).audio.format = AudioFormat::Mp3;
            let probe = Arc::new(PipelineProbe {
                events: Mutex::new(Vec::new()),
                fail_encoding,
            });
            deps.trimmer = probe.clone();
            deps.encoder = probe.clone();
            deps.stt_client = probe.clone();
            deps.temp_store = probe.clone();
            let audio = CapturedAudio {
                session_id: SessionId::new(),
                path: "/tmp/test.wav".into(),
                duration_ms: 1000,
                sample_rate_hz: 16000,
                channels: 1,
            };
            let session_id = audio.session_id;

            spawn_processing_task(tx, deps, audio);

            let event = rx.recv().await.expect("processing event");
            if fail_encoding {
                assert!(matches!(event, InternalEvent::ProcessingFinished {
                    session_id: id,
                    outcome: CycleOutcome::Failed(ref message),
                } if id == session_id && message == "encoding failed"));
                assert_eq!(
                    *probe.events.lock().expect("events lock"),
                    ["trim", "encode", "cleanup"]
                );
            } else {
                assert!(matches!(event, InternalEvent::ProcessingReady {
                    session_id: id,
                    ref text,
                } if id == session_id && text == "hello"));
                assert_eq!(
                    *probe.events.lock().expect("events lock"),
                    ["trim", "encode", "transcribe"]
                );
            }
        }
    }
}
