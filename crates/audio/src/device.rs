//! The default playback device and the audio clients opened on it.

use anyhow::{Context, Result};
use protocol::audio::{CHANNELS, FRAME_BYTES, SAMPLE_RATE};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};

/// WASAPI durations are in 100 ns units.
const HNS_PER_MILLISECOND: i64 = 10_000;

/// Finds the default playback device (the "Console" role, the one the
/// volume icon shows).
pub struct DefaultDevice {
    enumerator: IMMDeviceEnumerator,
}

impl DefaultDevice {
    pub fn new() -> Result<Self> {
        // SAFETY: plain COM activation; COM is initialised on this thread
        // (see `ComApartment`).
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .context("creating the audio device enumerator")?;
        Ok(Self { enumerator })
    }

    /// The default device now; fails when there is none.
    pub(crate) fn device(&self) -> Result<IMMDevice> {
        // SAFETY: valid enumerator, plain call.
        unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .context("no audio playback device")
    }

    /// The identifier of the default device now, to notice a change.
    pub fn id(&self) -> Result<String> {
        device_id(&self.device()?)
    }
}

pub(crate) fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: `GetId` returns a string allocated with the COM allocator,
    // read once and freed here.
    unsafe {
        let id = device.GetId().context("reading the audio device id")?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast_const().cast()));
        text.context("the audio device id is not UTF-16")
    }
}

/// The wire format: 48 kHz, stereo, 16-bit PCM.
fn wire_format() -> WAVEFORMATEX {
    let block_align = FRAME_BYTES as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

/// A shared-mode client on `device` in the wire format, converted by
/// Windows from or to the device's own; `flags` adds e.g. loopback.
pub(crate) fn open_client(device: &IMMDevice, flags: u32, buffer_ms: i64) -> Result<IAudioClient> {
    // SAFETY: valid device, plain activation.
    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.context("activating the audio client")?;
    let format = wire_format();
    let flags =
        flags | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // SAFETY: `format` outlives the call; shared mode takes no periodicity.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            buffer_ms * HNS_PER_MILLISECOND,
            0,
            &format,
            None,
        )
    }
    .context("initialising the audio client (48 kHz stereo 16-bit)")?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_format_is_the_wire_format() {
        let format = wire_format();
        let (rate, align, bytes) = (
            format.nSamplesPerSec,
            format.nBlockAlign,
            format.nAvgBytesPerSec,
        );
        assert_eq!(rate, 48_000);
        assert_eq!(usize::from(align), FRAME_BYTES);
        assert_eq!(bytes, 192_000);
    }
}
