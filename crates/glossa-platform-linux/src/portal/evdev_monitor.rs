//! Evdev-based key release fallback for push-to-talk mode.
//!
//! GNOME Shell / Mutter emits the portal `Deactivated` signal only when the
//! **non-modifier key** of a combo is released while the modifier is still
//! held.  If the modifier is released first, `Deactivated` is never sent.
//!
//! This module provides a fallback: once a push-to-talk `Activated` fires, we
//! monitor all keyboard evdev devices and detect when **every** key in the
//! combo has been released, regardless of release order.
//!
//! Requires the user to be in the `input` group (already a prerequisite for
//! `dotool`).

use std::collections::HashSet;
use std::path::PathBuf;

use evdev::{Device, EventSummary, EventType, KeyCode};
use tokio::sync::oneshot;
use tracing::{debug, warn};

pub(crate) fn parse_accelerator_keys(trigger_description: &str) -> Option<HashSet<KeyCode>> {
    let accel = trigger_description
        .strip_prefix("Press ")
        .unwrap_or(trigger_description)
        .trim();

    if accel.is_empty() {
        return None;
    }

    let mut keys = HashSet::new();
    let mut remaining = accel;

    while let Some(start) = remaining.find('<') {
        let end = remaining[start..].find('>')? + start;
        let modifier = &remaining[start + 1..end];
        if let Some(key) = modifier_to_evdev(modifier) {
            keys.insert(key);
        } else {
            warn!(
                modifier,
                "unknown modifier in accelerator; evdev fallback may be incomplete"
            );
        }
        remaining = &remaining[end + 1..];
    }

    let main_key = remaining.trim();
    if !main_key.is_empty() {
        if let Some(key) = key_name_to_evdev(main_key) {
            keys.insert(key);
        } else {
            warn!(
                key = main_key,
                "unknown key in accelerator; evdev fallback may be incomplete"
            );
            return None;
        }
    }

    if keys.is_empty() {
        return None;
    }

    debug!(
        accelerator = accel,
        ?keys,
        "parsed accelerator for evdev monitoring"
    );
    Some(keys)
}

pub(crate) fn spawn_release_monitor(combo_keys: HashSet<KeyCode>) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        if let Err(error) = monitor_key_release(combo_keys, tx).await {
            warn!(error = %error, "evdev key release monitor failed");
        }
    });

    rx
}

pub(crate) fn spawn_escape_press_monitor() -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        if let Err(error) = monitor_key_press(KeyCode::KEY_ESC, tx).await {
            warn!(error = %error, "evdev escape key monitor failed");
        }
    });

    rx
}

async fn monitor_key_release(
    combo_keys: HashSet<KeyCode>,
    done: oneshot::Sender<()>,
) -> Result<(), String> {
    let mut held: HashSet<KeyCode> = combo_keys.clone();
    let mut streams = open_keyboard_event_streams()?;

    debug!(
        device_count = streams.len(),
        "evdev release monitor started"
    );

    use futures_util::stream::FuturesUnordered;
    use futures_util::StreamExt;

    loop {
        if held.is_empty() {
            debug!("all combo keys released; signalling stop");
            let _ = done.send(());
            return Ok(());
        }

        if done.is_closed() {
            debug!("evdev monitor cancelled; portal Deactivated arrived first");
            return Ok(());
        }

        let mut futs: FuturesUnordered<_> = streams
            .iter_mut()
            .enumerate()
            .map(|(i, s)| async move { (i, s.next_event().await) })
            .collect();

        if let Some((idx, result)) = futs.next().await {
            drop(futs); // release mutable borrows on streams
            match result {
                Ok(event) => {
                    if let EventSummary::Key(_, key, value) = event.destructure() {
                        let key = normalize_modifier_key(key);
                        if value == 0 && combo_keys.contains(&key) {
                            debug!(?key, "combo key released");
                            held.remove(&key);
                        } else if value == 1 && combo_keys.contains(&key) {
                            held.insert(key);
                        }
                    }
                }
                Err(error) => {
                    debug!(error = %error, "evdev read error; continuing with other devices");
                    discard_failed_stream(&mut streams, idx);
                }
            }
        } else {
            return Err("all evdev event streams ended".into());
        }
    }
}

