use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use camino::Utf8Path;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, SampleRate, StreamConfig,
};

use glossa_app::{ports::AudioCapture, AppError};
use glossa_core::{RecordSpec, SessionId};

use super::{active_recording::CpalActiveRecording, sample_convert};

type WriterHandle = thread::JoinHandle<Result<(), String>>;

const FIRST_INPUT_BUFFER_TIMEOUT: Duration = Duration::from_secs(2);
const KEEPALIVE_FIRST_INPUT_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_STARTUP_TIMEOUT: Duration = Duration::from_secs(6);
const LOW_LATENCY_BUFFER_FRAMES: [u32; 2] = [512, 1024];
const KEEPALIVE_REOPEN_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const KEEPALIVE_REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const KEEPALIVE_HEALTHY_AFTER: Duration = Duration::from_secs(30);

/// Audio capture backed by the system default input device through CPAL.
#[derive(Clone)]
pub struct CpalAudioCapture {
    idle_keepalive: Arc<IdleKeepaliveController>,
}

impl CpalAudioCapture {
    #[must_use]
    pub fn new() -> Self {
        Self {
            idle_keepalive: Arc::new(IdleKeepaliveController::default()),
        }
    }
}

impl Default for CpalAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CpalAudioCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpalAudioCapture")
            .field("idle_keepalive_active", &self.idle_keepalive.is_active())
            .finish()
    }
}

#[async_trait]
impl AudioCapture for CpalAudioCapture {
    async fn start(
        &self,
        session_id: SessionId,
        spec: RecordSpec,
        path: &Utf8Path,
    ) -> Result<Box<dyn glossa_app::ports::ActiveRecording>, AppError> {
        self.idle_keepalive.ensure_off();
        let startup_started_at = Instant::now();
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AppError::message("no default input device is available"))?;
        let default_config = device
            .default_input_config()
            .map_err(|error| AppError::message(format!("failed to read input config: {error}")))?;
        let fallback_config = default_config.config();
        let requested_config = StreamConfig {
            channels: spec.channels,
            sample_rate: SampleRate(spec.sample_rate_hz),
            buffer_size: cpal::BufferSize::Default,
        };

        let writer_started_at = Instant::now();
        let (writer_tx, writer_handle, writer_config) = start_writer(path, &requested_config);
        let writer_setup_ms = writer_started_at.elapsed().as_millis();
        let (first_input_tx, first_input_rx) = mpsc::sync_channel(1);
        let first_input_notifier = FirstInputBufferNotifier::new(first_input_tx);
        let stream_build_started_at = Instant::now();
        let ((stream, stream_config), used_fallback_config) = match build_preferred_stream(
            &device,
            default_config.sample_format(),
            &requested_config,
            writer_tx.clone(),
            first_input_notifier.clone(),
        ) {
            Ok(stream) => (stream, false),
            Err(error) => {
                tracing::info!(
                    %session_id,
                    requested_sample_rate_hz = requested_config.sample_rate.0,
                    requested_channels = requested_config.channels,
                    fallback_sample_rate_hz = fallback_config.sample_rate.0,
                    fallback_channels = fallback_config.channels,
                    error = %error,
                    "requested input configs failed, retrying with default input config"
                );
                (
                    build_preferred_stream(
                        &device,
                        default_config.sample_format(),
                        &fallback_config,
                        writer_tx.clone(),
                        first_input_notifier,
                    )
                    .map_err(AppError::message)?,
                    true,
                )
            }
        };
        let stream_build_ms = stream_build_started_at.elapsed().as_millis();
        let play_started_at = Instant::now();
        stream
            .play()
            .map_err(|error| AppError::message(format!("failed to start audio stream: {error}")))?;
        let stream_play_ms = play_started_at.elapsed().as_millis();
        // `cpal::Stream` is not Send, so the readiness wait cannot cross an await.
        let first_input_buffer_ms = match wait_for_first_input_buffer(&first_input_rx) {
            Ok(ms) => ms,
            Err(error) => {
                let _ = stream.pause();
                drop(stream);
                drop(writer_tx);
                match writer_handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(writer_error)) => {
                        tracing::warn!(
                            error = %writer_error,
                            "audio writer failed while cleaning up capture startup"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            "audio writer thread panicked while cleaning up capture startup"
                        );
                    }
                }
                return Err(error);
            }
        };
        tracing::info!(
            %session_id,
            requested_sample_rate_hz = spec.sample_rate_hz,
            requested_channels = spec.channels,
            writer_sample_rate_hz = writer_config.sample_rate.0,
            writer_channels = writer_config.channels,
            input_sample_rate_hz = stream_config.sample_rate.0,
            input_channels = stream_config.channels,
            input_buffer_size = %buffer_size_label(&stream_config.buffer_size),
            fallback_sample_rate_hz = fallback_config.sample_rate.0,
            fallback_channels = fallback_config.channels,
            used_fallback_config,
            writer_setup_ms,
            stream_build_ms,
            stream_play_ms,
            first_input_buffer_ms,
            total_startup_ms = startup_started_at.elapsed().as_millis(),
            "audio capture initialized"
        );

        Ok(Box::new(CpalActiveRecording {
            session_id,
            path: path.to_owned(),
            sample_rate_hz: writer_config.sample_rate.0,
            channels: writer_config.channels,
            started_at: Instant::now(),
            stream,
            tx: Some(writer_tx),
            writer_handle: Some(writer_handle),
        }))
    }

    async fn ensure_idle_stream_on(&self) -> Result<(), AppError> {
        self.idle_keepalive.ensure_on()
    }

    async fn ensure_idle_stream_off(&self) -> Result<(), AppError> {
        self.idle_keepalive.ensure_off();
        Ok(())
    }

    async fn schedule_idle_stream_timeout(&self, timeout: Duration) -> Result<(), AppError> {
        Arc::clone(&self.idle_keepalive).schedule_timeout(timeout)
    }

    async fn is_idle_stream_active(&self) -> bool {
        self.idle_keepalive.is_active()
    }
}

