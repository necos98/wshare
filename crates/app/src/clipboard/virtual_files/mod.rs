//! Files copied on the other PC, pasteable here with Ctrl+V in Explorer as
//! if they were local: virtual files whose bytes travel only when pasted.
//!
//! - [`descriptor`]: the list as the shell describes it.
//! - [`data_object`]: the `IDataObject` put on the clipboard.
//! - [`stream`]: the `IStream` of one file, fed by the session.
//!
//! The list is checked like any received list (see `transfer::plan`):
//! names Explorer will create come from it.

mod data_object;
mod descriptor;
mod stream;

pub use data_object::{RemoteDataObject, RemoteFiles};

use protocol::transfer::{FileEntry, OfferId};

use crate::transfer::PasteLink;
use crate::transfer::plan::{PlanError, check_list};
use descriptor::Described;

/// The other PC's clipboard files `entries`, if they pass the checks.
pub fn prepare(
    offer: OfferId,
    entries: Vec<FileEntry>,
    link: PasteLink,
) -> Result<RemoteFiles, PlanError> {
    let paths = check_list(&entries)?;
    let described = entries
        .iter()
        .zip(&paths)
        .map(|(entry, path)| Described {
            path: path.to_windows(),
            size: entry.size,
            is_dir: entry.is_dir,
        })
        .collect();
    Ok(RemoteFiles {
        offer,
        entries,
        described,
        link,
    })
}

#[cfg(test)]
mod tests;
