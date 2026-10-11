use glossa_core::{
    AppCommand, AppState, CoreError, PastingState, ProcessingState, RecordingState, SessionId,
};

use super::{should_ignore_recording_command, Action};

/// Result of reducing one command against the current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub next_state: AppState,
    pub actions: Vec<Action>,
}

/// Applies a command to the current state and returns the next state plus side effects.
pub fn reduce(state: &AppState, command: &AppCommand) -> Result<Decision, CoreError> {
    use AppCommand::{
        CancelRecording, DisableInputStream, EnableInputStream, Restart, Shutdown, StartRecording,
        StopRecording, ToggleInputStream, ToggleRecording,
    };

    let decision = match (state, command) {
        (_, Restart { .. }) => Decision {
            next_state: AppState::ShuttingDown,
            actions: vec![Action::Restart],
        },
        (_, Shutdown { .. }) => Decision {
            next_state: AppState::ShuttingDown,
            actions: vec![Action::Shutdown],
        },
        (_, ToggleInputStream { .. }) => Decision {
            next_state: state.clone(),
            actions: vec![Action::ToggleInputStream],
        },
        (_, EnableInputStream { .. }) => Decision {
            next_state: state.clone(),
            actions: vec![Action::EnableInputStream],
        },
        (_, DisableInputStream { .. }) => Decision {
            next_state: state.clone(),
            actions: vec![Action::DisableInputStream],
        },
        (AppState::Idle, StartRecording { .. } | ToggleRecording { .. }) => {
            let session_id = SessionId::new();
            Decision {
                next_state: AppState::Recording(RecordingState { session_id }),
                actions: vec![
                    Action::SetTrayRecording,
                    Action::ShowOverlay,
                    Action::StartRecording { session_id },
                    Action::PlayStartCue,
                ],
            }
        }
        (AppState::Idle, StopRecording { .. } | CancelRecording { .. }) => Decision {
            next_state: AppState::Idle,
            actions: vec![Action::Ignore {
                reason: "recording is not active",
            }],
        },
        (AppState::Recording(RecordingState { session_id }), CancelRecording { .. }) => Decision {
            next_state: AppState::Idle,
            actions: vec![
                Action::SetTrayIdle,
                Action::HideOverlay,
                Action::PlayStopCue,
                Action::AbortRecording {
                    session_id: *session_id,
                },
            ],
        },
        (
            AppState::Recording(RecordingState { session_id }),
            StopRecording { .. } | ToggleRecording { .. },
        ) => Decision {
            next_state: AppState::Processing(ProcessingState {
                session_id: *session_id,
            }),
            actions: vec![
                Action::HideOverlay,
                Action::PlayStopCue,
                Action::SetTrayProcessing,
                Action::StopRecording {
                    session_id: *session_id,
                },
            ],
        },
        (AppState::Recording(_), StartRecording { .. }) => Decision {
            next_state: state.clone(),
            actions: vec![Action::Ignore {
                reason: "recording is already active",
            }],
        },
        (
            busy_state,
            StartRecording { .. }
            | StopRecording { .. }
            | ToggleRecording { .. }
            | CancelRecording { .. },
        ) if should_ignore_recording_command(busy_state) => Decision {
            next_state: busy_state.clone(),
            actions: vec![Action::Ignore {
                reason: "recording command ignored while the previous cycle is busy",
            }],
        },
        (AppState::ShuttingDown, _) => Decision {
            next_state: AppState::ShuttingDown,
            actions: vec![Action::Ignore {
                reason: "daemon is shutting down",
            }],
        },
        (AppState::Pasting(PastingState { .. }), _)
        | (AppState::Processing(ProcessingState { .. }), _) => Decision {
            next_state: state.clone(),
            actions: vec![Action::Ignore {
                reason: "unexpected command for busy state",
            }],
        },
    };

    if matches!(decision.next_state, AppState::Idle)
        && !matches!(state, AppState::Idle)
        && !decision
            .actions
            .iter()
            .any(|action| matches!(action, Action::SetTrayIdle))
    {
        return Ok(Decision {
            actions: [decision.actions, vec![Action::SetTrayIdle]].concat(),
            ..decision
        });
    }

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use glossa_core::{
        AppCommand, AppState, CommandOrigin, PastingState, RecordingState, SessionId,
    };

    use super::{reduce, Action};

    #[test]
    fn idle_toggle_should_start_recording() {
        let decision = reduce(
            &AppState::Idle,
            &AppCommand::ToggleRecording {
                origin: CommandOrigin::PortalShortcut,
            },
        )
        .expect("reducer should succeed");

        assert!(matches!(decision.next_state, AppState::Recording(_)));
        assert!(decision.actions.contains(&Action::ShowOverlay));
    }

    #[test]
    fn recording_toggle_should_stop_recording() {
        let session_id = SessionId::new();
        let decision = reduce(
            &AppState::Recording(RecordingState { session_id }),
            &AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            },
        )
        .expect("reducer should succeed");

        assert!(matches!(decision.next_state, AppState::Processing(_)));
    }

    #[test]
    fn recording_cancel_should_return_to_idle_without_processing() {
        let session_id = SessionId::new();
        let decision = reduce(
            &AppState::Recording(RecordingState { session_id }),
            &AppCommand::CancelRecording {
                origin: CommandOrigin::EscapeKey,
            },
        )
        .expect("reducer should succeed");

        assert_eq!(decision.next_state, AppState::Idle);
        assert_eq!(decision.next_state, AppState::Idle);
        assert_eq!(
            decision.actions,
            vec![
                Action::SetTrayIdle,
                Action::HideOverlay,
                Action::PlayStopCue,
                Action::AbortRecording { session_id },
            ]
        );
    }

    #[test]
    fn idle_cancel_should_be_ignored() {
        let decision = reduce(
            &AppState::Idle,
            &AppCommand::CancelRecording {
                origin: CommandOrigin::EscapeKey,
            },
        )
        .expect("reducer should succeed");

        assert_eq!(decision.next_state, AppState::Idle);
        assert_eq!(
            decision.actions,
            vec![Action::Ignore {
                reason: "recording is not active",
            }]
        );
    }

    #[test]
    fn processing_commands_should_be_ignored() {
        let decision = reduce(
            &AppState::Processing(glossa_core::ProcessingState {
                session_id: Default::default(),
            }),
            &AppCommand::ToggleRecording {
                origin: CommandOrigin::CliControl,
            },
        )
        .expect("reducer should succeed");

        assert!(matches!(decision.next_state, AppState::Processing(_)));
        assert_eq!(decision.actions.len(), 1);
    }

    #[test]
    fn busy_cancel_should_be_ignored() {
        for state in [
            AppState::Processing(glossa_core::ProcessingState {
                session_id: Default::default(),
            }),
            AppState::Pasting(PastingState {
                session_id: Default::default(),
                text_len: 5,
            }),
            AppState::ShuttingDown,
        ] {
            let decision = reduce(
                &state,
                &AppCommand::CancelRecording {
                    origin: CommandOrigin::EscapeKey,
                },
            )
            .expect("reducer should succeed");

            assert_eq!(decision.next_state, state);
            assert_eq!(decision.actions.len(), 1);
        }
    }

    #[test]
    fn restart_should_request_shutdown_from_any_state() {
        let decision = reduce(
            &AppState::Recording(RecordingState {
                session_id: Default::default(),
            }),
            &AppCommand::Restart {
                origin: CommandOrigin::TrayMenu,
            },
        )
        .expect("reducer should succeed");

        assert!(matches!(decision.next_state, AppState::ShuttingDown));
        assert_eq!(decision.actions, vec![Action::Restart]);
    }

    #[test]
    fn input_stream_commands_should_be_handled_from_busy_states() {
        for (command, expected_action) in [
            (
                AppCommand::EnableInputStream {
                    origin: CommandOrigin::TrayMenu,
                },
                Action::EnableInputStream,
            ),
            (
                AppCommand::DisableInputStream {
                    origin: CommandOrigin::TrayMenu,
                },
                Action::DisableInputStream,
            ),
        ] {
            let state = AppState::Processing(glossa_core::ProcessingState {
                session_id: Default::default(),
            });
            let decision = reduce(&state, &command).expect("reducer should succeed");

            assert_eq!(decision.next_state, state);
            assert_eq!(decision.actions, vec![expected_action]);
        }
    }

    #[test]
    fn toggle_input_stream_should_be_handled_from_busy_states() {
        let state = AppState::Processing(glossa_core::ProcessingState {
            session_id: Default::default(),
        });

        let decision = reduce(
            &state,
            &AppCommand::ToggleInputStream {
                origin: CommandOrigin::CliControl,
            },
        )
        .expect("reducer should succeed");

        assert_eq!(decision.next_state, state);
        assert_eq!(decision.actions, vec![Action::ToggleInputStream]);
    }
}
