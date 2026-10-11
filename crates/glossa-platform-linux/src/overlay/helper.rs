use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use gtk::{gdk, glib, prelude::*};

use super::pill::PillWindow;

/// Runs the isolated recording indicator with line commands on stdin.
pub fn run_overlay_helper() -> Result<(), String> {
    gtk::init()
        .map_err(|error| format!("cannot initialize XWayland recording overlay: {error}"))?;
    let display = gdk::Display::default().ok_or("recording overlay has no display")?;
    if display.type_().name() != "GdkX11Display" {
        return Err("recording overlay requires GDK_BACKEND=x11 in its own process".into());
    }
    eprintln!(
        "recording overlay backend={} display={}",
        display.type_().name(),
        display.name()
    );

    let pill = PillWindow::new();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    glib::timeout_add_local(Duration::from_millis(16), move || {
        loop {
            match rx.try_recv() {
                Ok(line) => match line.trim() {
                    "show" => pill.set_visible(true),
                    "hide" => pill.set_visible(false),
                    "quit" => {
                        pill.set_visible(false);
                        gtk::main_quit();
                        return glib::ControlFlow::Break;
                    }
                    value => {
                        if let Some(percent) = value
                            .strip_prefix("level ")
                            .and_then(|value| value.parse::<u8>().ok())
                        {
                            pill.set_level(percent);
                        }
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    pill.set_visible(false);
                    gtk::main_quit();
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
    gtk::main();
    Ok(())
}
