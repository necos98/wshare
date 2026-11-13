//! Conversions between the media crates' types and the wire protocol, kept
//! here so neither side depends on the other.

use capture::{PointerChanges, PointerImage};
use codec::Codec;
use protocol::control::{ControlMessage, VideoCodec};
use protocol::input::CursorShape;

pub fn to_wire(codec: Codec) -> VideoCodec {
    match codec {
        Codec::H264 => VideoCodec::H264,
        Codec::Hevc => VideoCodec::Hevc,
    }
}

pub fn from_wire(codec: VideoCodec) -> Codec {
    match codec {
        VideoCodec::H264 => Codec::H264,
        VideoCodec::Hevc => Codec::Hevc,
    }
}

pub fn cursor_shape(image: PointerImage) -> CursorShape {
    CursorShape {
        id: image.id,
        width: image.width,
        height: image.height,
        hotspot_x: image.hotspot_x,
        hotspot_y: image.hotspot_y,
        rgba: image.rgba,
    }
}

/// Control messages announcing pointer changes, shape first so a pointer
/// that reappears does so with its new look.
pub fn pointer_messages(changes: PointerChanges) -> Vec<ControlMessage> {
    let mut messages = Vec::new();
    if let Some(image) = changes.shape {
        messages.push(ControlMessage::Cursor(cursor_shape(image)));
    }
    if let Some(visible) = changes.visible {
        messages.push(ControlMessage::CursorVisible(visible));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> PointerImage {
        PointerImage {
            id: 42,
            width: 2,
            height: 1,
            hotspot_x: 1,
            hotspot_y: 0,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
        }
    }

    #[test]
    fn codec_mapping_roundtrips() {
        for codec in [Codec::H264, Codec::Hevc] {
            assert_eq!(from_wire(to_wire(codec)), codec);
        }
    }

    #[test]
    fn cursor_shape_copies_every_field_and_is_valid() {
        let shape = cursor_shape(image());
        assert_eq!((shape.id, shape.width, shape.height), (42, 2, 1));
        assert_eq!((shape.hotspot_x, shape.hotspot_y), (1, 0));
        assert!(shape.is_valid());
    }

    #[test]
    fn pointer_messages_in_order_and_only_for_changes() {
        assert!(pointer_messages(PointerChanges::default()).is_empty());
        let both = pointer_messages(PointerChanges { shape: Some(image()), visible: Some(true) });
        assert!(matches!(both[0], ControlMessage::Cursor(_)));
        assert_eq!(both[1], ControlMessage::CursorVisible(true));
        let hidden = pointer_messages(PointerChanges { shape: None, visible: Some(false) });
        assert_eq!(hidden, vec![ControlMessage::CursorVisible(false)]);
    }
}
