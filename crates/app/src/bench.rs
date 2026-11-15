//! `wshare bench`: capture -> hardware encode -> file, with latency stats.
//! Exercises the host's media path without any network.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use codec::{
    Codec, DesktopSource, EncodedFrame, FrameSource, HwEncoder, MediaFoundation, VideoEncoder,
};
use pipeline::FramePacer;

use crate::cli::BenchOptions;

/// Accumulates what the encoder produced.
#[derive(Default)]
struct Report {
    submitted_at: HashMap<i64, Instant>,
    latencies_us: Vec<u64>,
    bytes: usize,
    keyframes: usize,
}

impl Report {
    fn on_submitted(&mut self, timestamp: i64, at: Instant) {
        self.submitted_at.insert(timestamp, at);
    }

    fn on_encoded(&mut self, frame: &EncodedFrame) {
        if let Some(t) = self.submitted_at.remove(&frame.timestamp) {
            self.latencies_us.push(t.elapsed().as_micros() as u64);
        }
        self.bytes += frame.data.len();
        self.keyframes += usize::from(frame.keyframe);
    }

    fn print(mut self, elapsed: Duration) -> Result<()> {
        let frames = self.latencies_us.len();
        if frames == 0 {
            bail!("no frames encoded: nothing changed on screen?");
        }
        let secs = elapsed.as_secs_f64();
        self.latencies_us.sort_unstable();
        let pct = |p: f64| self.latencies_us[((frames - 1) as f64 * p) as usize] as f64 / 1000.0;
        let avg = self.latencies_us.iter().sum::<u64>() as f64 / frames as f64 / 1000.0;
        println!();
        println!(
            "Frames:   {frames} ({:.1} fps average, {} keyframes)",
            frames as f64 / secs,
            self.keyframes
        );
        println!(
            "Bitrate:  {:.1} Mbps average",
            self.bytes as f64 * 8.0 / secs / 1e6
        );
        println!(
            "Encode latency (submit -> bitstream): avg {avg:.2} ms, p50 {:.2}, p95 {:.2}, max {:.2}",
            pct(0.5),
            pct(0.95),
            pct(1.0)
        );
        Ok(())
    }
}

pub fn run(options: BenchOptions) -> Result<()> {
    let _mf = MediaFoundation::start()?;
    let stream = &options.stream;
    let out = options.out.clone().unwrap_or_else(|| match stream.codec {
        Codec::H264 => "capture.h264".into(),
        Codec::Hevc => "capture.h265".into(),
    });

    let mut source = DesktopSource::open(stream.monitor, stream.fps)?;
    let gpu = source.gpu().clone();
    let config = stream.encoder_config(source.width(), source.height());
    let mut encoder = HwEncoder::new(&gpu.device, gpu.adapter_luid, &config)?;
    println!("GPU:      {}", gpu.adapter_name);
    println!("Encoder:  {}", encoder.name());
    println!(
        "Stream:   {}x{} @ {} fps, {} Mbps {:?} -> {out}",
        config.width, config.height, stream.fps, stream.mbps, stream.codec
    );
    println!(
        "Recording {} s: move windows or play a video to generate frames...",
        options.seconds
    );

    let mut file = BufWriter::new(File::create(&out)?);
    let mut report = Report::default();
    let mut encoded = VecDeque::new();
    let mut write_out = |encoded: &mut VecDeque<EncodedFrame>, report: &mut Report| -> Result<()> {
        while let Some(frame) = encoded.pop_front() {
            report.on_encoded(&frame);
            file.write_all(&frame.data)?;
        }
        Ok(())
    };

    let mut pacer = FramePacer::new(stream.fps);
    let start = Instant::now();
    let end = start + Duration::from_secs(options.seconds);
    while Instant::now() < end {
        encoder.poll(!encoder.can_accept(), &mut encoded)?;
        write_out(&mut encoded, &mut report)?;

        let now = Instant::now();
        if !pacer.should_submit(now) && source.wait_for_change(pacer.capture_timeout(now))? {
            pacer.mark_dirty();
        }
        let now = Instant::now();
        if pacer.should_submit(now) && encoder.can_accept() {
            let frame = source.current_frame()?;
            let timestamp = (now - start).as_nanos() as i64 / 100;
            report.on_submitted(timestamp, now);
            encoder.submit(&frame, timestamp)?;
            pacer.on_submitted(now);
        }
    }
    encoder.finish(&mut encoded)?;
    write_out(&mut encoded, &mut report)?;
    file.flush()?;
    report.print(start.elapsed())?;
    println!("Saved {out} - play it with: ffplay {out}");
    Ok(())
}
