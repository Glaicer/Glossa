use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Instant;

use gtk::cairo::{self, Context};
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk::{DrawingArea, Window, WindowType};

const PILL_WIDTH: i32 = 320;
const PILL_HEIGHT: i32 = 72;
const PILL_RADIUS: f64 = 36.0;
const BOTTOM_MARGIN: i32 = 28;
const BACKGROUND_RGB: (f64, f64, f64) = (64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0);

const MIC_SIZE: f64 = 32.0;
const MIC_ORIGIN_X: f64 = 24.0;

const BAR_COUNT: usize = 10;
const BAR_WIDTH: f64 = 8.0;
const BAR_GAP: f64 = 15.0;
const BAR_MIN_HEIGHT: f64 = 6.0;
const BAR_MAX_HEIGHT: f64 = 40.0;
const BAR_ORIGIN_X: f64 = MIC_ORIGIN_X + MIC_SIZE + 20.0;
const BAR_WOBBLE_SPEED: f64 = 9.0;
const MIN_BAR_FRACTION: f64 = BAR_MIN_HEIGHT / BAR_MAX_HEIGHT;
const BAR_MULTIPLIERS: [f64; BAR_COUNT] =
    [0.30, 0.48, 0.66, 0.84, 1.0, 1.0, 0.84, 0.66, 0.48, 0.30];

const DOT_MIN_RADIUS: f64 = 3.0;
const DOT_MAX_RADIUS: f64 = 8.0;

const LEVEL_ATTACK: f32 = 0.45;
const LEVEL_DECAY: f32 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillMode {
    Recording,
    Processing,
}

/// XWayland indicator drawn as a dark pill with a live waveform or processing dots.
pub struct PillWindow {
    window: Window,
    level: Rc<Cell<f32>>,
    target: Rc<Cell<f32>>,
    mode: Rc<Cell<PillMode>>,
}

impl PillWindow {
    pub fn new() -> Self {
        let window = Window::new(WindowType::Toplevel);
        window.set_title("glossa-recording");
        window.set_skip_taskbar_hint(true);
        window.set_skip_pager_hint(true);
        window.set_type_hint(gdk::WindowTypeHint::Dock);
        window.set_keep_above(true);
        window.set_accept_focus(false);
        window.set_focus_on_map(false);
        window.stick();
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_app_paintable(true);
        window.set_default_size(PILL_WIDTH, PILL_HEIGHT);
        let screen = GtkWindowExt::screen(&window);
        window.set_visual(
            screen
                .as_ref()
                .and_then(|screen| screen.rgba_visual())
                .as_ref(),
        );

        let drawing_area = DrawingArea::new();
        drawing_area.set_size_request(PILL_WIDTH, PILL_HEIGHT);

        let level = Rc::new(Cell::new(0.0_f32));
        let mode = Rc::new(Cell::new(PillMode::Recording));
        let draw_mode = Rc::clone(&mode);
        let started_at = Instant::now();
        let draw_level = Rc::clone(&level);
        drawing_area.connect_draw(move |_area, context| {
            draw_pill(
                context,
                draw_mode.get(),
                draw_level.get(),
                started_at.elapsed().as_secs_f64(),
            );
            glib::Propagation::Proceed
        });

        let target = Rc::new(Cell::new(0.0_f32));
        let tick_target = Rc::clone(&target);
        let tick_level = Rc::clone(&level);
        drawing_area.add_tick_callback(move |widget, _frame_clock| {
            tick_level.set(smooth_level(tick_level.get(), tick_target.get()));
            widget.queue_draw();
            glib::ControlFlow::Continue
        });

        window.add(&drawing_area);
        make_click_through(&window);
        window.set_visible(false);

        Self {
            window,
            level,
            target,
            mode,
        }
    }

    pub fn show(&self, mode: PillMode) {
        self.mode.set(mode);
        self.set_visible(true);
    }

