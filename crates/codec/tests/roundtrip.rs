//! Encoder -> decoder roundtrip on the real GPU.
//!
//! Run with `cargo test -p codec -- --ignored`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use codec::{
    BgraToNv12, Codec, EncodedFrame, EncoderConfig, HwDecoder, HwEncoder, MediaFoundation,
    VideoDecoder, VideoEncoder,
};
use gpu::{Gpu, GpuFrame};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

const WIDTH: u32 = 1920;
/// Not a multiple of 16: checks that the decoder reports the display size,
/// not the padded coded size (1088).
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;

/// Distinct colours, one per frame, so a picture arriving a frame late
/// would show the previous frame's colour.
const COLOURS: [[u8; 3]; 6] = [
    [200, 40, 40],
    [40, 200, 40],
    [40, 40, 200],
    [128, 128, 128],
    [230, 230, 30],
    [20, 160, 220],
];

#[test]
#[ignore = "requires GPU"]
fn h264_roundtrip_is_immediate_and_colour_accurate() -> Result<()> {
    roundtrip(Codec::H264)
}

#[test]
#[ignore = "requires GPU"]
fn hevc_roundtrip_is_immediate_and_colour_accurate() -> Result<()> {
    roundtrip(Codec::Hevc)
}

fn roundtrip(codec: Codec) -> Result<()> {
    let _mf = MediaFoundation::start()?;
    let gpu = Gpu::default_adapter()?;
    let source = create_bgra(&gpu)?;
    let mut converter = BgraToNv12::new(&gpu, &source, WIDTH, HEIGHT, FPS)?;
    let config = EncoderConfig {
        codec,
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        bitrate_bps: 20_000_000,
    };
    let mut encoder = HwEncoder::new(&gpu.device, gpu.adapter_luid, &config)?;
    let mut decoder = HwDecoder::new(&gpu, codec, WIDTH, HEIGHT)?;
    println!("{codec:?}: {} -> {}", encoder.name(), decoder.name());

    let mut decode_times = Vec::new();
    for (index, colour) in COLOURS.iter().enumerate() {
        fill_bgra(&gpu, &source, *colour);
        let timestamp = index as i64 * 10_000_000 / FPS as i64;
        let unit = encode_one(&mut encoder, &converter.convert()?, timestamp)?;
        assert_eq!(unit.timestamp, timestamp);
        if index == 0 {
            assert!(unit.keyframe, "first frame must be a keyframe");
        }

        let started = Instant::now();
        let frame = decoder
            .decode(&unit.data, unit.timestamp)?
            .with_context(|| format!("frame {index} was held back by the decoder"))?;
        decode_times.push(started.elapsed());

        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT), "display size");
        assert_eq!(frame.timestamp, timestamp, "decoder returned another frame");
        let decoded = read_centre_rgb(&gpu, &frame)?;
        assert_close(decoded, *colour, index);
    }
    assert_eq!(decoder.display_size(), Some((WIDTH, HEIGHT)));
    let steady = &decode_times[1..];
    println!(
        "{codec:?} decode: first {:?}, steady avg {:?}, max {:?}",
        decode_times[0],
        steady.iter().sum::<Duration>() / steady.len() as u32,
        steady.iter().max().unwrap()
    );
    Ok(())
}

/// Submits one frame and waits for its bitstream.
fn encode_one(
    encoder: &mut HwEncoder,
    nv12: &ID3D11Texture2D,
    timestamp: i64,
) -> Result<EncodedFrame> {
    let mut out = VecDeque::new();
    while !encoder.can_accept() {
        encoder.poll(true, &mut out)?;
    }
    encoder.submit(nv12, timestamp)?;
    for _ in 0..100 {
        if let Some(frame) = out.pop_front() {
            return Ok(frame);
        }
        encoder.poll(true, &mut out)?;
    }
    anyhow::bail!("encoder produced no output")
}

fn create_bgra(gpu: &Gpu) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: WIDTH,
        Height: HEIGHT,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut texture))?
    };
    texture.context("BGRA texture")
}

fn fill_bgra(gpu: &Gpu, texture: &ID3D11Texture2D, [r, g, b]: [u8; 3]) {
    let pixels: Vec<u8> = [b, g, r, 255].repeat((WIDTH * HEIGHT) as usize);
    unsafe {
        gpu.context
            .UpdateSubresource(texture, 0, None, pixels.as_ptr().cast(), WIDTH * 4, 0);
    }
}

/// Copies the frame's slice to a staging texture and converts the centre
/// pixel from BT.709 limited-range YCbCr to RGB.
fn read_centre_rgb(gpu: &Gpu, frame: &GpuFrame) -> Result<[u8; 3]> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { frame.texture.GetDesc(&mut desc) };
    desc.ArraySize = 1;
    desc.MipLevels = 1;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    unsafe {
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut staging))?
    };
    let staging: ID3D11Texture2D = staging.context("staging texture")?;
    let (x, y) = (frame.width as usize / 2, frame.height as usize / 2);
    unsafe {
        gpu.context.CopySubresourceRegion(
            &staging,
            0,
            0,
            0,
            0,
            &frame.texture,
            frame.subresource,
            None,
        );
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let pitch = mapped.RowPitch as usize;
        let data = mapped.pData as *const u8;
        let luma = *data.add(y * pitch + x);
        let chroma = data.add(pitch * desc.Height as usize + (y / 2) * pitch + (x / 2) * 2);
        let (cb, cr) = (*chroma, *chroma.add(1));
        gpu.context.Unmap(&staging, 0);
        Ok(bt709_limited_to_rgb(luma, cb, cr))
    }
}

fn bt709_limited_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = (y as f32 - 16.0) / 219.0;
    let cb = (cb as f32 - 128.0) / 224.0;
    let cr = (cr as f32 - 128.0) / 224.0;
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    [
        to_u8(y + 1.5748 * cr),
        to_u8(y - 0.1873 * cb - 0.4681 * cr),
        to_u8(y + 1.8556 * cb),
    ]
}

fn assert_close(actual: [u8; 3], expected: [u8; 3], index: usize) {
    let off = actual.iter().zip(expected).any(|(a, e)| a.abs_diff(e) > 12);
    assert!(
        !off,
        "frame {index}: decoded {actual:?}, expected {expected:?}"
    );
}
