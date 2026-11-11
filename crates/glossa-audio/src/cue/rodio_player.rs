use std::{
    any::Any,
    fmt::Display,
    fs::File,
    io::BufReader,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use camino::Utf8PathBuf;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SampleFormat, SizedSample,
};
use rodio::{
    dynamic_mixer::{self, DynamicMixer, DynamicMixerController},
    Decoder, Sink, Source,
};
use tokio::sync::oneshot;

use glossa_app::{ports::CuePlayer, AppError};

const REOPEN_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const PLAYBACK_DEADLINE_SLACK: Duration = Duration::from_secs(1);
const PLAYBACK_DEADLINE_UNKNOWN_LENGTH: Duration = Duration::from_secs(5);
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(5);

type OpenAudioFn = dyn FnMut(StreamFailureReporter) -> Result<CueAudio, AppError> + Send;

/// Cue player backed by Rodio for start/stop sound effects.
#[derive(Debug, Clone)]
pub struct RodioCuePlayer {
    start_sound: Utf8PathBuf,
    stop_sound: Utf8PathBuf,
    request_tx: mpsc::Sender<WorkerMessage>,
    _shutdown: Arc<ShutdownOnDrop>,
}

// why: open streams hold wake senders, so the channel never disconnects on its own
// and the worker would outlive the player across daemon restarts.
#[derive(Debug)]
struct ShutdownOnDrop(mpsc::Sender<WorkerMessage>);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(WorkerMessage::Shutdown);
    }
}

#[derive(Debug)]
struct PlayRequest {
    cue: &'static str,
    path: Utf8PathBuf,
    done_tx: oneshot::Sender<()>,
}

#[derive(Debug)]
enum WorkerMessage {
    Play(PlayRequest),
    StreamFailed(String),
    Shutdown,
}

struct CueAudio {
    _stream: Box<dyn Any>,
    mixer: Arc<DynamicMixerController<f32>>,
}

struct OpenStream {
    audio: CueAudio,
    failed: Arc<AtomicBool>,
}

#[derive(Clone)]
struct StreamFailureReporter {
    failed: Arc<AtomicBool>,
    wake_tx: mpsc::Sender<WorkerMessage>,
}

impl StreamFailureReporter {
    // why: cpal 0.15 ALSA calls the error callback in a tight loop after POLLERR,
    // so only the first error per stream may reach the worker.
    fn report(&self, error: &dyn Display) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            let _ = self
                .wake_tx
                .send(WorkerMessage::StreamFailed(error.to_string()));
        }
    }
}

impl RodioCuePlayer {
    #[must_use]
    pub fn new(start_sound: Utf8PathBuf, stop_sound: Utf8PathBuf) -> Self {
        Self::from_open_audio(start_sound, stop_sound, Box::new(open_default_output))
    }

    #[cfg(test)]
    fn new_with_open_audio<F>(
        start_sound: Utf8PathBuf,
        stop_sound: Utf8PathBuf,
        open_audio: F,
    ) -> Self
    where
        F: FnMut(StreamFailureReporter) -> Result<CueAudio, AppError> + Send + 'static,
    {
        Self::from_open_audio(start_sound, stop_sound, Box::new(open_audio))
    }

    fn from_open_audio(
        start_sound: Utf8PathBuf,
        stop_sound: Utf8PathBuf,
        open_audio: Box<OpenAudioFn>,
    ) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<WorkerMessage>();
        let wake_tx = request_tx.clone();
        thread::spawn(move || {
            CueWorker {
                open_audio,
                wake_tx,
                stream: None,
                backoff: REOPEN_BACKOFF_INITIAL,
                failures_since_healthy: 0,
            }
            .run(&request_rx);
        });

        Self {
            start_sound,
            stop_sound,
            _shutdown: Arc::new(ShutdownOnDrop(request_tx.clone())),
            request_tx,
        }
    }

    async fn play(&self, cue: &'static str, path: Utf8PathBuf) -> Result<(), AppError> {
        let (done_tx, done_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerMessage::Play(PlayRequest { cue, path, done_tx }))
            .map_err(|_| AppError::message("cue audio worker is unavailable"))?;
        done_rx
            .await
            .map_err(|_| AppError::message("cue audio worker stopped before playback completed"))?;
        Ok(())
    }
}

#[async_trait]
impl CuePlayer for RodioCuePlayer {
    async fn play_start(&self) -> Result<(), AppError> {
        self.play("start", self.start_sound.clone()).await
    }