fn start_writer(
    path: &Utf8Path,
    config: &StreamConfig,
) -> (SyncSender<Vec<i16>>, WriterHandle, StreamConfig) {
    let wav_path = path.to_path_buf().into_std_path_buf();
    let wav_spec = hound::WavSpec {
        channels: config.channels,
        sample_rate: config.sample_rate.0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(8);
    let handle = thread::spawn(move || -> Result<(), String> {
        let mut writer = hound::WavWriter::create(wav_path, wav_spec)
            .map_err(|error| format!("failed to create wav writer: {error}"))?;
        while let Ok(chunk) = rx.recv() {
            for sample in chunk {
                writer
                    .write_sample(sample)
                    .map_err(|error| format!("failed to write wav sample: {error}"))?;
            }
        }
        writer
            .finalize()
            .map_err(|error| format!("failed to finalize wav writer: {error}"))?;
        Ok(())
    });
    (tx, handle, config.clone())
}

fn build_preferred_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    base_config: &StreamConfig,
    tx: SyncSender<Vec<i16>>,
    first_input_notifier: FirstInputBufferNotifier,
) -> Result<(cpal::Stream, StreamConfig), String> {
    let mut errors = Vec::new();

    for config in input_config_candidates(base_config) {
        match build_stream(
            device,
            sample_format,
            &config,
            tx.clone(),
            first_input_notifier.clone(),
        ) {
            Ok(stream) => return Ok((stream, config)),
            Err(error) => {
                tracing::debug!(
                    sample_rate_hz = config.sample_rate.0,
                    channels = config.channels,
                    buffer_size = %buffer_size_label(&config.buffer_size),
                    error = %error,
                    "input stream config candidate failed"
                );
                errors.push(format!(
                    "{} Hz/{} ch/{} buffer: {}",
                    config.sample_rate.0,
                    config.channels,
                    buffer_size_label(&config.buffer_size),
                    error
                ));
            }
        }
    }

    Err(format!(
        "failed to build input stream with preferred configs: {}",
        errors.join("; ")
    ))
}

fn build_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    tx: SyncSender<Vec<i16>>,
    first_input_notifier: FirstInputBufferNotifier,
) -> Result<cpal::Stream, String> {
    let error_callback = first_error_only(|error: cpal::StreamError| {
        tracing::error!(
            error = %error,
            "audio stream error; later errors on this stream are suppressed"
        );
    });

    match sample_format {
        SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _| {
                    send_converted(
                        data.iter().copied().map(sample_convert::f32_to_i16),
                        &tx,
                        &first_input_notifier,
                    )
                },
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build f32 input stream: {error}")),
        SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _| {
                    send_converted(data.iter().copied(), &tx, &first_input_notifier)
                },
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build i16 input stream: {error}")),
        SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _| {
                    send_converted(
                        data.iter().copied().map(sample_convert::u16_to_i16),
                        &tx,
                        &first_input_notifier,
                    )
                },
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build u16 input stream: {error}")),
        _ => Err("unsupported input sample format".into()),
    }
}

