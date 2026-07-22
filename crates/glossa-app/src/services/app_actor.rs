use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use glossa_core::{
    AppCommand, AppConfig, AppState, LatencyMode, PastingState, RecordSpec, SessionId,
};

use crate::{
    machine::{reduce, Action},
    ports::{
        ActiveRecording, AudioCapture, ClipboardWriter, CuePlayer, PasteBackend, SilenceTrimmer,
        SttClient, TempStore, TextEnhancer, TrayPort, TrayState,
    },
    services::{
        command_router::AppHandle,
        recording_pipeline::{
            spawn_paste_task, spawn_processing_task, CycleOutcome, InternalEvent,
            PipelineDependencies,
        },
        status_store::StatusStore,
    },
    AppError,
};

/// Runtime dependencies owned by the application actor.
#[derive(Clone)]
pub struct AppDependencies {
    pub audio_capture: Arc<dyn AudioCapture>,
    pub trimmer: Arc<dyn SilenceTrimmer>,
    pub cue_player: Arc<dyn CuePlayer>,
    pub stt_client: Arc<dyn SttClient>,
    pub text_enhancer: Arc<dyn TextEnhancer>,
    pub clipboard: Arc<dyn ClipboardWriter>,
    pub paste: Arc<dyn PasteBackend>,
    pub tray: Arc<dyn TrayPort>,
    pub temp_store: Arc<dyn TempStore>,
}

/// Main daemon actor that serializes all state transitions.
pub struct AppActor {
    config: Arc<AppConfig>,
    deps: AppDependencies,
    state: AppState,
    active_recording: Option<Box<dyn ActiveRecording>>,
    recording_deadline: Option<tokio::time::Instant>,
    command_rx: mpsc::UnboundedReceiver<AppCommand>,
    internal_tx: mpsc::UnboundedSender<InternalEvent>,
    internal_rx: mpsc::UnboundedReceiver<InternalEvent>,
    status_store: StatusStore,
    manual_input_stream_override: Option<bool>,
}

/// Reason why the actor stopped processing commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorExit {
    Restart,
    Shutdown,
}

impl AppActor {
    /// Creates a new actor plus the handle that external components can use.
    #[must_use]
    pub fn new(config: AppConfig, deps: AppDependencies) -> (Self, AppHandle) {
        let config = Arc::new(config);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let status_store = StatusStore::new(config.initial_status());
        let handle = AppHandle::new(command_tx, status_store.clone());
        let actor = Self {
            config,
            deps,
            state: AppState::Idle,
            active_recording: None,
            recording_deadline: None,
            command_rx,
            internal_tx,
            internal_rx,
            status_store,
            manual_input_stream_override: None,
        };
        (actor, handle)
    }

    /// Runs the actor until shutdown is requested.
    pub async fn run(mut self) -> Result<ActorExit, AppError> {
        self.deps.temp_store.cleanup_stale_files().await?;
        self.set_tray_state(TrayState::Idle).await;
        self.apply_initial_latency_policy().await;
        self.publish_status();

        loop {
            let recording_deadline = self.recording_deadline;
            tokio::select! {
                maybe_command = self.command_rx.recv() => {
                    let Some(command) = maybe_command else {
                        break;
                    };
                    if let Some(exit) = self.handle_command(command).await? {
                        return Ok(exit);
                    }
                }
                maybe_event = self.internal_rx.recv() => {
                    let Some(event) = maybe_event else {
                        break;
                    };
                    self.handle_internal_event(event).await?;
                }
                _ = tokio::time::sleep_until(
                    recording_deadline.unwrap_or_else(tokio::time::Instant::now)
                ), if recording_deadline.is_some() => {
                    if let Some(exit) = self.handle_command(AppCommand::StopRecording {
                        origin: glossa_core::CommandOrigin::Internal,
                    }).await? {
                        return Ok(exit);
                    }
                }
            }
        }

        Ok(ActorExit::Shutdown)
    }

