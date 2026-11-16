//! The shell's description of files that exist only as streams
//! (`CFSTR_FILEDESCRIPTORW`): a `FILEGROUPDESCRIPTORW`, i.e. a count and
//! one `FILEDESCRIPTORW` per entry, folders before what they hold.
//! Explorer creates the folders itself and reads each file's contents from
//! `CFSTR_FILECONTENTS` with the entry's position as `lindex`.

use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Shell::{
    FD_ATTRIBUTES, FD_FILESIZE, FD_PROGRESSUI, FD_UNICODE, FILEDESCRIPTORW,
};

/// One entry as the shell sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    /// Relative, with `\` between folders, at most
    /// [`protocol::transfer::MAX_PATH_UNITS`] UTF-16 units (checked before).
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

const COUNT_BYTES: usize = size_of::<u32>();
/// Units of `FILEDESCRIPTORW::cFileName`.
const NAME_UNITS: usize = 260;

/// The `FILEGROUPDESCRIPTORW` of `entries`, as the bytes of its global
/// memory.
pub fn group_descriptor(entries: &[Described]) -> Vec<u8> {
    let count = u32::try_from(entries.len()).expect("lists are bounded far below u32");
    let mut bytes = Vec::with_capacity(COUNT_BYTES + entries.len() * size_of::<FILEDESCRIPTORW>());
    bytes.extend_from_slice(&count.to_ne_bytes());
    for entry in entries {
        let descriptor = descriptor(entry);
        // SAFETY: FILEDESCRIPTORW is plain old data (integers and arrays of
        // them); its bytes are what the shell reads.
        let raw = unsafe {
            std::slice::from_raw_parts(
                (&raw const descriptor).cast::<u8>(),
                size_of::<FILEDESCRIPTORW>(),
            )
        };
        bytes.extend_from_slice(raw);
    }
    bytes
}

fn descriptor(entry: &Described) -> FILEDESCRIPTORW {
    let mut descriptor = FILEDESCRIPTORW {
        dwFlags: (FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_PROGRESSUI.0 | FD_UNICODE.0) as u32,
        dwFileAttributes: if entry.is_dir {
            FILE_ATTRIBUTE_DIRECTORY.0
        } else {
            FILE_ATTRIBUTE_NORMAL.0
        },
        // Split on purpose: the high and the low 32 bits.
        nFileSizeHigh: (entry.size >> 32) as u32,
        nFileSizeLow: entry.size as u32,
        ..Default::default()
    };
    // Filled apart: the struct is packed, its fields cannot be borrowed.
    // The last unit stays 0: the name is always terminated.
    let mut name = [0u16; NAME_UNITS];
    for (slot, unit) in name
        .iter_mut()
        .zip(entry.path.encode_utf16().take(NAME_UNITS - 1))
    {
        *slot = unit;
    }
    descriptor.cFileName = name;
    descriptor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_descriptor(bytes: &[u8], index: usize) -> FILEDESCRIPTORW {
        let start = COUNT_BYTES + index * size_of::<FILEDESCRIPTORW>();
        let slice = &bytes[start..start + size_of::<FILEDESCRIPTORW>()];
        // SAFETY: `slice` holds exactly the bytes of one descriptor written
        // by `group_descriptor`; read unaligned.
        unsafe { std::ptr::read_unaligned(slice.as_ptr().cast::<FILEDESCRIPTORW>()) }
    }

    fn name(descriptor: &FILEDESCRIPTORW) -> String {
        let units = descriptor.cFileName;
        let end = units.iter().position(|&u| u == 0).unwrap();
        String::from_utf16(&units[..end]).unwrap()
    }

    #[test]
    fn the_group_holds_its_count_then_each_descriptor() {
        let entries = [
            Described {
                path: "Foto".into(),
                size: 0,
                is_dir: true,
            },
            Described {
                path: "Foto\\mare è.jpg".into(),
                size: (5 << 32) + 7,
                is_dir: false,
            },
        ];
        let bytes = group_descriptor(&entries);
        assert_eq!(bytes.len(), 4 + 2 * size_of::<FILEDESCRIPTORW>());
        assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), 2);

        let folder = read_descriptor(&bytes, 0);
        assert_eq!(name(&folder), "Foto");
        assert_eq!({ folder.dwFileAttributes }, FILE_ATTRIBUTE_DIRECTORY.0);

        let file = read_descriptor(&bytes, 1);
        assert_eq!(name(&file), "Foto\\mare è.jpg");
        assert_eq!((file.nFileSizeHigh, file.nFileSizeLow), (5, 7));
        assert_eq!({ file.dwFileAttributes }, FILE_ATTRIBUTE_NORMAL.0);
        let flags = file.dwFlags;
        assert_ne!(flags & FD_FILESIZE.0 as u32, 0);
        assert_ne!(flags & FD_PROGRESSUI.0 as u32, 0);
    }

    #[test]
    fn a_name_is_always_terminated() {
        let long = Described {
            path: "x".repeat(400),
            size: 1,
            is_dir: false,
        };
        let descriptor = read_descriptor(&group_descriptor(&[long]), 0);
        let units = descriptor.cFileName;
        assert_eq!(units[NAME_UNITS - 1], 0);
    }

    #[test]
    fn an_empty_group_is_just_a_count() {
        assert_eq!(group_descriptor(&[]), 0u32.to_ne_bytes());
    }
}
