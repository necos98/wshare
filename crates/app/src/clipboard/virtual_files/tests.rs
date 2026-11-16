//! The data object as Explorer would query it, called directly (no
//! clipboard involved): its formats, the list, and a file's stream.

use protocol::transfer::{FileEntry, OfferId};
use windows::Win32::Foundation::{DV_E_FORMATETC, DV_E_LINDEX, S_OK};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::ReleaseStgMedium;
use windows::Win32::UI::Shell::{CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW};
use windows::core::HRESULT;

use super::*;
use crate::transfer::plan::PlanError;

fn entries() -> Vec<FileEntry> {
    vec![FileEntry::folder("Foto"), FileEntry::file("Foto/a.jpg", 42)]
}

fn object() -> IDataObject {
    let (link, _requests) = PasteLink::new();
    RemoteDataObject::create(prepare(OfferId(1), entries(), link).unwrap())
}

fn format(name: windows::core::PCWSTR, tymed: i32, lindex: i32) -> FORMATETC {
    FORMATETC {
        // SAFETY: registering a named format has no preconditions.
        cfFormat: unsafe { RegisterClipboardFormatW(name) } as u16,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed: tymed as u32,
    }
}

/// The error of a call that must fail.
fn failure(result: windows::core::Result<STGMEDIUM>) -> HRESULT {
    match result {
        Ok(mut medium) => {
            // SAFETY: a medium the call handed over.
            unsafe { ReleaseStgMedium(&mut medium) };
            panic!("the call succeeded");
        }
        Err(error) => error.code(),
    }
}

#[test]
fn the_list_is_offered_as_file_descriptors() {
    let object = object();
    let descriptors = format(CFSTR_FILEDESCRIPTORW, TYMED_HGLOBAL.0, -1);
    // SAFETY: a valid FORMATETC; the medium is released below.
    unsafe {
        assert_eq!(object.QueryGetData(&descriptors), S_OK);
        let mut medium = object.GetData(&descriptors).unwrap();
        assert_eq!(medium.tymed, TYMED_HGLOBAL.0 as u32);
        let memory = medium.u.hGlobal;
        let size = GlobalSize(memory);
        let pointer = GlobalLock(memory).cast::<u8>();
        let bytes = std::slice::from_raw_parts(pointer, size);
        assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), 2);
        let _ = GlobalUnlock(memory);
        ReleaseStgMedium(&mut medium);
    }
}

#[test]
fn a_file_is_offered_as_a_stream_and_a_folder_is_not() {
    let object = object();
    // SAFETY: valid FORMATETCs; the medium is released below.
    unsafe {
        let file = format(CFSTR_FILECONTENTS, TYMED_ISTREAM.0, 1);
        let mut medium = object.GetData(&file).unwrap();
        assert_eq!(medium.tymed, TYMED_ISTREAM.0 as u32);
        ReleaseStgMedium(&mut medium);

        let folder = format(CFSTR_FILECONTENTS, TYMED_ISTREAM.0, 0);
        assert_eq!(failure(object.GetData(&folder)), DV_E_LINDEX);
        let outside = format(CFSTR_FILECONTENTS, TYMED_ISTREAM.0, 9);
        assert_eq!(failure(object.GetData(&outside)), DV_E_LINDEX);
    }
}

#[test]
fn other_formats_are_not_offered() {
    let object = object();
    let text = FORMATETC {
        cfFormat: 13,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: a valid FORMATETC.
    unsafe {
        assert_eq!(object.QueryGetData(&text), DV_E_FORMATETC);
        assert_eq!(failure(object.GetData(&text)), DV_E_FORMATETC);
        let descriptors_by_stream = format(CFSTR_FILEDESCRIPTORW, TYMED_ISTREAM.0, -1);
        assert_eq!(object.QueryGetData(&descriptors_by_stream), DV_E_FORMATETC);
    }
}

#[test]
fn every_format_is_enumerated() {
    let object = object();
    // SAFETY: plain COM calls on a live object.
    unsafe {
        let formats = object.EnumFormatEtc(1).unwrap();
        let mut found = [FORMATETC::default(); 4];
        let mut count = 0;
        let _ = formats.Next(&mut found, Some(&mut count));
        assert_eq!(count, 3);
    }
}

#[test]
fn names_explorer_would_create_are_checked_first() {
    let (link, _requests) = PasteLink::new();
    for entries in [
        vec![FileEntry::file("../evil.exe", 1)],
        vec![FileEntry::file("C:/Windows/evil.exe", 1)],
        vec![FileEntry::file("NUL", 1)],
        vec![FileEntry::file("a.txt:stream", 1)],
        vec![FileEntry::file("cartella/orfano.txt", 1)],
        Vec::new(),
    ] {
        assert!(
            prepare(OfferId(1), entries.clone(), link.clone()).is_err(),
            "{entries:?}"
        );
    }
    assert!(matches!(
        prepare(OfferId(1), Vec::new(), link.clone()),
        Err(PlanError::Empty)
    ));
    let files = prepare(OfferId(2), entries(), link).unwrap();
    assert_eq!(files.described[1].path, "Foto\\a.jpg");
}