async fn monitor_key_press(target_key: KeyCode, done: oneshot::Sender<()>) -> Result<(), String> {
    let mut streams = open_keyboard_event_streams()?;

    debug!(
        ?target_key,
        device_count = streams.len(),
        "evdev key press monitor started"
    );

    use futures_util::stream::FuturesUnordered;
    use futures_util::StreamExt;

    loop {
        if done.is_closed() {
            debug!(?target_key, "evdev key press monitor cancelled");
            return Ok(());
        }

        let mut futs: FuturesUnordered<_> = streams
            .iter_mut()
            .enumerate()
            .map(|(i, s)| async move { (i, s.next_event().await) })
            .collect();

        if let Some((idx, result)) = futs.next().await {
            drop(futs);
            match result {
                Ok(event) => {
                    if let EventSummary::Key(_, key, value) = event.destructure() {
                        let key = normalize_modifier_key(key);
                        if value == 1 && key == target_key {
                            debug!(?key, "target key pressed");
                            let _ = done.send(());
                            return Ok(());
                        }
                    }
                }
                Err(error) => {
                    debug!(error = %error, "evdev read error; continuing with other devices");
                    discard_failed_stream(&mut streams, idx);
                }
            }
        } else {
            return Err("all evdev event streams ended".into());
        }
    }
}

fn discard_failed_stream<T>(streams: &mut Vec<T>, failed_index: usize) {
    let _ = streams.swap_remove(failed_index);
}

fn open_keyboard_devices() -> Result<Vec<Device>, String> {
    let mut devices = Vec::new();

    let input_dir = PathBuf::from("/dev/input");
    let entries =
        std::fs::read_dir(&input_dir).map_err(|e| format!("cannot read /dev/input: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("event") {
            continue;
        }

        match Device::open(&path) {
            Ok(device) => {
                if device.supported_events().contains(EventType::KEY) {
                    if device
                        .supported_keys()
                        .is_some_and(|keys| keys.contains(KeyCode::KEY_A))
                    {
                        debug!(path = %path.display(), name = ?device.name(), "opened keyboard device");
                        devices.push(device);
                    }
                }
            }
            Err(error) => {
                debug!(
                    path = %path.display(),
                    error = %error,
                    "cannot open evdev device (permission denied?)"
                );
            }
        }
    }

    Ok(devices)
}

fn open_keyboard_event_streams() -> Result<Vec<evdev::EventStream>, String> {
    let devices = open_keyboard_devices()?;
    if devices.is_empty() {
        return Err("no keyboard evdev devices found; is the user in the 'input' group?".into());
    }

    let mut streams = Vec::new();
    for device in devices {
        match device.into_event_stream() {
            Ok(stream) => streams.push(stream),
            Err(error) => {
                debug!(error = %error, "failed to create event stream for device");
            }
        }
    }

    if streams.is_empty() {
        return Err("could not create event streams for any keyboard device".into());
    }

    Ok(streams)
}

fn normalize_modifier_key(key: KeyCode) -> KeyCode {
    match key {
        KeyCode::KEY_RIGHTCTRL => KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_RIGHTALT => KeyCode::KEY_LEFTALT,
        KeyCode::KEY_RIGHTSHIFT => KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_RIGHTMETA => KeyCode::KEY_LEFTMETA,
        other => other,
    }
}

fn modifier_to_evdev(modifier: &str) -> Option<KeyCode> {
    match modifier.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "primary" => Some(KeyCode::KEY_LEFTCTRL),
        "shift" => Some(KeyCode::KEY_LEFTSHIFT),
        "alt" | "mod1" => Some(KeyCode::KEY_LEFTALT),
        "super" | "logo" | "meta" | "mod4" => Some(KeyCode::KEY_LEFTMETA),
        _ => None,
    }
}

