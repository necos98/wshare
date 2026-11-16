//! File transfers between the two PCs of a session, both ways: pushed
//! (dropped on the viewer window, "Invia file…") into the receiver's
//! download folder, or copied on one PC and pasted on the other (see
//! `clipboard`, which offers and installs the files, and
//! [`hub::TransferHub`], which moves them). The wire format is
//! `protocol::transfer`; every file has a QUIC stream of its own.
//!
//! Layers, from the pure to the ones doing I/O:
//! - [`safe_path`] and [`names`]: the checks every received path passes,
//!   and the free name (`nome (1).ext`) for one already taken.
//! - [`plan`] and [`receiving`]: where a pushed list lands, and its state.
//! - [`listing`]: the list of what this PC sends, walked from the disk.
//! - [`part_file`]: a file being received, as `.part` until complete, and
//!   [`free_rename`], which then gives it a free name, replacing nothing.
//! - [`events`] and [`board`]: what the front-ends hear and show.
//! - [`registry`], [`shared`], [`streams`], [`push`], [`planning`],
//!   [`paste`], [`revocation`] and [`hub`]: the transfers of one session
//!   and their tasks.

pub mod board;
pub mod events;
mod free_rename;
mod hub;
pub mod listing;
pub mod names;
mod part_file;
mod paste;
pub mod plan;
mod planning;
mod push;
mod receiving;
mod registry;
mod revocation;
pub mod safe_path;
mod shared;
mod streams;
pub mod terminal;

pub use board::{ActiveTransfer, Ended, Summary, TransferBoard};
pub use events::{
    Direction, Failure, Kind, Outcome, TransferCommand, TransferEvent, TransferInfo,
    TransferObserver,
};
pub use hub::{HubConfig, TransferHub};
pub use paste::PasteLink;
#[cfg(test)]
pub use paste::PasteRequest;
pub use registry::{Chunk, Role};
pub use shared::TransferPermissions;