fn input_config_candidates(base_config: &StreamConfig) -> Vec<StreamConfig> {
    let mut candidates = Vec::with_capacity(LOW_LATENCY_BUFFER_FRAMES.len() + 1);

    for buffer_frames in LOW_LATENCY_BUFFER_FRAMES {
        let mut config = base_config.clone();
        config.buffer_size = cpal::BufferSize::Fixed(buffer_frames);
        candidates.push(config);
    }

    let mut default_config = base_config.clone();
    default_config.buffer_size = cpal::BufferSize::Default;
    candidates.push(default_config);

    candidates
}

struct IdleKeepaliveController {
    stream: Mutex<Option<Arc<dyn IdleInputStream>>>,
    timeout_generation: AtomicU64,
    stream_factory: Arc<IdleInputStreamFactory>,
}

type IdleInputStreamFactory = dyn Fn() -> Result<Arc<dyn IdleInputStream>, AppError> + Send + Sync;

trait IdleInputStream: Send + Sync {}

impl IdleInputStream for InputKeepalive {}

impl Default for IdleKeepaliveController {
    fn default() -> Self {
        Self {
            stream: Mutex::new(None),
            timeout_generation: AtomicU64::new(0),
            stream_factory: Arc::new(spawn_input_keepalive),
        }
    }
}

impl IdleKeepaliveController {
    #[cfg(test)]
    fn with_stream_factory(stream_factory: Arc<IdleInputStreamFactory>) -> Self {
        Self {
            stream: Mutex::new(None),
            timeout_generation: AtomicU64::new(0),
            stream_factory,
        }
    }

    fn ensure_on(&self) -> Result<(), AppError> {
        self.cancel_timeout();
        self.start_if_needed()
    }

    fn ensure_off(&self) {
        self.cancel_timeout();
        self.stop_current();
    }

    fn schedule_timeout(self: Arc<Self>, timeout: Duration) -> Result<(), AppError> {
        self.start_if_needed()?;
        let timeout_generation = self.timeout_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let controller = Arc::clone(&self);
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if controller.timeout_generation.load(Ordering::Acquire) == timeout_generation {
                controller.stop_current();
            }
        });
        Ok(())
    }

    fn cancel_timeout(&self) {
        self.timeout_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn is_active(&self) -> bool {
        self.stream
            .lock()
            .map(|stream| stream.is_some())
            .unwrap_or(false)
    }

    fn start_if_needed(&self) -> Result<(), AppError> {
        if self.is_active() {
            return Ok(());
        }

        let keepalive = (self.stream_factory)()?;
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| AppError::message("audio input keepalive mutex is poisoned"))?;
        if stream.is_none() {
            *stream = Some(keepalive);
        }
        Ok(())
    }

    fn stop_current(&self) {
        if let Ok(mut stream) = self.stream.lock() {
            stream.take();
        }
    }
}

fn spawn_input_keepalive() -> Result<Arc<dyn IdleInputStream>, AppError> {
    let stream: Arc<dyn IdleInputStream> =
        Arc::new(InputKeepalive::spawn().map_err(AppError::message)?);
    Ok(stream)
}

#[derive(Debug)]
enum KeepaliveMessage {
    StreamFailed { generation: u64, error: String },
    Shutdown,
}