fn key_name_to_evdev(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "a" => Some(KeyCode::KEY_A),
        "b" => Some(KeyCode::KEY_B),
        "c" => Some(KeyCode::KEY_C),
        "d" => Some(KeyCode::KEY_D),
        "e" => Some(KeyCode::KEY_E),
        "f" => Some(KeyCode::KEY_F),
        "g" => Some(KeyCode::KEY_G),
        "h" => Some(KeyCode::KEY_H),
        "i" => Some(KeyCode::KEY_I),
        "j" => Some(KeyCode::KEY_J),
        "k" => Some(KeyCode::KEY_K),
        "l" => Some(KeyCode::KEY_L),
        "m" => Some(KeyCode::KEY_M),
        "n" => Some(KeyCode::KEY_N),
        "o" => Some(KeyCode::KEY_O),
        "p" => Some(KeyCode::KEY_P),
        "q" => Some(KeyCode::KEY_Q),
        "r" => Some(KeyCode::KEY_R),
        "s" => Some(KeyCode::KEY_S),
        "t" => Some(KeyCode::KEY_T),
        "u" => Some(KeyCode::KEY_U),
        "v" => Some(KeyCode::KEY_V),
        "w" => Some(KeyCode::KEY_W),
        "x" => Some(KeyCode::KEY_X),
        "y" => Some(KeyCode::KEY_Y),
        "z" => Some(KeyCode::KEY_Z),
        "0" => Some(KeyCode::KEY_0),
        "1" => Some(KeyCode::KEY_1),
        "2" => Some(KeyCode::KEY_2),
        "3" => Some(KeyCode::KEY_3),
        "4" => Some(KeyCode::KEY_4),
        "5" => Some(KeyCode::KEY_5),
        "6" => Some(KeyCode::KEY_6),
        "7" => Some(KeyCode::KEY_7),
        "8" => Some(KeyCode::KEY_8),
        "9" => Some(KeyCode::KEY_9),
        "f1" => Some(KeyCode::KEY_F1),
        "f2" => Some(KeyCode::KEY_F2),
        "f3" => Some(KeyCode::KEY_F3),
        "f4" => Some(KeyCode::KEY_F4),
        "f5" => Some(KeyCode::KEY_F5),
        "f6" => Some(KeyCode::KEY_F6),
        "f7" => Some(KeyCode::KEY_F7),
        "f8" => Some(KeyCode::KEY_F8),
        "f9" => Some(KeyCode::KEY_F9),
        "f10" => Some(KeyCode::KEY_F10),
        "f11" => Some(KeyCode::KEY_F11),
        "f12" => Some(KeyCode::KEY_F12),
        "f13" => Some(KeyCode::KEY_F13),
        "space" => Some(KeyCode::KEY_SPACE),
        "return" | "enter" => Some(KeyCode::KEY_ENTER),
        "escape" | "esc" => Some(KeyCode::KEY_ESC),
        "tab" => Some(KeyCode::KEY_TAB),
        "backspace" => Some(KeyCode::KEY_BACKSPACE),
        "delete" => Some(KeyCode::KEY_DELETE),
        "insert" => Some(KeyCode::KEY_INSERT),
        "home" => Some(KeyCode::KEY_HOME),
        "end" => Some(KeyCode::KEY_END),
        "page_up" | "pageup" | "prior" => Some(KeyCode::KEY_PAGEUP),
        "page_down" | "pagedown" | "next" => Some(KeyCode::KEY_PAGEDOWN),
        "up" => Some(KeyCode::KEY_UP),
        "down" => Some(KeyCode::KEY_DOWN),
        "left" => Some(KeyCode::KEY_LEFT),
        "right" => Some(KeyCode::KEY_RIGHT),
        "pause" => Some(KeyCode::KEY_PAUSE),
        "scroll_lock" | "scrolllock" => Some(KeyCode::KEY_SCROLLLOCK),
        "print" | "print_screen" | "sysrq" => Some(KeyCode::KEY_SYSRQ),
        "caps_lock" | "capslock" => Some(KeyCode::KEY_CAPSLOCK),
        "num_lock" | "numlock" => Some(KeyCode::KEY_NUMLOCK),
        "minus" => Some(KeyCode::KEY_MINUS),
        "equal" | "equals" => Some(KeyCode::KEY_EQUAL),
        "bracketleft" | "bracket_left" => Some(KeyCode::KEY_LEFTBRACE),
        "bracketright" | "bracket_right" => Some(KeyCode::KEY_RIGHTBRACE),
        "backslash" => Some(KeyCode::KEY_BACKSLASH),
        "semicolon" => Some(KeyCode::KEY_SEMICOLON),
        "apostrophe" | "quoteright" => Some(KeyCode::KEY_APOSTROPHE),
        "grave" | "quoteleft" => Some(KeyCode::KEY_GRAVE),
        "comma" => Some(KeyCode::KEY_COMMA),
        "period" => Some(KeyCode::KEY_DOT),
        "slash" => Some(KeyCode::KEY_SLASH),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ctrl_backslash() {
        let keys = parse_accelerator_keys("Press <Ctrl>backslash").unwrap();
        assert!(keys.contains(&KeyCode::KEY_LEFTCTRL));
        assert!(keys.contains(&KeyCode::KEY_BACKSLASH));
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn parse_alt_super_space() {
        let keys = parse_accelerator_keys("Press <Alt><Super>space").unwrap();
        assert!(keys.contains(&KeyCode::KEY_LEFTALT));
        assert!(keys.contains(&KeyCode::KEY_LEFTMETA));
        assert!(keys.contains(&KeyCode::KEY_SPACE));
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn parse_single_key() {
        let keys = parse_accelerator_keys("Press backslash").unwrap();
        assert!(keys.contains(&KeyCode::KEY_BACKSLASH));
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn parse_without_press_prefix() {
        let keys = parse_accelerator_keys("<Shift>a").unwrap();
        assert!(keys.contains(&KeyCode::KEY_LEFTSHIFT));
        assert!(keys.contains(&KeyCode::KEY_A));
    }

    #[test]
    fn parse_unknown_key_returns_none() {
        assert!(parse_accelerator_keys("Press <Ctrl>xyznonexistent").is_none());
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_accelerator_keys("").is_none());
    }

    #[test]
    fn failed_stream_is_discarded_while_healthy_streams_remain() {
        let mut streams = vec!["keyboard", "disconnected hotkey device"];

        discard_failed_stream(&mut streams, 1);

        assert_eq!(streams, vec!["keyboard"]);
    }
}