    async fn handle_command(&mut self, command: AppCommand) -> Result<Option<ActorExit>, AppError> {
        info!(state = ?self.state.kind(), command = ?command, "received command");
        let decision = reduce(&self.state, &command)?;
        self.state = decision.next_state;
        if !matches!(self.state, AppState::Recording(_)) {
            self.recording_deadline = None;
        }
        self.publish_status();

        for action in decision.actions {
            match action {
                Action::StartRecording { session_id } => self.start_recording(session_id).await?,
                Action::StopRecording { session_id } => self.stop_recording(session_id).await?,
                Action::AbortRecording { session_id } => self.abort_recording(session_id).await,
                Action::SetTrayIdle => self.set_tray_state(TrayState::Idle).await,
                Action::SetTrayRecording => self.set_tray_state(TrayState::Recording).await,
                Action::SetTrayProcessing => self.set_tray_state(TrayState::Processing).await,
                Action::PlayStartCue => {
                    if let Err(error) = self.deps.cue_player.play_start().await {
                        warn!(error = %error, "failed to play start cue");
                    }
                }
                Action::PlayStopCue => {
                    if let Err(error) = self.deps.cue_player.play_stop().await {
                        warn!(error = %error, "failed to play stop cue");
                    }
                }
                Action::ToggleInputStream => {
                    self.toggle_input_stream().await;
                }
                Action::EnableInputStream => {
                    self.enable_input_stream().await;
                }
                Action::DisableInputStream => {
                    self.disable_input_stream().await;
                }
                Action::Ignore { reason } => {
                    info!(reason, "ignored command");
                }
                Action::Restart => {
                    self.shutdown().await?;
                    return Ok(Some(ActorExit::Restart));
                }
                Action::Shutdown => {
                    self.shutdown().await?;
                    return Ok(Some(ActorExit::Shutdown));
                }
            }
        }

        Ok(None)
    }

    async fn handle_internal_event(&mut self, event: InternalEvent) -> Result<(), AppError> {
        match event {
            InternalEvent::ProcessingReady { session_id, text } => {
                if !matches!(
                    self.state,
                    AppState::Processing(glossa_core::ProcessingState { session_id: current })
                        if current == session_id
                ) {
                    return Ok(());
                }

                self.state = AppState::Pasting(PastingState {
                    session_id,
                    text_len: text.len(),
                });
                self.publish_status();
                spawn_paste_task(
                    self.internal_tx.clone(),
                    self.pipeline_dependencies(),
                    session_id,
                    text,
                );
            }
            InternalEvent::ProcessingFinished {
                session_id,
                outcome,
            } => {
                self.finish_cycle(session_id, outcome).await;
            }
            InternalEvent::PasteFinished {
                session_id,
                outcome,
            } => {
                self.finish_cycle(session_id, outcome).await;
            }
            InternalEvent::IdleStreamStatusRefresh => {
                self.update_mic_stream_tray_state().await;
            }
        }
        Ok(())
    }

    async fn abort_recording(&mut self, session_id: SessionId) {
        let abort_error = match self.active_recording.take() {
            Some(recording) => recording.abort().await.err(),
            None => {
                warn!(%session_id, "cancel requested without an active recording");
                None
            }
        };
        let purge_error = self.deps.temp_store.purge_session(session_id).await.err();

        match (abort_error, purge_error) {
            (None, None) => {
                info!(%session_id, "recording cancelled");
            }
            (abort_error, purge_error) => {
                let error_message = match (abort_error, purge_error) {
                    (Some(abort_error), Some(purge_error)) => {
                        format!(
                            "failed to cancel recording for session {session_id}: \
                             abort failed: {abort_error}; purge failed: {purge_error}"
                        )
                    }
                    (Some(abort_error), None) => {
                        format!(
                            "failed to cancel recording for session {session_id}: {abort_error}"
                        )
                    }
                    (None, Some(purge_error)) => {
                        format!(
                            "failed to purge cancelled recording for session {session_id}: \
                             {purge_error}"
                        )
                    }
                    (None, None) => unreachable!("success case handled above"),
                };
                error!(%session_id, error = %error_message, "recording cancel failed");
                let _ = self.deps.tray.show_error(&error_message).await;
            }
        }

        self.apply_post_recording_latency_policy().await;
    }