struct InputKeepalive {
    shutdown_tx: Mutex<Option<mpsc::Sender<KeepaliveMessage>>>,
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl InputKeepalive {
    fn spawn() -> Result<Self, String> {
        let (shutdown_tx, messages_rx) = mpsc::channel();
        let failure_tx = shutdown_tx.clone();
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let thread_handle =
            thread::spawn(move || run_input_keepalive(&messages_rx, failure_tx, startup_tx));

        match startup_rx.recv_timeout(KEEPALIVE_STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                shutdown_tx: Mutex::new(Some(shutdown_tx)),
                thread_handle: Mutex::new(Some(thread_handle)),
            }),
            Ok(Err(error)) => {
                let _ = thread_handle.join();
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = shutdown_tx.send(KeepaliveMessage::Shutdown);
                Err(format!(
                    "audio input keepalive stream did not start within {} ms",
                    KEEPALIVE_STARTUP_TIMEOUT.as_millis()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread_handle.join();
                Err("audio input keepalive stream stopped before startup completed".into())
            }
        }
    }
}

impl Drop for InputKeepalive {
    fn drop(&mut self) {
        if let Ok(mut shutdown_tx) = self.shutdown_tx.lock() {
            if let Some(shutdown_tx) = shutdown_tx.take() {
                let _ = shutdown_tx.send(KeepaliveMessage::Shutdown);
            }
        }

        if let Ok(mut thread_handle) = self.thread_handle.lock() {
            if let Some(thread_handle) = thread_handle.take() {
                let _ = thread_handle.join();
            }
        }
    }
}

struct InputKeepaliveStream {
    _stream: cpal::Stream,
    device_name: String,
    config: StreamConfig,
    first_input_rx: mpsc::Receiver<()>,
}

fn run_input_keepalive(
    messages_rx: &mpsc::Receiver<KeepaliveMessage>,
    failure_tx: mpsc::Sender<KeepaliveMessage>,
    startup_tx: SyncSender<Result<(), String>>,
) {
    let startup_started_at = Instant::now();
    let opened = open_input_keepalive_stream(&failure_tx, 0).and_then(|stream| {
        let first_input_buffer_ms = wait_for_first_input_buffer_with_timeout(
            &stream.first_input_rx,
            KEEPALIVE_FIRST_INPUT_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
        Ok((stream, first_input_buffer_ms))
    });
    let (stream, first_input_buffer_ms) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            return;
        }
    };
    tracing::info!(
        device = %stream.device_name,
        sample_rate_hz = stream.config.sample_rate.0,
        channels = stream.config.channels,
        buffer_size = %buffer_size_label(&stream.config.buffer_size),
        first_input_buffer_ms,
        startup_ms = startup_started_at.elapsed().as_millis(),
        "audio input keepalive stream initialized"
    );
    let _ = startup_tx.send(Ok(()));

    run_keepalive_loop(messages_rx, stream, |generation| {
        open_input_keepalive_stream(&failure_tx, generation)
    });
}

fn run_keepalive_loop<S>(
    messages_rx: &mpsc::Receiver<KeepaliveMessage>,
    initial_stream: S,
    mut open: impl FnMut(u64) -> Result<S, String>,
) {
    let mut stream = Some(initial_stream);
    let mut generation = 0_u64;
    let mut backoff = KEEPALIVE_REOPEN_BACKOFF_INITIAL;
    let mut failures = 0_u32;

    loop {
        let wait = if stream.is_none() {
            Some(backoff)
        } else if failures > 0 {
            Some(KEEPALIVE_HEALTHY_AFTER)
        } else {
            None
        };
        let message = match wait {
            None => messages_rx.recv().unwrap_or(KeepaliveMessage::Shutdown),
            Some(timeout) => match messages_rx.recv_timeout(timeout) {
                Ok(message) => message,
                Err(RecvTimeoutError::Disconnected) => KeepaliveMessage::Shutdown,
                Err(RecvTimeoutError::Timeout) if stream.is_some() => {
                    tracing::info!(failures, "audio input keepalive stream recovered");
                    failures = 0;
                    backoff = KEEPALIVE_REOPEN_BACKOFF_INITIAL;
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {
                    generation += 1;
                    match open(generation) {
                        Ok(reopened) => {
                            tracing::debug!("audio input keepalive stream reopened");
                            stream = Some(reopened);
                        }
                        Err(error) => {
                            tracing::debug!(error, "audio input keepalive stream reopen failed");
                            failures += 1;
                            backoff = (backoff * 2).min(KEEPALIVE_REOPEN_BACKOFF_MAX);
                        }
                    }
                    continue;
                }
            },
        };

        match message {
            KeepaliveMessage::Shutdown => break,
            KeepaliveMessage::StreamFailed {
                generation: failed_generation,
                error,
            } => {
                if failed_generation != generation || stream.is_none() {
                    continue;
                }
                stream = None;
                if failures == 0 {
                    tracing::warn!(
                        error,
                        "audio input keepalive stream failed; closing it and reopening in the background"
                    );
                } else {
                    tracing::debug!(error, "audio input keepalive stream failed again");
                }
                failures += 1;
                backoff = (backoff * 2).min(KEEPALIVE_REOPEN_BACKOFF_MAX);
            }
        }
    }
}

fn open_input_keepalive_stream(
    failure_tx: &mpsc::Sender<KeepaliveMessage>,
    generation: u64,
) -> Result<InputKeepaliveStream, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no default input device is available".to_string())?;
    let device_name = device
        .name()
        .unwrap_or_else(|error| format!("unknown input device ({error})"));
    let default_config = device
        .default_input_config()
        .map_err(|error| format!("failed to read input config: {error}"))?;
    let (first_input_tx, first_input_rx) = mpsc::sync_channel(1);
    let first_input_notifier = FirstInputBufferNotifier::new(first_input_tx);
    let (stream, config) = build_preferred_discard_stream(
        &device,
        default_config.sample_format(),
        &default_config.config(),
        first_input_notifier,
        failure_tx,
        generation,
    )?;
    stream
        .play()
        .map_err(|error| format!("failed to start audio input keepalive stream: {error}"))?;

    Ok(InputKeepaliveStream {
        _stream: stream,
        device_name,
        config,
        first_input_rx,
    })
}