    async fn play_stop(&self) -> Result<(), AppError> {
        self.play("stop", self.stop_sound.clone()).await
    }
}

struct CueWorker {
    open_audio: Box<OpenAudioFn>,
    wake_tx: mpsc::Sender<WorkerMessage>,
    stream: Option<OpenStream>,
    backoff: Duration,
    failures_since_healthy: u32,
}

impl CueWorker {
    fn run(mut self, request_rx: &mpsc::Receiver<WorkerMessage>) {
        if let Err(error) = self.open() {
            tracing::warn!(
                error = %error,
                "cue audio unavailable at startup; cue playback will retry later"
            );
            self.failures_since_healthy = 1;
        }

        loop {
            let message = if self.stream.is_some() {
                request_rx.recv().ok()
            } else {
                match request_rx.recv_timeout(self.backoff) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => {
                        self.retry_open();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => None,
                }
            };
            let Some(message) = message else {
                break;
            };

            match message {
                WorkerMessage::Play(request) => {
                    self.play_with_reopen_once(request.cue, &request.path);
                    let _ = request.done_tx.send(());
                }
                WorkerMessage::StreamFailed(error) => self.close_failed_stream(&error),
                WorkerMessage::Shutdown => break,
            }
        }
    }

    fn open(&mut self) -> Result<(), AppError> {
        self.stream = None;
        let failed = Arc::new(AtomicBool::new(false));
        let audio = (self.open_audio)(StreamFailureReporter {
            failed: Arc::clone(&failed),
            wake_tx: self.wake_tx.clone(),
        })?;
        self.stream = Some(OpenStream { audio, failed });
        Ok(())
    }

    fn retry_open(&mut self) {
        match self.open() {
            Ok(()) => tracing::debug!("cue audio output stream reopened"),
            Err(error) => {
                tracing::debug!(error = %error, "cue audio output stream reopen failed");
                self.record_failure();
            }
        }
    }

    fn close_failed_stream(&mut self, error: &str) {
        let current_failed = self
            .stream
            .as_ref()
            .is_some_and(|stream| stream.failed.load(Ordering::Acquire));
        if !current_failed {
            return;
        }
        self.stream = None;
        if self.failures_since_healthy == 0 {
            tracing::warn!(
                error,
                "cue audio output stream failed; closing it and reopening in the background"
            );
        } else {
            tracing::debug!(error, "cue audio output stream failed again");
        }
        self.record_failure();
    }

    fn record_failure(&mut self) {
        self.failures_since_healthy += 1;
        self.backoff = (self.backoff * 2).min(REOPEN_BACKOFF_MAX);
    }

    fn mark_healthy(&mut self) {
        if self.failures_since_healthy > 0 {
            tracing::info!(
                failures = self.failures_since_healthy,
                "cue audio output recovered"
            );
        }
        self.failures_since_healthy = 0;
        self.backoff = REOPEN_BACKOFF_INITIAL;
    }

    fn play_with_reopen_once(&mut self, cue: &'static str, path: &Utf8PathBuf) {
        if let Err(first_error) = self.play_file_once(cue, path) {
            tracing::warn!(
                cue,
                path = %path,
                error = %first_error,
                "cue playback failed; reopening audio output stream and retrying once"
            );
            if let Err(error) = self.open() {
                tracing::warn!(error = %error, "failed to reopen cue audio output stream");
            }

            if let Err(second_error) = self.play_file_once(cue, path) {
                tracing::warn!(
                    cue,
                    path = %path,
                    error = %second_error,
                    "cue playback failed after reopening audio output stream; skipping cue"
                );
                return;
            }
        }
        self.mark_healthy();
    }