    async fn start_recording(&mut self, session_id: SessionId) -> Result<(), AppError> {
        self.close_idle_stream_for_recording().await;
        let startup_started_at = Instant::now();
        let path_started_at = Instant::now();
        let path = self
            .deps
            .temp_store
            .create_recording_path(session_id, self.config.audio.format)
            .await?;
        let path_prep_ms = path_started_at.elapsed().as_millis();
        let spec = RecordSpec {
            sample_rate_hz: self.config.audio.sample_rate_hz,
            channels: self.config.audio.channels,
            format: self.config.audio.format,
            max_duration_sec: self.config.audio.max_duration_sec,
        };
        let capture_started_at = Instant::now();
        match self
            .deps
            .audio_capture
            .start(session_id, spec, path.as_path())
            .await
        {
            Ok(recording) => {
                self.active_recording = Some(recording);
                self.recording_deadline = Some(
                    tokio::time::Instant::now()
                        + Duration::from_secs(u64::from(self.config.audio.max_duration_sec)),
                );
                info!(
                    %session_id,
                    path = %path,
                    path_prep_ms,
                    capture_startup_ms = capture_started_at.elapsed().as_millis(),
                    total_startup_ms = startup_started_at.elapsed().as_millis(),
                    "recording started"
                );
            }
            Err(error) => {
                error!(
                    %session_id,
                    error = %error,
                    path_prep_ms,
                    capture_startup_ms = capture_started_at.elapsed().as_millis(),
                    total_startup_ms = startup_started_at.elapsed().as_millis(),
                    "failed to start recording"
                );
                self.state = AppState::Idle;
                self.publish_status();
                self.set_tray_state(TrayState::Idle).await;
                self.apply_post_recording_latency_policy().await;
                let _ = self.deps.temp_store.cleanup_session(session_id).await;
                let _ = self.deps.tray.show_error(&error.to_string()).await;
            }
        }
        Ok(())
    }

    async fn stop_recording(&mut self, session_id: SessionId) -> Result<(), AppError> {
        let Some(recording) = self.active_recording.take() else {
            warn!(%session_id, "stop requested without an active recording");
            self.state = AppState::Idle;
            self.publish_status();
            self.set_tray_state(TrayState::Idle).await;
            self.apply_post_recording_latency_policy().await;
            return Ok(());
        };

        match recording.stop().await {
            Ok(audio) => {
                info!(%session_id, duration_ms = audio.duration_ms, path = %audio.path, "recording stopped");
                spawn_processing_task(
                    self.internal_tx.clone(),
                    self.pipeline_dependencies(),
                    audio,
                );
            }
            Err(error) => {
                error!(%session_id, error = %error, "failed to stop recording");
                self.finish_cycle(session_id, CycleOutcome::Failed(error.to_string()))
                    .await;
            }
        }
        self.apply_post_recording_latency_policy().await;
        Ok(())
    }

    async fn finish_cycle(&mut self, session_id: SessionId, outcome: CycleOutcome) {
        match &outcome {
            CycleOutcome::Completed => info!(%session_id, "cycle completed"),
            CycleOutcome::CompletedWithWarning(reason) => {
                warn!(%session_id, reason, "cycle completed with warning");
            }
            CycleOutcome::Failed(reason) => {
                error!(%session_id, reason, "cycle failed");
                let _ = self.deps.tray.show_error(reason).await;
            }
        }

        self.state = AppState::Idle;
        self.publish_status();
        self.set_tray_state(TrayState::Idle).await;
    }

    async fn shutdown(&mut self) -> Result<(), AppError> {
        if let Some(recording) = self.active_recording.take() {
            let _ = recording.abort().await;
        }
        self.ensure_idle_stream_off("failed to close idle input stream during shutdown")
            .await;
        self.state = AppState::ShuttingDown;
        self.publish_status();
        self.set_tray_state(TrayState::Idle).await;
        self.set_mic_stream_tray_state(false).await;
        Ok(())
    }

    async fn apply_initial_latency_policy(&self) {
        if self.config.audio.latency_mode == LatencyMode::Instant {
            self.ensure_idle_stream_on("failed to start instant idle input stream")
                .await;
        } else {
            self.ensure_idle_stream_off("failed to close idle input stream at startup")
                .await;
        }
        self.update_mic_stream_tray_state().await;
    }

    async fn close_idle_stream_for_recording(&self) {
        self.ensure_idle_stream_off("failed to close idle input stream before recording")
            .await;
        self.update_mic_stream_tray_state().await;
    }

    async fn apply_post_recording_latency_policy(&self) {
        match self.manual_input_stream_override {
            Some(true) => {
                self.ensure_idle_stream_on("failed to start manually enabled idle input stream")
                    .await;
            }
            Some(false) => {
                self.ensure_idle_stream_off("failed to keep idle input stream disabled")
                    .await;
            }
            None => match self.config.audio.latency_mode {
                LatencyMode::Off => {
                    self.ensure_idle_stream_off("failed to close idle input stream")
                        .await;
                }
                LatencyMode::Balanced => {
                    let timeout =
                        Duration::from_secs(self.config.audio.keepalive_after_stop_seconds);
                    if let Err(error) = self
                        .deps
                        .audio_capture
                        .schedule_idle_stream_timeout(timeout)
                        .await
                    {
                        warn!(error = %error, "failed to schedule balanced idle input stream");
                    } else {
                        self.spawn_idle_stream_status_refresh(timeout);
                    }
                }
                LatencyMode::Instant => {
                    self.ensure_idle_stream_on("failed to start instant idle input stream")
                        .await;
                }
            },
        }
        self.update_mic_stream_tray_state().await;
    }