fn build_preferred_discard_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    base_config: &StreamConfig,
    first_input_notifier: FirstInputBufferNotifier,
    failure_tx: &mpsc::Sender<KeepaliveMessage>,
    generation: u64,
) -> Result<(cpal::Stream, StreamConfig), String> {
    let mut errors = Vec::new();

    for config in input_config_candidates(base_config) {
        match build_discard_stream(
            device,
            sample_format,
            &config,
            first_input_notifier.clone(),
            keepalive_error_callback(failure_tx.clone(), generation),
        ) {
            Ok(stream) => return Ok((stream, config)),
            Err(error) => {
                tracing::debug!(
                    sample_rate_hz = config.sample_rate.0,
                    channels = config.channels,
                    buffer_size = %buffer_size_label(&config.buffer_size),
                    error = %error,
                    "input keepalive config candidate failed"
                );
                errors.push(format!(
                    "{} Hz/{} ch/{} buffer: {}",
                    config.sample_rate.0,
                    config.channels,
                    buffer_size_label(&config.buffer_size),
                    error
                ));
            }
        }
    }

    Err(format!(
        "failed to build input keepalive stream with preferred configs: {}",
        errors.join("; ")
    ))
}

fn build_discard_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    first_input_notifier: FirstInputBufferNotifier,
    error_callback: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String> {
    match sample_format {
        SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _| notify_discarded_input(data, &first_input_notifier),
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build f32 input keepalive stream: {error}")),
        SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _| notify_discarded_input(data, &first_input_notifier),
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build i16 input keepalive stream: {error}")),
        SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _| notify_discarded_input(data, &first_input_notifier),
                error_callback,
                None,
            )
            .map_err(|error| format!("failed to build u16 input keepalive stream: {error}")),
        _ => Err("unsupported input sample format".into()),
    }
}

fn keepalive_error_callback(
    failure_tx: mpsc::Sender<KeepaliveMessage>,
    generation: u64,
) -> impl FnMut(cpal::StreamError) + Send + 'static {
    first_error_only(move |error: cpal::StreamError| {
        let _ = failure_tx.send(KeepaliveMessage::StreamFailed {
            generation,
            error: error.to_string(),
        });
    })
}

// why: cpal 0.15 ALSA calls the error callback in a tight loop after POLLERR without
// recovering, so only the first error per stream may log or wake anyone.
fn first_error_only<E>(
    mut on_first: impl FnMut(E) + Send + 'static,
) -> impl FnMut(E) + Send + 'static {
    let mut reported = false;
    move |error| {
        if !reported {
            reported = true;
            on_first(error);
        }
    }
}

fn notify_discarded_input<T>(data: &[T], first_input_notifier: &FirstInputBufferNotifier) {
    if !data.is_empty() {
        first_input_notifier.notify();
    }
}

fn buffer_size_label(buffer_size: &cpal::BufferSize) -> String {
    match buffer_size {
        cpal::BufferSize::Default => "default".into(),
        cpal::BufferSize::Fixed(size) => size.to_string(),
    }
}

#[derive(Debug, Clone)]
struct FirstInputBufferNotifier {
    tx: SyncSender<()>,
    notified: Arc<AtomicBool>,
}

impl FirstInputBufferNotifier {
    fn new(tx: SyncSender<()>) -> Self {
        Self {
            tx,
            notified: Arc::new(AtomicBool::new(false)),
        }
    }