    pub fn set_visible(&self, visible: bool) {
        self.level.set(0.0);
        self.target.set(0.0);
        if visible {
            self.window.show_all();
            position_bottom_center(&self.window);
        } else {
            self.window.hide();
        }
    }

    pub fn set_level(&self, percent: u8) {
        self.target.set(f32::from(percent.min(100)) / 100.0);
    }
}

fn position_bottom_center(window: &Window) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let Some(monitor) = display.primary_monitor().or_else(|| display.monitor(0)) else {
        return;
    };
    let area = monitor.geometry();
    window.move_(
        area.x() + (area.width() - PILL_WIDTH) / 2,
        area.y() + area.height() - PILL_HEIGHT - BOTTOM_MARGIN,
    );
}

fn make_click_through(window: &Window) {
    window.input_shape_combine_region(Some(&cairo::Region::create()));
}

fn draw_pill(context: &Context, mode: PillMode, level: f32, elapsed_secs: f64) {
    context.set_operator(cairo::Operator::Source);
    context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    context.paint().expect("clear pill");
    context.set_operator(cairo::Operator::Over);
    context.set_source_rgb(BACKGROUND_RGB.0, BACKGROUND_RGB.1, BACKGROUND_RGB.2);
    rounded_rectangle(
        context,
        0.0,
        0.0,
        f64::from(PILL_WIDTH),
        f64::from(PILL_HEIGHT),
        PILL_RADIUS,
    );
    context.fill().expect("fill pill background");

    match mode {
        PillMode::Recording => {
            draw_microphone(context);
            for index in 0..BAR_COUNT {
                draw_bar(context, index, bar_fraction(level, elapsed_secs, index));
            }
        }
        PillMode::Processing => {
            draw_server(context);
            context.set_source_rgb(1.0, 1.0, 1.0);
            for index in 0..BAR_COUNT {
                let x = BAR_ORIGIN_X + BAR_WIDTH / 2.0 + index as f64 * (BAR_WIDTH + BAR_GAP);
                context.new_path();
                context.arc(
                    x,
                    f64::from(PILL_HEIGHT) / 2.0,
                    dot_radius(elapsed_secs, index),
                    0.0,
                    2.0 * PI,
                );
                context.fill().expect("fill processing dot");
            }
        }
    }
}

fn draw_microphone(context: &Context) {
    context.save().expect("save context for microphone");
    context.translate(MIC_ORIGIN_X, (f64::from(PILL_HEIGHT) - MIC_SIZE) / 2.0);
    context.scale(MIC_SIZE / 24.0, MIC_SIZE / 24.0);
    context.set_source_rgb(1.0, 1.0, 1.0);
    append_microphone_head(context);
    context.fill().expect("fill microphone head");
    append_microphone_stand(context);
    context.fill().expect("fill microphone stand");
    context.restore().expect("restore context after microphone");
}

fn draw_server(context: &Context) {
    context.save().expect("save context for server");
    context.translate(MIC_ORIGIN_X, (f64::from(PILL_HEIGHT) - MIC_SIZE) / 2.0);
    context.scale(MIC_SIZE / 512.0, MIC_SIZE / 512.0);
    context.set_source_rgb(1.0, 1.0, 1.0);
    context.set_fill_rule(cairo::FillRule::EvenOdd);
    for y in [32.0, 192.0, 352.0] {
        rounded_rectangle(context, 0.0, y, 512.0, 128.0, 32.0);
        for x in [368.0, 432.0] {
            context.new_sub_path();
            context.arc(x, y + 64.0, 24.0, 0.0, 2.0 * PI);
        }
        context.fill().expect("fill server rack");
    }
    context.restore().expect("restore context after server");
}

fn dot_radius(elapsed_secs: f64, index: usize) -> f64 {
    let phase = index as f64 * 1.7;
    let pulse = 0.5
        + 0.5
            * (0.65 * (elapsed_secs * 4.0 + phase).sin()
                + 0.35 * (elapsed_secs * 2.7 + phase * 1.3).sin());
    DOT_MIN_RADIUS + (DOT_MAX_RADIUS - DOT_MIN_RADIUS) * pulse
}

