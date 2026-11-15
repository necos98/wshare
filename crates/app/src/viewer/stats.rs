//! The statistics overlay's content: rates measured over refresh intervals
//! and the text lines shown. Pure, so the numbers are unit-tested.

use std::time::{Duration, Instant};

use pipeline::ViewerStats;
use protocol::control::StreamInfo;

use crate::text;

/// Cumulative counters sampled by the render loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub bytes_received: u64,
    pub frames_presented: u64,
}

/// Rates over the last measurement interval.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub fps: f64,
    pub mbps: f64,
}

/// Turns cumulative totals into per-second rates between samples.
#[derive(Default)]
pub struct RateMeter {
    last: Option<(Instant, Totals)>,
    rates: Rates,
}

impl RateMeter {
    /// Records a sample; returns the rates since the previous one (zero on
    /// the first sample).
    pub fn sample(&mut self, now: Instant, totals: Totals) -> Rates {
        if let Some((then, before)) = self.last {
            let secs = now.saturating_duration_since(then).as_secs_f64();
            if secs > 0.0 {
                let frames = totals
                    .frames_presented
                    .saturating_sub(before.frames_presented);
                let bytes = totals.bytes_received.saturating_sub(before.bytes_received);
                self.rates = Rates {
                    fps: frames as f64 / secs,
                    mbps: bytes as f64 * 8.0 / secs / 1e6,
                };
            }
        }
        self.last = Some((now, totals));
        self.rates
    }
}

/// Average time spent decoding and presenting a frame.
#[derive(Default)]
pub struct FrameTimer {
    total: Duration,
    frames: u32,
    average: Option<Duration>,
}

impl FrameTimer {
    pub fn record(&mut self, elapsed: Duration) {
        self.total += elapsed;
        self.frames += 1;
    }

    /// Average since the last call (kept if no frame arrived meanwhile).
    pub fn take_average(&mut self) -> Option<Duration> {
        if self.frames > 0 {
            self.average = Some(self.total / self.frames);
            self.total = Duration::ZERO;
            self.frames = 0;
        }
        self.average
    }
}

/// What the overlay shows besides the measured values.
pub struct Labels<'a> {
    pub stream: &'a StreamInfo,
    pub decoder: &'a str,
}

/// Width of the label column of the overlay.
const LABEL_WIDTH: usize = 13;

/// One `label  value` row, the values aligned.
fn row(label: &str, value: String) -> String {
    format!("{label:<LABEL_WIDTH$}{value}")
}

/// The overlay text (Italian, like the rest of the UI).
pub fn overlay_lines(
    labels: &Labels<'_>,
    rates: Rates,
    stats: &ViewerStats,
    rtt: Duration,
    frame_time: Option<Duration>,
) -> Vec<String> {
    let ms = |d: Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
    let stream = labels.stream;
    vec![
        format!(
            "{}x{} {:?} - max {} fps",
            stream.width, stream.height, stream.codec, stream.fps
        ),
        row(text::STAT_FPS, format!("{:.0}", rates.fps)),
        row(text::STAT_BITRATE, format!("{:.1} Mbit/s", rates.mbps)),
        row(text::STAT_RTT, ms(rtt)),
        row(
            text::STAT_DECODE,
            frame_time.map_or_else(|| "-".to_string(), ms),
        ),
        text::stats_losses(
            stats.frames_lost,
            stats.frames_skipped,
            stats.keyframes_requested,
        ),
        row(text::STAT_ENCODER, stream.encoder.clone()),
        row(text::STAT_DECODER, labels.decoder.to_string()),
        text::STATS_HOTKEYS.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use protocol::control::VideoCodec;

    use super::*;

    #[test]
    fn first_sample_has_no_rate_then_rates_follow_deltas() {
        let t0 = Instant::now();
        let mut meter = RateMeter::default();
        assert_eq!(meter.sample(t0, Totals::default()), Rates::default());
        let rates = meter.sample(
            t0 + Duration::from_millis(500),
            Totals {
                bytes_received: 1_000_000,
                frames_presented: 30,
            },
        );
        assert!((rates.fps - 60.0).abs() < 1e-9);
        assert!((rates.mbps - 16.0).abs() < 1e-9);
    }

    #[test]
    fn same_instant_keeps_previous_rates() {
        let t0 = Instant::now();
        let mut meter = RateMeter::default();
        meter.sample(t0, Totals::default());
        let t1 = t0 + Duration::from_secs(1);
        let rates = meter.sample(
            t1,
            Totals {
                bytes_received: 125_000,
                frames_presented: 10,
            },
        );
        assert_eq!(
            meter.sample(
                t1,
                Totals {
                    bytes_received: 999,
                    frames_presented: 99
                }
            ),
            rates
        );
    }

    #[test]
    fn frame_timer_averages_and_keeps_last_value_when_idle() {
        let mut timer = FrameTimer::default();
        assert_eq!(timer.take_average(), None);
        timer.record(Duration::from_millis(1));
        timer.record(Duration::from_millis(3));
        assert_eq!(timer.take_average(), Some(Duration::from_millis(2)));
        assert_eq!(timer.take_average(), Some(Duration::from_millis(2)));
    }

    #[test]
    fn overlay_shows_every_figure() {
        let stream = StreamInfo {
            codec: VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            encoder: "Intel QSV".into(),
        };
        let stats = ViewerStats {
            frames_lost: 2,
            frames_skipped: 3,
            ..Default::default()
        };
        let lines = overlay_lines(
            &Labels {
                stream: &stream,
                decoder: "MS H264",
            },
            Rates {
                fps: 59.6,
                mbps: 12.34,
            },
            &stats,
            Duration::from_micros(800),
            Some(Duration::from_micros(1500)),
        );
        let text = lines.join("\n");
        for expected in [
            "1920x1080 H264",
            "60",
            "12.3 Mbit/s",
            "0.8 ms",
            "1.5 ms",
            "Persi 2",
            "Scartati 3",
            "Intel QSV",
            "MS H264",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }
}