    fn notify(&self) {
        if !self.notified.swap(true, Ordering::AcqRel) {
            let _ = self.tx.try_send(());
        }
    }
}

fn wait_for_first_input_buffer(rx: &mpsc::Receiver<()>) -> Result<u128, AppError> {
    wait_for_first_input_buffer_with_timeout(rx, FIRST_INPUT_BUFFER_TIMEOUT)
}

fn wait_for_first_input_buffer_with_timeout(
    rx: &mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<u128, AppError> {
    let wait_started_at = Instant::now();
    match rx.recv_timeout(timeout) {
        Ok(()) => Ok(wait_started_at.elapsed().as_millis()),
        Err(RecvTimeoutError::Timeout) => Err(AppError::message(format!(
            "audio input stream did not produce samples within {} ms",
            timeout.as_millis()
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(AppError::message(
            "audio input stream stopped before producing samples",
        )),
    }
}

fn send_converted<I>(
    samples: I,
    tx: &SyncSender<Vec<i16>>,
    first_input_notifier: &FirstInputBufferNotifier,
) where
    I: Iterator<Item = i16>,
{
    let mut peak = 0i16;
    let chunk: Vec<i16> = samples
        .inspect(|sample| peak = peak.max(sample.abs()))
        .collect();
    let has_samples = !chunk.is_empty();
    if has_samples {
        first_input_notifier.notify();
    }
    glossa_app::level::record_peak(f32::from(peak) / f32::from(i16::MAX));
    let _ = tx.send(chunk);
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
            Arc,
        },
        time::Duration,
    };

    use super::*;

    struct FakeIdleInputStream {
        dropped: Arc<AtomicUsize>,
    }

    impl IdleInputStream for FakeIdleInputStream {}

    impl Drop for FakeIdleInputStream {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

    fn test_idle_controller(
        started: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    ) -> IdleKeepaliveController {
        IdleKeepaliveController::with_stream_factory(Arc::new(move || {
            started.fetch_add(1, AtomicOrdering::SeqCst);
            let stream: Arc<dyn IdleInputStream> = Arc::new(FakeIdleInputStream {
                dropped: Arc::clone(&dropped),
            });
            Ok(stream)
        }))
    }

    #[test]
    fn send_converted_should_signal_first_input_buffer_once() {
        let (audio_tx, audio_rx) = mpsc::sync_channel(2);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let notifier = FirstInputBufferNotifier::new(ready_tx);

        send_converted([1_i16, 2_i16].into_iter(), &audio_tx, &notifier);

        assert_eq!(
            audio_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("audio chunk should be sent"),
            vec![1_i16, 2_i16]
        );
        assert!(
            ready_rx.recv_timeout(Duration::from_millis(50)).is_ok(),
            "first input buffer should signal readiness"
        );

        send_converted([3_i16].into_iter(), &audio_tx, &notifier);

        assert!(
            ready_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "subsequent input buffers should not signal readiness again"
        );
    }

    #[test]
    fn send_converted_should_signal_first_input_before_blocked_writer_send_completes() {
        let (audio_tx, audio_rx) = mpsc::sync_channel(0);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let notifier = FirstInputBufferNotifier::new(ready_tx);

        let handle = thread::spawn(move || {
            send_converted([1_i16].into_iter(), &audio_tx, &notifier);
        });

        let ready_result = ready_rx.recv_timeout(Duration::from_millis(50));
        assert_eq!(
            audio_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("audio chunk should unblock the writer send"),
            vec![1_i16]
        );
        handle.join().expect("send thread should finish");

        assert!(
            ready_result.is_ok(),
            "first input readiness should be signaled when the callback receives samples"
        );
    }

    #[test]
    fn input_config_candidates_should_prefer_fixed_low_latency_buffers() {
        let base_config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(16_000),
            buffer_size: cpal::BufferSize::Default,
        };

        let candidates = input_config_candidates(&base_config);

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].buffer_size, cpal::BufferSize::Fixed(512));
        assert_eq!(candidates[1].buffer_size, cpal::BufferSize::Fixed(1024));
        assert_eq!(candidates[2].buffer_size, cpal::BufferSize::Default);
    }

    #[test]
    fn idle_keepalive_controller_should_start_and_stop_without_recording_writer() {
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let controller = test_idle_controller(Arc::clone(&started), Arc::clone(&dropped));

        controller.ensure_on().expect("idle keepalive should start");
        controller
            .ensure_on()
            .expect("second start should reuse idle keepalive");
        assert!(controller.is_active());
        assert_eq!(started.load(AtomicOrdering::SeqCst), 1);

        controller.ensure_off();

        assert!(!controller.is_active());
        assert_eq!(dropped.load(AtomicOrdering::SeqCst), 1);
    }

    fn pollerr() -> cpal::StreamError {
        cpal::StreamError::BackendSpecific {
            err: cpal::BackendSpecificError {
                description: "`alsa::poll()` returned POLLERR".into(),
            },
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

    #[test]
    fn recording_stream_error_burst_should_reach_handler_once() {
        let handled = Arc::new(AtomicUsize::new(0));
        let mut callback = first_error_only({
            let handled = Arc::clone(&handled);
            move |_: cpal::StreamError| {
                handled.fetch_add(1, AtomicOrdering::SeqCst);
            }
        });

        for _ in 0..10_000 {
            callback(pollerr());
        }

        assert_eq!(handled.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn keepalive_error_burst_should_reopen_the_stream_once() {
        let (messages_tx, messages_rx) = mpsc::channel();
        let opens = Arc::new(AtomicUsize::new(0));
        let worker = thread::spawn({
            let opens = Arc::clone(&opens);
            move || {
                run_keepalive_loop(&messages_rx, (), |_| {
                    opens.fetch_add(1, AtomicOrdering::SeqCst);
                    Ok(())
                });
            }
        });

        let mut callback = keepalive_error_callback(messages_tx.clone(), 0);
        for _ in 0..10_000 {
            callback(pollerr());
        }

        assert!(wait_until(Duration::from_secs(3), || opens
            .load(AtomicOrdering::SeqCst)
            == 1));
        thread::sleep(KEEPALIVE_REOPEN_BACKOFF_INITIAL * 3);
        assert_eq!(opens.load(AtomicOrdering::SeqCst), 1);

        messages_tx
            .send(KeepaliveMessage::Shutdown)
            .expect("keepalive worker should be running");
        worker.join().expect("keepalive worker should exit");
    }

    #[test]
    fn keepalive_failing_right_after_open_should_back_off() {
        let (messages_tx, messages_rx) = mpsc::channel();
        let opens = Arc::new(AtomicUsize::new(0));
        let worker = thread::spawn({
            let opens = Arc::clone(&opens);
            let messages_tx = messages_tx.clone();
            move || {
                run_keepalive_loop(&messages_rx, (), |generation| {
                    opens.fetch_add(1, AtomicOrdering::SeqCst);
                    keepalive_error_callback(messages_tx.clone(), generation)(pollerr());
                    Ok(())
                });
            }
        });

        keepalive_error_callback(messages_tx.clone(), 0)(pollerr());
        thread::sleep(Duration::from_millis(3_500));

        assert!(opens.load(AtomicOrdering::SeqCst) <= 3);
        messages_tx
            .send(KeepaliveMessage::Shutdown)
            .expect("keepalive worker should be running");
        worker.join().expect("keepalive worker should exit");
    }

    #[test]
    fn keepalive_should_shut_down_while_waiting_to_reopen() {
        let (messages_tx, messages_rx) = mpsc::channel();
        let exited = Arc::new(AtomicBool::new(false));
        thread::spawn({
            let exited = Arc::clone(&exited);
            move || {
                run_keepalive_loop(&messages_rx, (), |_| Err("no input device".to_string()));
                exited.store(true, AtomicOrdering::Release);
            }
        });

        keepalive_error_callback(messages_tx.clone(), 0)(pollerr());
        thread::sleep(Duration::from_millis(100));
        messages_tx
            .send(KeepaliveMessage::Shutdown)
            .expect("keepalive worker should be running");

        assert!(wait_until(Duration::from_millis(500), || exited
            .load(AtomicOrdering::Acquire)));
    }

    #[tokio::test]
    async fn idle_keepalive_controller_should_release_stream_after_timeout() {
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let controller = Arc::new(test_idle_controller(
            Arc::clone(&started),
            Arc::clone(&dropped),
        ));

        Arc::clone(&controller)
            .schedule_timeout(Duration::from_millis(10))
            .expect("idle keepalive timeout should be scheduled");
        assert!(controller.is_active());

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!controller.is_active());
        assert_eq!(started.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(dropped.load(AtomicOrdering::SeqCst), 1);
    }
}