    fn play_file_once(&self, cue: &'static str, path: &Utf8PathBuf) -> Result<(), AppError> {
        let playback_started_at = Instant::now();
        let file_open_started_at = Instant::now();
        let file = File::open(path.as_std_path())
            .map_err(|error| AppError::io("failed to open cue sound file", error))?;
        let file_open_ms = file_open_started_at.elapsed().as_millis();
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AppError::message("cue audio output stream is unavailable"))?;
        let sink_started_at = Instant::now();
        let (sink, queue) = Sink::new_idle();
        stream.audio.mixer.add(queue);
        let sink_create_ms = sink_started_at.elapsed().as_millis();
        let decode_started_at = Instant::now();
        let source = Decoder::new(BufReader::new(file))
            .map_err(|error| AppError::message(format!("failed to decode cue sound: {error}")))?;
        let decode_ms = decode_started_at.elapsed().as_millis();
        let deadline = Instant::now()
            + source
                .total_duration()
                .unwrap_or(PLAYBACK_DEADLINE_UNKNOWN_LENGTH)
            + PLAYBACK_DEADLINE_SLACK;
        sink.append(source);
        tracing::info!(
            cue,
            path = %path,
            file_open_ms,
            sink_create_ms,
            decode_ms,
            enqueue_ms = playback_started_at.elapsed().as_millis(),
            "cue playback enqueued"
        );
        // why: a dead output stream stops pulling samples, so an unbounded wait
        // would block the actor, which awaits the stop cue before stopping capture.
        while !sink.empty() {
            if stream.failed.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(AppError::message(
                    "cue audio output stream stopped playing before the cue finished",
                ));
            }
            thread::sleep(PLAYBACK_POLL_INTERVAL);
        }
        tracing::info!(
            cue,
            path = %path,
            total_playback_ms = playback_started_at.elapsed().as_millis(),
            "cue playback finished"
        );
        Ok(())
    }
}