    async fn enable_input_stream(&mut self) {
        self.manual_input_stream_override = Some(true);
        if self.active_recording.is_none() {
            if let Err(error) = self.deps.audio_capture.ensure_idle_stream_on().await {
                warn!(error = %error, "failed to manually enable idle input stream");
                let _ = self.deps.tray.show_error(&error.to_string()).await;
            }
        }
        self.update_mic_stream_tray_state().await;
    }

    async fn toggle_input_stream(&mut self) {
        let active = self.manual_input_stream_override == Some(true)
            || self.deps.audio_capture.is_idle_stream_active().await;
        if active {
            self.disable_input_stream().await;
        } else {
            self.enable_input_stream().await;
        }
    }

    async fn disable_input_stream(&mut self) {
        self.manual_input_stream_override =
            if self.config.audio.latency_mode == LatencyMode::Instant {
                Some(false)
            } else {
                None
            };
        self.ensure_idle_stream_off("failed to manually disable idle input stream")
            .await;
        self.update_mic_stream_tray_state().await;
    }

    async fn ensure_idle_stream_on(&self, log_message: &'static str) {
        if let Err(error) = self.deps.audio_capture.ensure_idle_stream_on().await {
            warn!(error = %error, "{log_message}");
        }
    }

    async fn ensure_idle_stream_off(&self, log_message: &'static str) {
        if let Err(error) = self.deps.audio_capture.ensure_idle_stream_off().await {
            warn!(error = %error, "{log_message}");
        }
    }

    async fn update_mic_stream_tray_state(&self) {
        let active = self.manual_input_stream_override == Some(true)
            || self.deps.audio_capture.is_idle_stream_active().await;
        self.set_mic_stream_tray_state(active).await;
    }

    async fn set_mic_stream_tray_state(&self, active: bool) {
        if let Err(error) = self.deps.tray.set_mic_stream_state(active).await {
            warn!(error = %error, "failed to update tray mic stream state");
        }
    }

