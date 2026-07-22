//! Linux-specific adapters for IPC, clipboard, paste, diagnostics, and portals.

pub mod clipboard;
pub mod dialog;
pub mod doctor;
pub mod ipc;
pub mod paste;
pub mod portal;
pub mod secret;
pub(crate) mod service;
pub(crate) mod shortcut_capture;
pub mod temp;
pub mod tray;
pub mod updater;