fn open_default_output(reporter: StreamFailureReporter) -> Result<CueAudio, AppError> {
    let started_at = Instant::now();
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| AppError::message("no default audio output device"))?;
    let supported = device.default_output_config().map_err(|error| {
        AppError::message(format!(
            "failed to read default audio output config: {error}"
        ))
    })?;
    let (mixer, source) =
        dynamic_mixer::mixer::<f32>(supported.channels(), supported.sample_rate().0);
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&device, &config, source, reporter),
        SampleFormat::F64 => build_output::<f64>(&device, &config, source, reporter),
        SampleFormat::I8 => build_output::<i8>(&device, &config, source, reporter),
        SampleFormat::I16 => build_output::<i16>(&device, &config, source, reporter),
        SampleFormat::I32 => build_output::<i32>(&device, &config, source, reporter),
        SampleFormat::I64 => build_output::<i64>(&device, &config, source, reporter),
        SampleFormat::U8 => build_output::<u8>(&device, &config, source, reporter),
        SampleFormat::U16 => build_output::<u16>(&device, &config, source, reporter),
        SampleFormat::U32 => build_output::<u32>(&device, &config, source, reporter),
        SampleFormat::U64 => build_output::<u64>(&device, &config, source, reporter),
        other => Err(AppError::message(format!(
            "unsupported audio output sample format: {other}"
        ))),
    }?;
    stream.play().map_err(|error| {
        AppError::message(format!("failed to start audio output stream: {error}"))
    })?;
    tracing::info!(
        audio_init_ms = started_at.elapsed().as_millis(),
        "cue audio output stream initialized"
    );

    Ok(CueAudio {
        _stream: Box::new(stream),
        mixer,
    })
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut source: DynamicMixer<f32>,
    reporter: StreamFailureReporter,
) -> Result<cpal::Stream, AppError>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for sample in data.iter_mut() {
                    *sample = source
                        .next()
                        .map_or(T::EQUILIBRIUM, |value| T::from_sample(value));
                }
            },
            move |error| reporter.report(&error),
            None,
        )
        .map_err(|error| AppError::message(format!("failed to open audio output stream: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use super::*;
    use crate::cue::CuePlayerBackend;

    fn start_sound() -> Utf8PathBuf {
        Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contrib/assets/sounds/start.wav")
    }

    fn silent_audio() -> CueAudio {
        let (mixer, _undrained_output) = dynamic_mixer::mixer::<f32>(2, 48_000);
        CueAudio {
            _stream: Box::new(()),
            mixer,
        }
    }

    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn draining_audio() -> CueAudio {
        let (mixer, mut output) = dynamic_mixer::mixer::<f32>(2, 48_000);
        let stopped = Arc::new(AtomicBool::new(false));
        thread::spawn({
            let stopped = Arc::clone(&stopped);
            move || {
                while !stopped.load(Ordering::Acquire) {
                    for _ in 0..9_600 {
                        let _ = output.next();
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        });
        CueAudio {
            _stream: Box::new(SetOnDrop(stopped)),
            mixer,
        }
    }

    fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    #[tokio::test]
    async fn disabled_backend_should_noop() {
        let backend = CuePlayerBackend::from_config(
            false,
            Utf8PathBuf::from("/tmp/start.wav"),
            Utf8PathBuf::from("/tmp/stop.wav"),
        );

        backend
            .play_start()
            .await
            .expect("disabled backend should ignore start cue");
        backend
            .play_stop()
            .await
            .expect("disabled backend should ignore stop cue");

        assert!(matches!(backend, CuePlayerBackend::Disabled));
    }

    #[tokio::test]
    async fn play_start_should_retry_audio_reopen_once_and_still_succeed() {
        let open_calls = Arc::new(AtomicUsize::new(0));
        let player = RodioCuePlayer::new_with_open_audio(
            Utf8PathBuf::from("/tmp/missing-start.wav"),
            Utf8PathBuf::from("/tmp/missing-stop.wav"),
            {
                let open_calls = Arc::clone(&open_calls);
                move |_| {
                    open_calls.fetch_add(1, Ordering::SeqCst);
                    Err(AppError::message("audio unavailable"))
                }
            },
        );

        player
            .play_start()
            .await
            .expect("cue playback failure should stay best-effort");

        assert_eq!(open_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn play_start_should_return_when_output_stream_stops_pulling_samples() {
        let player = RodioCuePlayer::new_with_open_audio(start_sound(), start_sound(), |_| {
            Ok(silent_audio())
        });

        tokio::time::timeout(Duration::from_secs(10), player.play_start())
            .await
            .expect("cue playback on a dead stream must not hang the actor")
            .expect("cue playback failure should stay best-effort");
    }

    #[test]
    fn stream_error_burst_should_reopen_the_stream_once() {
        let reporters = Arc::new(Mutex::new(Vec::<StreamFailureReporter>::new()));
        let _player = RodioCuePlayer::new_with_open_audio(start_sound(), start_sound(), {
            let reporters = Arc::clone(&reporters);
            move |reporter| {
                reporters.lock().expect("reporters lock").push(reporter);
                Ok(silent_audio())
            }
        });
        let opens = || reporters.lock().expect("reporters lock").len();
        assert!(wait_until(Duration::from_secs(2), || opens() == 1));

        let first_stream = reporters.lock().expect("reporters lock")[0].clone();
        for _ in 0..10_000 {
            first_stream.report(&"`alsa::poll()` returned POLLERR");
        }

        assert!(wait_until(Duration::from_secs(3), || opens() == 2));
        thread::sleep(REOPEN_BACKOFF_INITIAL * 3);
        assert_eq!(opens(), 2);
    }

    #[test]
    fn stream_error_burst_should_wake_the_worker_once() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let reporter = StreamFailureReporter {
            failed: Arc::new(AtomicBool::new(false)),
            wake_tx,
        };

        for _ in 0..10_000 {
            reporter.report(&"`alsa::poll()` returned POLLERR");
        }

        assert_eq!(wake_rx.try_iter().count(), 1);
    }

    #[tokio::test]
    async fn play_start_on_healthy_stream_should_finish_without_reopening() {
        let open_calls = Arc::new(AtomicUsize::new(0));
        let player = RodioCuePlayer::new_with_open_audio(start_sound(), start_sound(), {
            let open_calls = Arc::clone(&open_calls);
            move |_| {
                open_calls.fetch_add(1, Ordering::SeqCst);
                Ok(draining_audio())
            }
        });

        let started_at = Instant::now();
        player.play_start().await.expect("cue should play");

        assert!(started_at.elapsed() < Duration::from_secs(1));
        assert_eq!(open_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stream_failing_right_after_open_should_back_off() {
        let open_calls = Arc::new(AtomicUsize::new(0));
        let _player = RodioCuePlayer::new_with_open_audio(start_sound(), start_sound(), {
            let open_calls = Arc::clone(&open_calls);
            move |reporter| {
                open_calls.fetch_add(1, Ordering::SeqCst);
                reporter.report(&"`alsa::poll()` returned POLLERR");
                Ok(silent_audio())
            }
        });

        thread::sleep(Duration::from_millis(3_500));

        assert!(open_calls.load(Ordering::SeqCst) <= 4);
    }

    #[test]
    fn dropping_the_player_should_close_the_output_stream() {
        let stream_closed = Arc::new(AtomicBool::new(false));
        let player = RodioCuePlayer::new_with_open_audio(start_sound(), start_sound(), {
            let stream_closed = Arc::clone(&stream_closed);
            move |_| {
                let (mixer, _output) = dynamic_mixer::mixer::<f32>(2, 48_000);
                Ok(CueAudio {
                    _stream: Box::new(SetOnDrop(Arc::clone(&stream_closed))),
                    mixer,
                })
            }
        });
        let player_clone = player.clone();

        drop(player);
        drop(player_clone);

        assert!(wait_until(Duration::from_secs(2), || stream_closed
            .load(Ordering::Acquire)));
    }
}
