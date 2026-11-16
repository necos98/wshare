//! Tests of [`super::RateAdaptation`] and of the samples it takes.

use std::time::Duration;

use anyhow::anyhow;
use pipeline::bitrate::DECREASE_FACTOR;

use super::*;

const MAX: u32 = 20_000_000;
const CUT: u32 = (MAX as f64 * DECREASE_FACTOR) as u32;

fn counters(sent: u64, lost: u64, skipped: u64) -> Counters {
    Counters {
        transport: TransportStats {
            rtt: Duration::from_millis(2),
            min_rtt: Duration::from_millis(1),
            congestion_window: 100_000,
            lost_packets: lost,
            sent_packets: sent,
            ..Default::default()
        },
        frames_skipped: skipped,
    }
}

fn accept(_: u32) -> Result<()> {
    Ok(())
}

#[test]
fn reports_add_up_until_taken() {
    let reports = ViewerReports::default();
    let report = |frames_lost, frames_late| ReceptionReport {
        frames_lost,
        frames_late,
    };
    reports.record(&report(1, 2));
    reports.record(&report(3, 4));
    assert_eq!(reports.take(), report(4, 6));
    assert_eq!(reports.take(), ReceptionReport::default());
    reports.record(&report(u32::MAX, 0));
    reports.record(&report(5, 0));
    assert_eq!(reports.take().frames_lost, u32::MAX);
}

#[test]
fn samples_are_the_difference_of_the_counters() {
    let viewer = ReceptionReport {
        frames_lost: 1,
        frames_late: 2,
    };
    let sample = network_sample(&counters(100, 1, 3), &counters(350, 6, 4), viewer);
    assert_eq!(sample.packets_sent, 250);
    assert_eq!(sample.packets_lost, 5);
    assert_eq!(sample.frames_skipped, 1);
    assert_eq!(sample.viewer, viewer);
    assert_eq!(sample.rtt, Duration::from_millis(2));
    assert_eq!(sample.min_rtt, Duration::from_millis(1));
}

#[test]
fn nothing_is_read_before_a_sample_is_due() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let early = start + SAMPLE_INTERVAL / 2;
    let change = rate.poll(early, || panic!("read too early"), accept);
    assert!(change.is_none());
}

#[test]
fn losses_reported_by_the_viewer_lower_the_encoder_rate() {
    let start = Instant::now();
    let reports = Arc::new(ViewerReports::default());
    let mut rate = RateAdaptation::new(MAX, reports.clone(), counters(0, 0, 0), start);
    reports.record(&ReceptionReport {
        frames_lost: 5,
        frames_late: 0,
    });
    let mut applied = None;
    let change = rate.poll(
        start + SAMPLE_INTERVAL,
        || counters(1_000, 0, 0),
        |bps| {
            applied = Some(bps);
            Ok(())
        },
    );
    assert!(matches!(change, Some(RateChange::Retargeted(CUT))));
    assert_eq!(applied, Some(CUT));
}

#[test]
fn transport_losses_lower_the_rate_and_calm_brings_it_back() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let mut now = start + SAMPLE_INTERVAL;
    let change = rate.poll(now, || counters(1_000, 100, 0), accept);
    assert!(matches!(change, Some(RateChange::Retargeted(CUT))));
    let mut target = CUT;
    for _ in 0..1_000 {
        now += SAMPLE_INTERVAL;
        if let Some(RateChange::Retargeted(bps)) =
            rate.poll(now, || counters(1_000, 100, 0), accept)
        {
            assert!(bps > target && bps <= MAX, "{bps} after {target}");
            target = bps;
        }
    }
    assert_eq!(target, MAX);
}

#[test]
fn the_sound_lowers_the_ceiling_and_the_target_with_it() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let lower = pipeline::video_ceiling(MAX, 1_700_000);
    let mut applied = None;
    let change = rate.set_ceiling(lower, |bps| {
        applied = Some(bps);
        Ok(())
    });
    assert!(matches!(change, Some(RateChange::Retargeted(bps)) if bps == lower));
    assert_eq!(applied, Some(lower));
    assert!(rate.set_ceiling(lower, accept).is_none(), "nothing changed");
    assert!(
        rate.set_ceiling(MAX, accept).is_none(),
        "climbed back later"
    );
}

