use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use glossa_app::{
    ports::ClipboardWriter,
    services::file_transcription::{validate_transcription_file, FileTranscriptionService},
};
use gtk::{
    glib, pango::EllipsizeMode, prelude::*, Box as GtkBox, Button, FileChooserAction,
    FileChooserDialog, FileFilter, Label, Orientation, ResponseType, ScrolledWindow, Separator,
    ShadowType, TextBuffer, TextView, Window, WindowType, WrapMode,
};
use tokio::{runtime::Handle, task::AbortHandle};

#[derive(Clone)]
pub struct FileTranscriptionUi {
    service: Arc<FileTranscriptionService>,
    clipboard: Arc<dyn ClipboardWriter>,
    runtime: Handle,
}

struct DialogState {
    file: RefCell<Option<PathBuf>>,
    task: RefCell<Option<AbortHandle>>,
    filename: Label,
    choose: Button,
    transcribe: Button,
    clear: Button,
    buffer: TextBuffer,
    status: Label,
    copy: Button,
}

impl DialogState {
    fn clear_result(&self) {
        self.buffer.set_text("");
        self.copy.set_sensitive(false);
        self.transcribe.set_label("Transcribe");
        self.clear.hide();
        self.status.set_text("");
    }
}

impl FileTranscriptionUi {
    pub fn new(
        service: Arc<FileTranscriptionService>,
        clipboard: Arc<dyn ClipboardWriter>,
        runtime: Handle,
    ) -> Self {
        Self {
            service,
            clipboard,
            runtime,
        }
    }

    pub(super) fn show(&self) {
        let window = Window::new(WindowType::Toplevel);
        window.set_title("Transcribe File");
        window.set_default_size(760, 560);
        let container = GtkBox::new(Orientation::Vertical, 12);
        container.set_border_width(12);
        let row = GtkBox::new(Orientation::Horizontal, 12);
        let filename = Label::new(None);
        filename.set_xalign(0.0);
        filename.set_ellipsize(EllipsizeMode::Middle);
        filename.set_max_width_chars(24);
        let choose = Button::with_label("Choose file…");
        let transcribe = Button::with_label("Transcribe");
        transcribe.set_no_show_all(true);
        let clear = Button::with_label("Clear");
        clear.set_no_show_all(true);
        row.pack_start(&choose, false, false, 0);
        row.pack_start(&filename, true, true, 0);
        row.pack_end(&transcribe, false, false, 0);
        row.pack_end(&clear, false, false, 0);
        container.pack_start(&row, false, false, 0);

        let result_label = Label::new(Some("Transcription"));
        result_label.set_xalign(0.0);
        let result = GtkBox::new(Orientation::Vertical, 4);
        result.pack_start(&result_label, false, false, 0);
        let buffer = TextBuffer::new(None::<&gtk::TextTagTable>);
        let text = TextView::with_buffer(&buffer);
        text.set_editable(false);
        text.set_wrap_mode(WrapMode::WordChar);
        text.set_left_margin(10);
        text.set_right_margin(10);
        text.set_top_margin(10);
        text.set_bottom_margin(10);
        let scrolled = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scrolled.set_shadow_type(ShadowType::In);
        scrolled.add(&text);
        result.pack_start(&scrolled, true, true, 0);
        container.pack_start(&result, true, true, 0);

        let status = Label::new(None);
        status.set_xalign(0.0);
        status.set_line_wrap(true);
        status.set_max_width_chars(70);
        container.pack_start(&status, false, false, 0);
        container.pack_start(&Separator::new(Orientation::Horizontal), false, false, 0);
        let buttons = GtkBox::new(Orientation::Horizontal, 8);
        let copy = Button::with_label("Copy");
        copy.set_sensitive(false);
        let close = Button::with_label("Close");
        buttons.pack_end(&close, false, false, 0);
        buttons.pack_end(&copy, false, false, 0);
        container.pack_start(&buttons, false, false, 0);
        window.add(&container);

        let state = Rc::new(DialogState {
            file: RefCell::new(None),
            task: RefCell::new(None),
            filename,
            choose,
            transcribe,
            clear,
            buffer,
            status,
            copy,
        });
        let weak_state = Rc::downgrade(&state);
        let parent = window.downgrade();
        state.choose.connect_clicked(move |_| {
            let (Some(state), Some(parent)) = (weak_state.upgrade(), parent.upgrade()) else {
                return;
            };
            let chooser = FileChooserDialog::new(
                Some("Choose audio file"),
                Some(&parent),
                FileChooserAction::Open,
            );
            chooser.set_modal(true);
            chooser.set_destroy_with_parent(true);
            chooser.add_buttons(&[
                ("Cancel", ResponseType::Cancel),
                ("Choose", ResponseType::Accept),
            ]);
            let filter = FileFilter::new();
            filter.set_name(Some("Audio files"));
            filter.add_mime_type("audio/*");
            chooser.add_filter(filter);
            if chooser.run() == ResponseType::Accept {
                if let Some(path) = chooser.filename() {
                    state.clear_result();
                    match validate_transcription_file(&path) {
                        Ok(_) => {
                            state
                                .filename
                                .set_text(&path.file_name().unwrap_or_default().to_string_lossy());
                            state
                                .filename
                                .set_tooltip_text(Some(&path.to_string_lossy()));
                            *state.file.borrow_mut() = Some(path);
                            state.transcribe.show();
                            state.status.set_text("");
                        }
                        Err(error) => {
                            *state.file.borrow_mut() = None;
                            state.filename.set_text("");
                            state.filename.set_tooltip_text(None);
                            state.transcribe.hide();
                            state.status.set_text(&error.to_string());
                        }
                    }
                }
            }
            chooser.close();
        });

        let weak_state = Rc::downgrade(&state);
        let ui = self.clone();
        state.transcribe.connect_clicked(move |_| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            let Some(path) = state.file.borrow().clone() else {
                return;
            };
            state.choose.set_sensitive(false);
            state.transcribe.set_sensitive(false);
            state.copy.set_sensitive(false);
            state.clear.hide();
            state.buffer.set_text("");
            state.status.set_text("Transcribing…");
            let service = ui.service.clone();
            let job = ui
                .runtime
                .spawn(async move { service.transcribe(&path).await });
            *state.task.borrow_mut() = Some(job.abort_handle());
            let weak_state = Rc::downgrade(&state);
            glib::MainContext::default().spawn_local(async move {
                let result = job.await;
                let Some(state) = weak_state.upgrade() else {
                    return;
                };
                state.task.borrow_mut().take();
                state.choose.set_sensitive(true);
                state.transcribe.set_sensitive(true);
                match result {
                    Ok(Ok(text)) => {
                        state.buffer.set_text(&text);
                        state.copy.set_sensitive(true);
                        state.transcribe.set_label("Retry");
                        state.clear.show();
                        state.status.set_text("");
                    }
                    Ok(Err(error)) => state.status.set_text(&error.to_string()),
                    Err(error) => state
                        .status
                        .set_text(&format!("Transcription failed: {error}")),
                }
            });
        });