fn draw_bar(context: &Context, index: usize, fraction: f64) {
    let height = BAR_MIN_HEIGHT + (BAR_MAX_HEIGHT - BAR_MIN_HEIGHT) * fraction;
    let x = BAR_ORIGIN_X + index as f64 * (BAR_WIDTH + BAR_GAP);
    let y = (f64::from(PILL_HEIGHT) - height) / 2.0;
    context.set_source_rgb(1.0, 1.0, 1.0);
    rounded_rectangle(context, x, y, BAR_WIDTH, height, BAR_WIDTH / 2.0);
    context.fill().expect("fill waveform bar");
}

/// Microphone head on the 24x24 source grid.
fn append_microphone_head(context: &Context) {
    context.new_path();
    context.move_to(12.0, 16.0);
    context.rel_curve_to(2.206, 0.0, 4.0, -1.794, 4.0, -4.0);
    context.line_to(16.0, 6.0);
    context.rel_curve_to(0.0, -2.217, -1.785, -4.021, -3.979, -4.021);
    context.line_to(11.812, 2.004);
    context.arc_negative(12.0, 6.0, 4.0, -92.7_f64.to_radians(), -PI);
    context.line_to(8.0, 12.0);
    context.rel_curve_to(0.0, 2.206, 1.794, 4.0, 4.0, 4.0);
    context.close_path();
}

/// Microphone stand on the 24x24 source grid.
fn append_microphone_stand(context: &Context) {
    context.new_path();
    context.move_to(11.0, 19.931);
    context.line_to(11.0, 22.0);
    context.line_to(13.0, 22.0);
    context.line_to(13.0, 19.931);
    context.rel_curve_to(3.939, -0.495, 7.0, -3.858, 7.0, -7.931);
    context.line_to(18.0, 12.0);
    context.rel_curve_to(0.0, 3.309, -2.691, 6.0, -6.0, 6.0);
    context.rel_curve_to(-3.291, 0.0, -6.0, -2.691, -6.0, -6.0);
    context.line_to(4.0, 12.0);
    context.rel_curve_to(0.0, 4.072, 3.061, 7.436, 7.0, 7.931);
    context.close_path();
}

fn rounded_rectangle(context: &Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    context.new_path();
    context.move_to(x + radius, y);
    context.line_to(x + width - radius, y);
    context.arc(x + width - radius, y + radius, radius, -PI / 2.0, 0.0);
    context.line_to(x + width, y + height - radius);
    context.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        PI / 2.0,
    );
    context.line_to(x + radius, y + height);
    context.arc(x + radius, y + height - radius, radius, PI / 2.0, PI);
    context.line_to(x, y + radius);
    context.arc(x + radius, y + radius, radius, PI, PI * 1.5);
    context.close_path();
}

/// Moves the displayed level toward the microphone level: quick on attack,
/// slow release so the waveform trails off instead of snapping to rest.
pub fn smooth_level(current: f32, target: f32) -> f32 {
    let factor = if target > current {
        LEVEL_ATTACK
    } else {
        LEVEL_DECAY
    };
    (current + (target - current) * factor).clamp(0.0, 1.0)
}

