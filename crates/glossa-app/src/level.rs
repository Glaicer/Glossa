//! Process-wide microphone level shared by audio capture and the recording overlay.

use std::sync::atomic::{AtomicU8, Ordering};

static PEAK_PERCENT: AtomicU8 = AtomicU8::new(0);

/// Records the peak absolute amplitude (0.0..=1.0) of one captured audio chunk.
///
/// Process-global on purpose: capture happens on the audio thread and the
/// overlay reads it from the GTK thread, and routing a meter through the
/// capture port trait would touch every implementation for one display value.
pub fn record_peak(peak: f32) {
    PEAK_PERCENT.store(peak_to_percent(peak), Ordering::Relaxed);
}

/// Latest recorded microphone level as a percentage (0..=100).
pub fn level_percent() -> u8 {
    PEAK_PERCENT.load(Ordering::Relaxed)
}

/// Maps a peak amplitude to a display percentage with a loudness curve so
/// normal speech fills most of the overlay waveform.
pub fn peak_to_percent(peak: f32) -> u8 {
    let peak = if peak.is_finite() {
        peak.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (peak.powf(0.6) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::{level_percent, peak_to_percent, record_peak};

    #[test]
    fn silent_peak_should_map_to_zero() {
        assert_eq!(peak_to_percent(0.0), 0);
    }

    #[test]
    fn full_scale_peak_should_map_to_one_hundred() {
        assert_eq!(peak_to_percent(1.0), 100);
    }

    #[test]
    fn out_of_range_peaks_should_be_clamped() {
        assert_eq!(peak_to_percent(-0.5), 0);
        assert_eq!(peak_to_percent(8.0), 100);
        assert_eq!(peak_to_percent(f32::NAN), 0);
    }

    #[test]
    fn louder_peaks_should_map_to_higher_levels() {
        let quiet = peak_to_percent(0.1);
        let loud = peak_to_percent(0.5);
        assert!(quiet < loud);
        assert!(loud < 100);
    }

    #[test]
    fn recorded_peak_should_be_readable_as_percent() {
        record_peak(1.0);
        assert_eq!(level_percent(), 100);
        record_peak(0.0);
        assert_eq!(level_percent(), 0);
    }
}
