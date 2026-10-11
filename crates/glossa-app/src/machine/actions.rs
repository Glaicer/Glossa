use glossa_core::SessionId;

/// Side effects requested by the pure reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StartRecording { session_id: SessionId },
    StopRecording { session_id: SessionId },
    AbortRecording { session_id: SessionId },
    SetTrayIdle,
    SetTrayRecording,
    SetTrayProcessing,
    ShowOverlay,
    HideOverlay,
    PlayStartCue,
    PlayStopCue,
    ToggleInputStream,
    EnableInputStream,
    DisableInputStream,
    Ignore { reason: &'static str },
    Restart,
    Shutdown,
}