/// Height of one waveform bar as a fraction of the maximum bar height.
pub fn bar_fraction(level: f32, elapsed_secs: f64, index: usize) -> f64 {
    let multiplier = BAR_MULTIPLIERS
        .get(index)
        .copied()
        .expect("bar index is inside the waveform");
    let wobble = 0.88 + 0.12 * (elapsed_secs * BAR_WOBBLE_SPEED + index as f64 * 0.9).sin();
    (f64::from(level) * multiplier * wobble).clamp(MIN_BAR_FRACTION, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{
        bar_fraction, dot_radius, draw_pill, smooth_level, PillMode, DOT_MAX_RADIUS,
        DOT_MIN_RADIUS, MIN_BAR_FRACTION,
    };
    use gtk::cairo;
    #[test]
    fn silence_should_rest_at_minimum_bars() {
        for index in 0..10 {
            assert_eq!(bar_fraction(0.0, 1.7, index), MIN_BAR_FRACTION);
        }
    }

    #[test]
    fn center_bars_should_be_taller_than_outer_bars() {
        let center = bar_fraction(1.0, 0.0, 4);
        let outer = bar_fraction(1.0, 0.0, 0);
        assert!(center > outer);
        assert!(center <= 1.0);
    }

    #[test]
    fn louder_level_should_produce_taller_bars() {
        assert!(bar_fraction(0.9, 0.1, 4) > bar_fraction(0.3, 0.1, 4));
    }

    #[test]
    fn rising_level_should_be_followed_quickly() {
        let next = smooth_level(0.2, 0.8);
        assert!(next > 0.4);
        assert!(next < 0.8);
    }

    #[test]
    fn falling_level_should_release_slowly() {
        let next = smooth_level(0.8, 0.0);
        assert!(next > 0.7);
        assert!(next < 0.8);
    }

    #[test]
    fn level_should_stay_within_bounds() {
        assert_eq!(smooth_level(0.0, 0.0), 0.0);
        assert_eq!(smooth_level(1.0, 1.0), 1.0);
    }

    #[test]
    fn rendered_pill_should_draw_dark_background_and_white_glyphs() {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 320, 72)
            .expect("create test surface");
        {
            let context = cairo::Context::new(&surface).expect("create test context");
            draw_pill(&context, PillMode::Recording, 0.5, 1.0);
        }
        surface.flush();

        let stride = surface.stride() as usize;
        let data = surface.data().expect("surface data should be readable");
        let pixel = |x: usize, y: usize| {
            let offset = y * stride + x * 4;
            (data[offset + 2], data[offset + 1], data[offset + 3])
        };

        assert_eq!(pixel(2, 2), (0, 0, 0));
        assert_eq!(pixel(160, 2), (64, 64, 255));

        let has_white = (24..56).any(|x| (20..52).any(|y| pixel(x, y) == (255, 255, 255)));
        assert!(has_white, "microphone area should contain white pixels");
    }

    #[test]
    fn processing_dots_should_pulse_independently_within_bounds() {
        for index in 0..10 {
            for frame in 0..600 {
                let radius = dot_radius(f64::from(frame) / 60.0, index);
                assert!((DOT_MIN_RADIUS..=DOT_MAX_RADIUS).contains(&radius));
            }
            assert_ne!(dot_radius(0.0, index), dot_radius(0.3, index));
        }
        assert_ne!(dot_radius(0.3, 0), dot_radius(0.3, 1));
    }

    #[test]
    fn processing_pill_should_draw_server_and_dots_instead_of_waveform() {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 320, 72)
            .expect("create test surface");
        {
            let context = cairo::Context::new(&surface).expect("create test context");
            draw_pill(&context, PillMode::Processing, 1.0, 0.0);
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface.data().expect("surface data should be readable");
        let pixel = |x: usize, y: usize| {
            let offset = y * stride + x * 4;
            (data[offset + 2], data[offset + 1], data[offset + 3])
        };
        assert_eq!(pixel(2, 2), (0, 0, 0));
        assert_eq!(pixel(160, 2), (64, 64, 255));
        for y in [26, 36, 46] {
            assert_eq!(pixel(30, y), (255, 255, 255));
        }
        assert_eq!(pixel(30, 31), (64, 64, 255));
        assert_eq!(pixel(47, 26), (64, 64, 255));
        for index in 0..10 {
            assert_eq!(pixel(80 + index * 23, 36), (255, 255, 255));
            assert_eq!(pixel(80 + index * 23, 24), (64, 64, 255));
        }
    }
}