#[test]
fn a_refused_ceiling_fixes_the_rate_like_a_refused_sample() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let change = rate.set_ceiling(MAX / 2, |_| Err(anyhow!("refused")));
    assert!(matches!(change, Some(RateChange::Fixed(_))));
    assert!(rate.set_ceiling(MAX / 4, accept).is_none());
}

#[test]
fn a_refused_rate_is_reported_once_and_adaptation_stops() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let mut now = start;
    let mut changes = Vec::new();
    for sent in 1..20 {
        now += SAMPLE_INTERVAL;
        let lossy = counters(sent * 1_000, sent * 100, 0);
        changes.extend(rate.poll(now, || lossy, |_| Err(anyhow!("refused"))));
    }
    assert_eq!(changes.len(), 1);
    assert!(matches!(&changes[0], RateChange::Fixed(e) if e.to_string() == "refused"));
}

/// Polls `rate` once with lossy counters and returns what it led to.
fn congest(rate: &mut RateAdaptation, sample: u64, now: Instant) -> Option<RateChange> {
    rate.poll(now, || counters(sample * 1_000, sample * 100, 0), accept)
}

/// A rate that congestion brought down to `CUT * DECREASE_FACTOR`.
fn settled_low(start: Instant) -> (RateAdaptation, Instant) {
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let mut now = start + SAMPLE_INTERVAL;
    congest(&mut rate, 1, now);
    now += pipeline::bitrate::DECREASE_HOLD;
    congest(&mut rate, 2, now);
    (rate, now)
}

const SETTLED: u32 = (CUT as f64 * DECREASE_FACTOR) as u32;

#[test]
fn a_restarted_stream_starts_from_the_settled_rate_not_the_maximum() {
    let (mut rate, _) = settled_low(Instant::now());
    let mut applied = None;
    let change = rate.resume(MAX, |bps| {
        applied = Some(bps);
        Ok(())
    });
    assert!(matches!(change, RateChange::Retargeted(SETTLED)));
    assert_eq!(applied, Some(SETTLED), "the new encoder is told at once");
}

#[test]
fn a_restarted_stream_is_limited_to_its_new_maximum() {
    let (mut rate, now) = settled_low(Instant::now());
    let lower = SETTLED / 2;
    assert!(matches!(
        rate.resume(lower, accept),
        RateChange::Retargeted(bps) if bps == lower
    ));
    // Back to a higher quality: still no jump, the target climbs as usual.
    assert!(matches!(
        rate.resume(MAX, accept),
        RateChange::Retargeted(bps) if bps == lower
    ));
    let next = now + SAMPLE_INTERVAL;
    assert!(
        rate.poll(next, || counters(3_000, 200, 0), accept)
            .is_none()
    );
}

#[test]
fn a_restart_gives_the_new_encoder_its_chance_after_a_refusal() {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
    let change = rate.set_ceiling(MAX / 2, |_| Err(anyhow!("refused")));
    assert!(matches!(change, Some(RateChange::Fixed(_))));
    assert!(matches!(
        rate.resume(MAX, accept),
        RateChange::Retargeted(bps) if bps == MAX / 2
    ));
    let change = congest(&mut rate, 1, start + SAMPLE_INTERVAL);
    assert!(
        matches!(change, Some(RateChange::Retargeted(_))),
        "adapting again"
    );
}

#[test]
fn a_file_stream_open_at_either_end_of_the_interval_or_in_between_counts() {
    let streams = |open, opened| {
        let mut counters = counters(0, 0, 0);
        counters.transport.file_streams_open = open;
        counters.transport.file_streams_opened = opened;
        counters
    };
    let viewer = ReceptionReport::default();
    let transfers = |before, after| network_sample(&before, &after, viewer).file_transfers;
    assert!(!transfers(streams(0, 0), streams(0, 0)));
    assert!(!transfers(streams(0, 3), streams(0, 3)), "all over before");
    assert!(transfers(streams(0, 0), streams(1, 1)), "started");
    assert!(transfers(streams(1, 1), streams(0, 1)), "ended");
    assert!(transfers(streams(0, 1), streams(0, 2)), "started and ended");
}

