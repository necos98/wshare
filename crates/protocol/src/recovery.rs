//! Loss recovery payloads: the viewer names the video datagrams it missed
//! so the host can send them again (see [`crate::control::ControlMessage::Nack`]).

use serde::{Deserialize, Serialize};

/// Most packet indices one [`Nack`] lists. A frame missing more than this
/// is asked for whole ([`Missing::Whole`]): the message stays small, and a
/// frame that lost so much is better resent entirely anyway.
pub const MAX_NACK_PACKETS: usize = 256;

/// Video datagrams of one frame the viewer did not receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nack {
    pub frame_id: u32,
    pub missing: Missing,
}

/// Which datagrams of the frame are missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Missing {
    /// Every datagram: none arrived (the viewer does not even know how many
    /// there are), or too many to list.
    Whole,
    /// These packet indices, ascending, at most [`MAX_NACK_PACKETS`]: a
    /// receiver looks at no more than that many.
    Packets(Vec<u16>),
}

impl Missing {
    /// The indices in `missing` (ascending), or [`Missing::Whole`] when they
    /// are too many to list.
    pub fn from_indices(missing: Vec<u16>) -> Self {
        if missing.is_empty() || missing.len() > MAX_NACK_PACKETS {
            Self::Whole
        } else {
            Self::Packets(missing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_list_is_kept_and_a_long_or_empty_one_asks_for_everything() {
        assert_eq!(
            Missing::from_indices(vec![1, 4]),
            Missing::Packets(vec![1, 4])
        );
        let all: Vec<u16> = (0..=MAX_NACK_PACKETS as u16).collect();
        assert_eq!(Missing::from_indices(all), Missing::Whole);
        let most: Vec<u16> = (0..MAX_NACK_PACKETS as u16).collect();
        assert!(matches!(Missing::from_indices(most), Missing::Packets(_)));
        assert_eq!(Missing::from_indices(Vec::new()), Missing::Whole);
    }
}
