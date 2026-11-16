//! Opening capture and encoder for a stream, on the streaming thread: at
//! the start of a session and again whenever the stream starts over (the
//! viewer chose another monitor or quality, the monitor changed mode).

use std::time::Duration;

use anyhow::{Result, bail};
use capture::ModeChanged;
use codec::{DesktopSource, HwEncoder};
use protocol::control::StreamInfo;

use crate::options::StreamOptions;
use crate::wire::to_wire;

/// Attempts at opening a stream that starts over. Right after a mode
/// change the monitor may refuse to be captured for a moment.
const REOPEN_ATTEMPTS: u32 = 10;
/// Pause between two of those attempts: together about two seconds.
const REOPEN_DELAY: Duration = Duration::from_millis(200);

/// Capture and encoder of one stream, and how the viewer is told about it.
pub(super) struct OpenStream {
    pub(super) source: DesktopSource,
    pub(super) encoder: HwEncoder,
    pub(super) info: StreamInfo,
}

/// Opens the monitor of `options` and an encoder for its current size, on
/// the monitor's adapter. Needs Media Foundation started on this thread.
pub(super) fn open_stream(options: &StreamOptions) -> Result<OpenStream> {
    let source = DesktopSource::open(options.monitor, options.fps)?;
    let gpu = source.gpu().clone();
    let config = options.encoder_config(source.width(), source.height());
    let encoder = HwEncoder::new(&gpu.device, gpu.adapter_luid, &config)?;
    let info = StreamInfo {
        codec: to_wire(options.codec),
        width: config.width,
        height: config.height,
        fps: options.fps,
        encoder: encoder.name().to_string(),
    };
    Ok(OpenStream {
        source,
        encoder,
        info,
    })
}

/// [`open_stream`] for a stream that starts over, tried a few times while
/// the monitor settles. Gives up early once `stopped`.
pub(super) fn reopen_stream(
    options: &StreamOptions,
    stopped: impl Fn() -> bool,
) -> Result<OpenStream> {
    retrying(REOPEN_ATTEMPTS, REOPEN_DELAY, stopped, || {
        open_stream(options)
    })
}

/// Runs `open` up to `attempts` times, pausing `delay` between two, until
/// it succeeds; the last error otherwise.
fn retrying<T>(
    attempts: u32,
    delay: Duration,
    stopped: impl Fn() -> bool,
    mut open: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut last = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            if stopped() {
                break;
            }
            std::thread::sleep(delay);
        }
        match open() {
            Ok(opened) => return Ok(opened),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e),
        None => bail!("no attempt made to open the stream"),
    }
}

/// The stream failed because the monitor changed resolution or pixel
/// format: it can start over in the new mode instead of ending.
pub(super) fn is_mode_change(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<ModeChanged>().is_some())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use anyhow::{Context, anyhow};

    use super::*;

    const NO_DELAY: Duration = Duration::ZERO;

    #[test]
    fn retrying_stops_at_the_first_success() {
        let calls = Cell::new(0);
        let opened = retrying(
            5,
            NO_DELAY,
            || false,
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(anyhow!("busy"))
                } else {
                    Ok(calls.get())
                }
            },
        );
        assert_eq!(opened.unwrap(), 3);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn retrying_returns_the_last_error_when_every_attempt_fails() {
        let calls = Cell::new(0);
        let failed = retrying(
            4,
            NO_DELAY,
            || false,
            || {
                calls.set(calls.get() + 1);
                Err::<(), _>(anyhow!("attempt {}", calls.get()))
            },
        );
        assert_eq!(failed.unwrap_err().to_string(), "attempt 4");
    }

    #[test]
    fn retrying_gives_up_once_stopped() {
        let calls = Cell::new(0);
        let failed = retrying(
            10,
            NO_DELAY,
            || true,
            || {
                calls.set(calls.get() + 1);
                Err::<(), _>(anyhow!("busy"))
            },
        );
        assert!(failed.is_err());
        assert_eq!(calls.get(), 1, "only the first attempt");
    }

    #[test]
    fn zero_attempts_is_an_error() {
        assert!(retrying(0, NO_DELAY, || false, || Ok(())).is_err());
    }

    #[test]
    fn a_mode_change_is_found_under_any_context() {
        let change = anyhow::Error::new(ModeChanged {
            width: 1280,
            height: 720,
        });
        assert!(is_mode_change(&change));
        let wrapped = Err::<(), _>(change).context("capturing").unwrap_err();
        assert!(is_mode_change(&wrapped));
        assert!(!is_mode_change(&anyhow!("encoder lost")));
    }
}
