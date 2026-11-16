//! The other PC's copied files on this PC's clipboard: an `IDataObject`
//! offering them as virtual files (`CFSTR_FILEDESCRIPTORW` for the list,
//! `CFSTR_FILECONTENTS` for each file's bytes). Everything is rendered when
//! asked for: until someone pastes, only the list exists here.

use std::mem::ManuallyDrop;

use protocol::transfer::{FileEntry, OfferId};
use windows::Win32::Foundation::{
    DV_E_FORMATETC, DV_E_LINDEX, E_NOTIMPL, OLE_E_ADVISENOTSUPPORTED, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, IStream, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Ole::DROPEFFECT_COPY;
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_PREFERREDDROPEFFECT, SHCreateStdEnumFmtEtc,
};
use windows::core::{BOOL, HRESULT, Ref, implement};

use super::descriptor::{Described, group_descriptor};
use super::stream::RemoteStream;
use crate::clipboard::global::alloc_bytes;
use crate::transfer::PasteLink;

/// The other PC's clipboard files, checked, with the way to fetch them.
pub struct RemoteFiles {
    pub offer: OfferId,
    /// As listed by the other PC (checked: see `transfer::plan`).
    pub entries: Vec<FileEntry>,
    /// The same entries as the shell describes them.
    pub described: Vec<Described>,
    pub link: PasteLink,
}

/// The clipboard formats offered, registered by name.
#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
    drop_effect: u16,
}

impl Formats {
    fn register() -> Self {
        // SAFETY: registering a named format has no preconditions; the
        // value is a 16-bit format id (0 on failure, never matched).
        let id = |name| unsafe { RegisterClipboardFormatW(name) } as u16;
        Self {
            descriptor: id(CFSTR_FILEDESCRIPTORW),
            contents: id(CFSTR_FILECONTENTS),
            drop_effect: id(CFSTR_PREFERREDDROPEFFECT),
        }
    }

    /// Every format offered, as the enumerator lists them.
    fn all(self) -> [FORMATETC; 3] {
        let format = |cf, tymed: i32| FORMATETC {
            cfFormat: cf,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: tymed as u32,
        };
        [
            format(self.descriptor, TYMED_HGLOBAL.0),
            format(self.contents, TYMED_ISTREAM.0),
            format(self.drop_effect, TYMED_HGLOBAL.0),
        ]
    }
}

#[implement(IDataObject)]
pub struct RemoteDataObject {
    files: RemoteFiles,
    formats: Formats,
}

impl RemoteDataObject {
    pub fn create(files: RemoteFiles) -> IDataObject {
        Self {
            files,
            formats: Formats::register(),
        }
        .into()
    }

    /// The medium for `format`, or why it cannot be given.
    fn render(&self, format: &FORMATETC) -> Result<STGMEDIUM, HRESULT> {
        let wants = |tymed: i32| format.tymed & tymed as u32 != 0;
        if format.cfFormat == self.formats.descriptor && wants(TYMED_HGLOBAL.0) {
            return global(&group_descriptor(&self.files.described));
        }
        if format.cfFormat == self.formats.drop_effect && wants(TYMED_HGLOBAL.0) {
            return global(&DROPEFFECT_COPY.0.to_ne_bytes());
        }
        if format.cfFormat == self.formats.contents && wants(TYMED_ISTREAM.0) {
            return self.contents(format.lindex);
        }
        Err(DV_E_FORMATETC)
    }

    /// The bytes of file `lindex` of the list, as a stream.
    fn contents(&self, lindex: i32) -> Result<STGMEDIUM, HRESULT> {
        let index = u32::try_from(lindex).map_err(|_| DV_E_LINDEX)?;
        let entry = usize::try_from(index)
            .ok()
            .and_then(|i| self.files.entries.get(i))
            .filter(|entry| !entry.is_dir)
            .ok_or(DV_E_LINDEX)?;
        let link = self.files.link.clone();
        let stream: IStream =
            RemoteStream::new(link, self.files.offer, index, entry.clone()).into();
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn offers(&self, format: &FORMATETC) -> bool {
        self.formats
            .all()
            .iter()
            .any(|f| f.cfFormat == format.cfFormat && f.tymed & format.tymed != 0)
    }
}

/// A medium holding a copy of `bytes` in global memory.
fn global(bytes: &[u8]) -> Result<STGMEDIUM, HRESULT> {
    let memory = alloc_bytes(bytes).map_err(|_| windows::Win32::Foundation::E_OUTOFMEMORY)?;
    Ok(STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 { hGlobal: memory },
        pUnkForRelease: ManuallyDrop::new(None),
    })
}

impl IDataObject_Impl for RemoteDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: COM's contract: a valid FORMATETC for the call.
        let format = unsafe { pformatetcin.as_ref() }.ok_or(DV_E_FORMATETC)?;
        self.render(format).map_err(Into::into)
    }

    fn GetDataHere(
        &self,
        _format: *const FORMATETC,
        _medium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: as in `GetData`.
        match unsafe { pformatetc.as_ref() } {
            Some(format) if self.offers(format) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, _out: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    /// Explorer tells what it did with the files ("Performed DropEffect");
    /// nothing to keep.
    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        // SAFETY: the formats are plain values copied by the enumerator.
        unsafe { SHCreateStdEnumFmtEtc(&self.formats.all()) }
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _advf: u32,
        _sink: Ref<IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}