    fn spawn_idle_stream_status_refresh(&self, timeout: Duration) {
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = tx.send(InternalEvent::IdleStreamStatusRefresh);
        });
    }

    fn publish_status(&self) {
        let mut status = self.config.initial_status();
        status.state = self.state.kind();
        self.status_store.update(status);
    }

    async fn set_tray_state(&self, state: TrayState) {
        if let Err(error) = self.deps.tray.set_state(state).await {
            warn!(error = %error, "failed to update tray state");
        }
    }

    fn pipeline_dependencies(&self) -> PipelineDependencies {
        PipelineDependencies {
            config: Arc::clone(&self.config),
            trimmer: Arc::clone(&self.deps.trimmer),
            stt_client: Arc::clone(&self.deps.stt_client),
            text_enhancer: Arc::clone(&self.deps.text_enhancer),
            clipboard: Arc::clone(&self.deps.clipboard),
            paste: Arc::clone(&self.deps.paste),
            temp_store: Arc::clone(&self.deps.temp_store),
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
    use camino::{Utf8Path, Utf8PathBuf};

    use glossa_core::{AppStateKind, AudioFormat, CapturedAudio, CommandOrigin, LatencyMode};

    use super::*;
    use crate::ports::{
        AudioCapture, ClipboardWriter, CuePlayer, NoopTextEnhancer, PasteBackend, SilenceTrimmer,
        SttClient, TempStore, TrayPort,
    };

    #[derive(Debug)]
    struct FakeAudioCapture {
        started: Arc<Mutex<bool>>,
    }

    #[derive(Debug)]
    struct OrderedAudioCapture {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[derive(Debug)]
    struct TrackingAudioCapture {
        lifecycle: Arc<RecordingLifecycle>,
    }

    struct FakeRecording;

    #[derive(Debug, Default)]
    struct RecordingLifecycle {
        stop_calls: Mutex<Vec<SessionId>>,
        abort_calls: Mutex<Vec<SessionId>>,
        idle_on_calls: Mutex<usize>,
        idle_off_calls: Mutex<usize>,
        idle_timeout_calls: Mutex<Vec<Duration>>,
        idle_active: Mutex<bool>,
    }

    struct TrackingRecording {
        session_id: SessionId,
        lifecycle: Arc<RecordingLifecycle>,
    }

    #[async_trait]
    impl AudioCapture for FakeAudioCapture {
        async fn start(
            &self,
            session_id: SessionId,
            spec: RecordSpec,
            path: &Utf8Path,
        ) -> Result<Box<dyn ActiveRecording>, AppError> {
            let _ = (session_id, spec, path);
            *self.started.lock().expect("mutex should not be poisoned") = true;
            Ok(Box::new(FakeRecording))
        }

        async fn ensure_idle_stream_on(&self) -> Result<(), AppError> {
            Ok(())
        }

        async fn ensure_idle_stream_off(&self) -> Result<(), AppError> {
            Ok(())
        }

        async fn schedule_idle_stream_timeout(&self, _timeout: Duration) -> Result<(), AppError> {
            Ok(())
        }

        async fn is_idle_stream_active(&self) -> bool {
            false
        }
    }

    #[async_trait]
    impl AudioCapture for OrderedAudioCapture {
        async fn start(
            &self,
            session_id: SessionId,
            spec: RecordSpec,
            path: &Utf8Path,
        ) -> Result<Box<dyn ActiveRecording>, AppError> {
            let _ = (session_id, spec, path);
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("start-recording");
            Ok(Box::new(FakeRecording))
        }

        async fn ensure_idle_stream_on(&self) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("idle-on");
            Ok(())
        }

        async fn ensure_idle_stream_off(&self) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("idle-off");
            Ok(())
        }

        async fn schedule_idle_stream_timeout(&self, _timeout: Duration) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("idle-timeout");
            Ok(())
        }

        async fn is_idle_stream_active(&self) -> bool {
            false
        }
    }

    #[async_trait]
    impl AudioCapture for TrackingAudioCapture {
        async fn start(
            &self,
            session_id: SessionId,
            spec: RecordSpec,
            path: &Utf8Path,
        ) -> Result<Box<dyn ActiveRecording>, AppError> {
            let _ = (spec, path);
            Ok(Box::new(TrackingRecording {
                session_id,
                lifecycle: Arc::clone(&self.lifecycle),
            }))
        }

        async fn ensure_idle_stream_on(&self) -> Result<(), AppError> {
            *self
                .lifecycle
                .idle_on_calls
                .lock()
                .expect("mutex should not be poisoned") += 1;
            *self
                .lifecycle
                .idle_active
                .lock()
                .expect("mutex should not be poisoned") = true;
            Ok(())
        }

        async fn ensure_idle_stream_off(&self) -> Result<(), AppError> {
            *self
                .lifecycle
                .idle_off_calls
                .lock()
                .expect("mutex should not be poisoned") += 1;
            *self
                .lifecycle
                .idle_active
                .lock()
                .expect("mutex should not be poisoned") = false;
            Ok(())
        }

        async fn schedule_idle_stream_timeout(&self, timeout: Duration) -> Result<(), AppError> {
            self.lifecycle
                .idle_timeout_calls
                .lock()
                .expect("mutex should not be poisoned")
                .push(timeout);
            *self
                .lifecycle
                .idle_active
                .lock()
                .expect("mutex should not be poisoned") = true;
            Ok(())
        }

        async fn is_idle_stream_active(&self) -> bool {
            *self
                .lifecycle
                .idle_active
                .lock()
                .expect("mutex should not be poisoned")
        }
    }

    #[async_trait(?Send)]
    impl ActiveRecording for FakeRecording {
        async fn stop(self: Box<Self>) -> Result<CapturedAudio, AppError> {
            Ok(CapturedAudio {
                session_id: SessionId::new(),
                path: Utf8PathBuf::from("/tmp/test.wav"),
                duration_ms: 500,
                sample_rate_hz: 16_000,
                channels: 1,
            })
        }

        async fn abort(self: Box<Self>) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[async_trait(?Send)]
    impl ActiveRecording for TrackingRecording {
        async fn stop(self: Box<Self>) -> Result<CapturedAudio, AppError> {
            self.lifecycle
                .stop_calls
                .lock()
                .expect("mutex should not be poisoned")
                .push(self.session_id);
            Ok(CapturedAudio {
                session_id: self.session_id,
                path: Utf8PathBuf::from("/tmp/test.wav"),
                duration_ms: 500,
                sample_rate_hz: 16_000,
                channels: 1,
            })
        }

        async fn abort(self: Box<Self>) -> Result<(), AppError> {
            self.lifecycle
                .abort_calls
                .lock()
                .expect("mutex should not be poisoned")
                .push(self.session_id);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeTrimmer;

    #[async_trait]
    impl SilenceTrimmer for FakeTrimmer {
        async fn trim(&self, input: &CapturedAudio) -> Result<CapturedAudio, AppError> {
            Ok(input.clone())
        }
    }

    #[derive(Debug, Default)]
    struct FakeCuePlayer;

    #[derive(Debug)]
    struct OrderedCuePlayer {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[derive(Debug)]
    struct TrackingCuePlayer {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl CuePlayer for FakeCuePlayer {
        async fn play_start(&self) -> Result<(), AppError> {
            Ok(())
        }

        async fn play_stop(&self) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[async_trait]
    impl CuePlayer for OrderedCuePlayer {
        async fn play_start(&self) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("play-start-cue");
            Ok(())
        }

        async fn play_stop(&self) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[async_trait]
    impl CuePlayer for TrackingCuePlayer {
        async fn play_start(&self) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("play-start-cue");
            Ok(())
        }

        async fn play_stop(&self) -> Result<(), AppError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push("play-stop-cue");
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeSttClient;

    #[derive(Debug, Default)]
    struct TrackingSttClient {
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl SttClient for FakeSttClient {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn transcribe(&self, _audio: &CapturedAudio) -> Result<String, AppError> {
            Ok("hello".into())
        }
    }

    #[async_trait]
    impl SttClient for TrackingSttClient {
        fn provider_name(&self) -> &'static str {
            "tracking"
        }

        async fn transcribe(&self, _audio: &CapturedAudio) -> Result<String, AppError> {
            let mut calls = self.calls.lock().expect("mutex should not be poisoned");
            *calls += 1;
            Ok("hello".into())
        }
    }

    #[derive(Debug, Default)]
    struct FakeClipboard;

    #[async_trait]
    impl ClipboardWriter for FakeClipboard {
        async fn set_text(&self, _text: &str) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakePaste;

    #[async_trait]
    impl PasteBackend for FakePaste {
        async fn paste(&self, _mode: glossa_core::PasteMode) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeTempStore;

    #[derive(Debug, Default)]
    struct TrackingTempStore {
        cleaned_sessions: Mutex<Vec<SessionId>>,
        purged_sessions: Mutex<Vec<SessionId>>,
    }

    #[derive(Debug, Default)]
    struct TrackingTray {
        states: Mutex<Vec<TrayState>>,
        mic_stream_states: Mutex<Vec<bool>>,
        errors: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TempStore for FakeTempStore {
        async fn create_recording_path(
            &self,
            session_id: SessionId,
            format: AudioFormat,
        ) -> Result<Utf8PathBuf, AppError> {
            Ok(Utf8PathBuf::from(format!(
                "/tmp/{session_id}.{}",
                format.extension()
            )))
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

    #[async_trait]
    impl TempStore for TrackingTempStore {
        async fn create_recording_path(
            &self,
            session_id: SessionId,
            format: AudioFormat,
        ) -> Result<Utf8PathBuf, AppError> {
            Ok(Utf8PathBuf::from(format!(
                "/tmp/{session_id}.{}",
                format.extension()
            )))
        }

        async fn cleanup_session(&self, session_id: SessionId) -> Result<(), AppError> {
            self.cleaned_sessions
                .lock()
                .expect("mutex should not be poisoned")
                .push(session_id);
            Ok(())
        }

        async fn purge_session(&self, session_id: SessionId) -> Result<(), AppError> {
            self.purged_sessions
                .lock()
                .expect("mutex should not be poisoned")
                .push(session_id);
            Ok(())
        }

        async fn cleanup_stale_files(&self) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[async_trait]
    impl TrayPort for TrackingTray {
        async fn set_state(&self, state: TrayState) -> Result<(), AppError> {
            self.states
                .lock()
                .expect("mutex should not be poisoned")
                .push(state);
            Ok(())
        }

        async fn set_shortcut_description(
            &self,
            _description: Option<&str>,
        ) -> Result<(), AppError> {
            Ok(())
        }

        async fn set_mic_stream_state(&self, active: bool) -> Result<(), AppError> {
            self.mic_stream_states
                .lock()
                .expect("mutex should not be poisoned")
                .push(active);
            Ok(())
        }

        async fn show_error(&self, message: &str) -> Result<(), AppError> {
            self.errors
                .lock()
                .expect("mutex should not be poisoned")
                .push(message.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn actor_should_expose_initial_status() {
        let deps = AppDependencies {
            audio_capture: Arc::new(FakeAudioCapture {
                started: Arc::new(Mutex::new(false)),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let (_actor, handle) = AppActor::new(AppConfig::default(), deps);
        assert_eq!(handle.status().state, AppStateKind::Idle);
    }

    #[tokio::test]
    async fn actor_should_start_recording_before_playing_start_cue() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let deps = AppDependencies {
            audio_capture: Arc::new(OrderedAudioCapture {
                events: Arc::clone(&events),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(OrderedCuePlayer {
                events: Arc::clone(&events),
            }),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        let exit = actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("command should succeed");

        assert_eq!(exit, None);
        assert_eq!(
            *events.lock().expect("mutex should not be poisoned"),
            vec!["idle-off", "start-recording", "play-start-cue"]
        );
    }

    #[tokio::test]
    async fn normal_stop_should_schedule_balanced_keepalive() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("start should succeed");
        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("stop should succeed");

        assert!(actor.recording_deadline.is_none());
        assert_eq!(
            *lifecycle
                .idle_timeout_calls
                .lock()
                .expect("mutex should not be poisoned"),
            vec![Duration::from_secs(60)]
        );
    }

    #[tokio::test]
    async fn recording_should_stop_at_configured_max_duration() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let config = AppConfig {
            audio: glossa_core::AudioConfig {
                max_duration_sec: 1,
                latency_mode: LatencyMode::Off,
                ..glossa_core::AudioConfig::default()
            },
            ..AppConfig::default()
        };
        let (actor, handle) = AppActor::new(config, deps);

        let driver = async {
            handle
                .send(AppCommand::ToggleRecording {
                    origin: CommandOrigin::CliControl,
                })
                .expect("start command should be sent");

            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if !lifecycle
                        .stop_calls
                        .lock()
                        .expect("mutex should not be poisoned")
                        .is_empty()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("recording should stop at its configured maximum duration");

            handle
                .send(AppCommand::Shutdown {
                    origin: CommandOrigin::Internal,
                })
                .expect("shutdown command should be sent");
        };

        let (actor_result, ()) = tokio::join!(actor.run(), driver);
        assert_eq!(
            actor_result.expect("actor should shut down cleanly"),
            ActorExit::Shutdown
        );
        assert_eq!(
            lifecycle
                .stop_calls
                .lock()
                .expect("mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn cancel_should_abort_recording_force_purge_audio_and_return_to_idle() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let cue_events = Arc::new(Mutex::new(Vec::new()));
        let temp_store = Arc::new(TrackingTempStore::default());
        let tray = Arc::new(TrackingTray::default());
        let stt_client = Arc::new(TrackingSttClient::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(TrackingCuePlayer {
                events: Arc::clone(&cue_events),
            }),
            stt_client: stt_client.clone(),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: tray.clone(),
            temp_store: temp_store.clone(),
        };
        let (mut actor, handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("start should succeed");

        let session_id = match actor.state {
            AppState::Recording(glossa_core::RecordingState { session_id }) => session_id,
            ref state => panic!("expected recording state, got {state:?}"),
        };

        let exit = actor
            .handle_command(AppCommand::CancelRecording {
                origin: CommandOrigin::EscapeKey,
            })
            .await
            .expect("cancel should succeed");

        assert_eq!(exit, None);
        assert_eq!(handle.status().state, AppStateKind::Idle);
        assert_eq!(
            *lifecycle
                .stop_calls
                .lock()
                .expect("mutex should not be poisoned"),
            Vec::<SessionId>::new()
        );
        assert_eq!(
            *lifecycle
                .abort_calls
                .lock()
                .expect("mutex should not be poisoned"),
            vec![session_id]
        );
        assert_eq!(
            *stt_client
                .calls
                .lock()
                .expect("mutex should not be poisoned"),
            0
        );
        assert_eq!(
            *temp_store
                .cleaned_sessions
                .lock()
                .expect("mutex should not be poisoned"),
            Vec::<SessionId>::new()
        );
        assert_eq!(
            *temp_store
                .purged_sessions
                .lock()
                .expect("mutex should not be poisoned"),
            vec![session_id]
        );
        assert_eq!(
            *cue_events.lock().expect("mutex should not be poisoned"),
            vec!["play-start-cue", "play-stop-cue"]
        );
        assert_eq!(
            *tray.states.lock().expect("mutex should not be poisoned"),
            vec![TrayState::Recording, TrayState::Idle]
        );
        assert_eq!(
            *lifecycle
                .idle_timeout_calls
                .lock()
                .expect("mutex should not be poisoned"),
            vec![Duration::from_secs(60)]
        );
        assert!(
            tray.errors
                .lock()
                .expect("mutex should not be poisoned")
                .is_empty(),
            "cancel should not show tray errors"
        );
    }

    #[tokio::test]
    async fn shutdown_should_close_idle_keepalive() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::Shutdown {
                origin: CommandOrigin::TrayMenu,
            })
            .await
            .expect("shutdown should succeed");

        assert_eq!(
            *lifecycle
                .idle_off_calls
                .lock()
                .expect("mutex should not be poisoned"),
            1
        );
    }

    #[tokio::test]
    async fn tray_input_stream_commands_should_update_balanced_override_and_status() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let tray = Arc::new(TrackingTray::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: tray.clone(),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::EnableInputStream {
                origin: CommandOrigin::TrayMenu,
            })
            .await
            .expect("enable should succeed");
        actor
            .handle_command(AppCommand::DisableInputStream {
                origin: CommandOrigin::TrayMenu,
            })
            .await
            .expect("disable should succeed");

        assert_eq!(actor.manual_input_stream_override, None);
        assert_eq!(
            *lifecycle
                .idle_on_calls
                .lock()
                .expect("mutex should not be poisoned"),
            1
        );
        assert_eq!(
            *lifecycle
                .idle_off_calls
                .lock()
                .expect("mutex should not be poisoned"),
            1
        );
        assert_eq!(
            *tray
                .mic_stream_states
                .lock()
                .expect("mutex should not be poisoned"),
            vec![true, false]
        );
    }

    #[tokio::test]
    async fn toggle_input_stream_command_should_match_tray_toggle_behavior() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let tray = Arc::new(TrackingTray::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: tray.clone(),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::ToggleInputStream {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("first toggle should enable stream");
        actor
            .handle_command(AppCommand::ToggleInputStream {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("second toggle should disable stream");

        assert_eq!(actor.manual_input_stream_override, None);
        assert_eq!(
            *lifecycle
                .idle_on_calls
                .lock()
                .expect("mutex should not be poisoned"),
            1
        );
        assert_eq!(
            *lifecycle
                .idle_off_calls
                .lock()
                .expect("mutex should not be poisoned"),
            1
        );
        assert_eq!(
            *tray
                .mic_stream_states
                .lock()
                .expect("mutex should not be poisoned"),
            vec![true, false]
        );
    }

    #[tokio::test]
    async fn balanced_tray_off_should_allow_keepalive_after_next_recording() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let (mut actor, _handle) = AppActor::new(AppConfig::default(), deps);

        actor
            .handle_command(AppCommand::DisableInputStream {
                origin: CommandOrigin::TrayMenu,
            })
            .await
            .expect("disable should succeed");
        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("start should succeed");
        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("stop should succeed");

        assert_eq!(actor.manual_input_stream_override, None);
        assert_eq!(
            *lifecycle
                .idle_timeout_calls
                .lock()
                .expect("mutex should not be poisoned"),
            vec![Duration::from_secs(60)]
        );
    }

    #[tokio::test]
    async fn instant_tray_off_should_suppress_automatic_keepalive_for_session() {
        let lifecycle = Arc::new(RecordingLifecycle::default());
        let deps = AppDependencies {
            audio_capture: Arc::new(TrackingAudioCapture {
                lifecycle: Arc::clone(&lifecycle),
            }),
            trimmer: Arc::new(FakeTrimmer),
            cue_player: Arc::new(FakeCuePlayer),
            stt_client: Arc::new(FakeSttClient),
            text_enhancer: Arc::new(NoopTextEnhancer),
            clipboard: Arc::new(FakeClipboard),
            paste: Arc::new(FakePaste),
            tray: Arc::new(crate::ports::NullTrayPort),
            temp_store: Arc::new(FakeTempStore),
        };
        let config = AppConfig {
            audio: glossa_core::AudioConfig {
                latency_mode: LatencyMode::Instant,
                ..glossa_core::AudioConfig::default()
            },
            ..AppConfig::default()
        };
        let (mut actor, _handle) = AppActor::new(config, deps);

        actor
            .handle_command(AppCommand::DisableInputStream {
                origin: CommandOrigin::TrayMenu,
            })
            .await
            .expect("disable should succeed");
        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("start should succeed");
        actor
            .handle_command(AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            })
            .await
            .expect("stop should succeed");

        assert_eq!(actor.manual_input_stream_override, Some(false));
        assert_eq!(
            *lifecycle
                .idle_on_calls
                .lock()
                .expect("mutex should not be poisoned"),
            0
        );
        assert!(lifecycle
            .idle_timeout_calls
            .lock()
            .expect("mutex should not be poisoned")
            .is_empty());
    }
}