/// A path of `capacity_bps` shared by the video and a large file sent as
/// fast as the path allows. quinn sends the video's datagrams first: the
/// video gets up to the capacity, the file the rest, and with the file
/// keeping the path full the whole connection loses packets and queues up
/// for as long as it lasts. The video's own send backlog grows only while
/// its target is above the capacity.
struct SharedPath {
    capacity_bps: u32,
    file_open: bool,
    counters: Counters,
}

/// Packets per sample interval; the file overruns the bottleneck's queue,
/// so 5% of them are lost.
const PACKETS_PER_SAMPLE: u64 = 1_000;
const LOST_PER_SAMPLE: u64 = 50;
/// Frames skipped per sample while the video is faster than the path.
const SKIPPED_PER_SAMPLE: u64 = 5;

impl SharedPath {
    fn new(capacity_bps: u32, file_open: bool) -> Self {
        let mut counters = counters(0, 0, 0);
        counters.transport.rtt = Duration::from_millis(120);
        counters.transport.file_streams_open = u64::from(file_open);
        counters.transport.file_streams_opened = u64::from(file_open);
        Self {
            capacity_bps,
            file_open,
            counters,
        }
    }

    /// One interval with the video at `video_bps`; the file's share.
    fn carry(&mut self, video_bps: u32) -> u32 {
        let transport = &mut self.counters.transport;
        transport.sent_packets += PACKETS_PER_SAMPLE;
        transport.lost_packets += LOST_PER_SAMPLE;
        if video_bps > self.capacity_bps {
            self.counters.frames_skipped += SKIPPED_PER_SAMPLE;
        }
        let video = video_bps.min(self.capacity_bps);
        if self.file_open {
            self.capacity_bps - video
        } else {
            0
        }
    }
}

/// Runs the video over `path` for `duration`: the video's target and the
/// file's share of each interval.
fn stream_over(path: &mut SharedPath, duration: Duration) -> Vec<(u32, u32)> {
    let start = Instant::now();
    let mut rate = RateAdaptation::new(MAX, Arc::default(), path.counters, start);
    let mut target = MAX;
    let mut shares = Vec::new();
    let mut now = start;
    while now < start + duration {
        now += SAMPLE_INTERVAL;
        shares.push((target, path.carry(target)));
        let counters = path.counters;
        if let Some(RateChange::Retargeted(bps)) = rate.poll(now, || counters, accept) {
            target = bps;
        }
    }
    shares
}

#[test]
fn a_concurrent_file_does_not_bring_the_video_down_to_the_floor() {
    const CAPACITY: u32 = 8_000_000;
    let mut path = SharedPath::new(CAPACITY, true);
    let shares = stream_over(&mut path, Duration::from_secs(60));

    // Past the first cuts from the maximum, the video stays close to what
    // the path leaves it, far from the floor.
    let settled = &shares[shares.len() / 4..];
    let lowest = settled.iter().map(|&(video, _)| video).min().unwrap();
    let near_capacity = (f64::from(CAPACITY) * DECREASE_FACTOR * DECREASE_FACTOR) as u32;
    assert!(lowest >= near_capacity, "{lowest} bps");
    assert!(lowest > pipeline::bitrate::MIN_BITRATE_BPS);

    // The file takes the rest of the path, never less than nothing.
    for &(video, file) in settled {
        assert_eq!(video.min(CAPACITY) + file, CAPACITY);
    }
    let file_average = settled
        .iter()
        .map(|&(_, file)| u64::from(file))
        .sum::<u64>()
        / settled.len() as u64;
    assert!(file_average > 0, "the file moves on");
}

#[test]
fn without_files_the_same_losses_do_bring_the_rate_down() {
    // The connection's losses and round trip count when no file explains
    // them: the controller backs off all the way.
    let mut path = SharedPath::new(8_000_000, false);
    let shares = stream_over(&mut path, Duration::from_secs(30));
    let (last, _) = *shares.last().unwrap();
    assert_eq!(last, pipeline::bitrate::MIN_BITRATE_BPS);
}