        let weak_state = Rc::downgrade(&state);
        state.clear.connect_clicked(move |_| {
            if let Some(state) = weak_state.upgrade() {
                state.clear_result();
            }
        });

        let weak_state = Rc::downgrade(&state);
        let ui = self.clone();
        state.copy.connect_clicked(move |_| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            let buffer = &state.buffer;
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .unwrap_or_default()
                .to_string();
            state.copy.set_sensitive(false);
            state.choose.set_sensitive(false);
            state.transcribe.set_sensitive(false);
            state.clear.set_sensitive(false);
            let clipboard = ui.clipboard.clone();
            let job = ui
                .runtime
                .spawn(async move { clipboard.set_text(&text).await });
            *state.task.borrow_mut() = Some(job.abort_handle());
            let weak_state = Rc::downgrade(&state);
            glib::MainContext::default().spawn_local(async move {
                let result = job.await;
                let Some(state) = weak_state.upgrade() else {
                    return;
                };
                state.task.borrow_mut().take();
                state.copy.set_sensitive(true);
                state.choose.set_sensitive(true);
                state.transcribe.set_sensitive(true);
                state.clear.set_sensitive(true);
                match result {
                    Ok(Ok(())) => state.status.set_text("Copied"),
                    Ok(Err(error)) => state.status.set_text(&error.to_string()),
                    Err(error) => state.status.set_text(&format!("Copy failed: {error}")),
                }
            });
        });
        let weak_window = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(window) = weak_window.upgrade() {
                window.close();
            }
        });
        window.connect_key_press_event(|window, event| {
            if event.keyval() == gtk::gdk::keys::constants::Escape {
                window.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.connect_destroy(move |_| {
            if let Some(task) = state.task.borrow_mut().take() {
                task.abort();
            }
        });
        window.show_all();
        window.present();
    }
}
