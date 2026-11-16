//! One pasted file as Explorer reads it: an `IStream` whose bytes come
//! from the other PC. Nothing is asked for until Explorer reads; the first
//! read starts the transfer (see `transfer::PasteLink`).
//!
//! Explorer's calls arrive on the clipboard thread (the apartment the data
//! object lives in), never on a window's thread: a read that waits for the
//! network holds only that thread. If the transfer fails or is cancelled,
//! the read fails with `E_ABORT` and Explorer reports the error.

use std::sync::Mutex;

use bytes::Bytes;
use protocol::transfer::{FileEntry, OfferId};
use tokio::sync::mpsc;
use windows::Win32::Foundation::{
    E_ABORT, E_NOTIMPL, E_POINTER, S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION,
};
use windows::Win32::System::Com::{
    ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATSTG, STGC, STGTY_STREAM,
    STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_SET,
};
use windows::core::{HRESULT, Ref, implement};

use crate::transfer::{Chunk, PasteLink};

/// Where a pasted file's bytes are at.
enum Fetch {
    /// Not read yet: nothing was asked of the other PC.
    NotAsked(PasteLink),
    Reading {
        chunks: mpsc::Receiver<Chunk>,
        /// Received and not yet handed to Explorer.
        pending: Bytes,
        ended: bool,
    },
    /// The transfer failed or was cancelled: every read fails.
    Failed,
}

#[implement(IStream)]
pub struct RemoteStream {
    offer: OfferId,
    index: u32,
    entry: FileEntry,
    state: Mutex<(Fetch, u64)>,
}

impl RemoteStream {
    pub fn new(link: PasteLink, offer: OfferId, index: u32, entry: FileEntry) -> Self {
        Self {
            offer,
            index,
            entry,
            state: Mutex::new((Fetch::NotAsked(link), 0)),
        }
    }

    /// Copies into `out` what is there, waiting only while nothing was
    /// copied yet. Returns how much was copied and whether the file ended.
    fn fill(&self, out: &mut [u8]) -> Result<(usize, bool), HRESULT> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (fetch, position) = &mut *state;
        if let Fetch::NotAsked(link) = fetch {
            *fetch = match link.fetch(self.offer, self.index, self.entry.clone()) {
                Some(chunks) => Fetch::Reading {
                    chunks,
                    pending: Bytes::new(),
                    ended: false,
                },
                None => Fetch::Failed,
            };
        }
        let Fetch::Reading {
            chunks,
            pending,
            ended,
        } = fetch
        else {
            return Err(E_ABORT);
        };
        let mut copied = 0;
        while copied < out.len() {
            if pending.is_empty() {
                if *ended {
                    break;
                }
                let next = if copied == 0 {
                    chunks.blocking_recv()
                } else {
                    match chunks.try_recv() {
                        Ok(chunk) => Some(chunk),
                        // Explorer gets what is there; the rest next time.
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => None,
                    }
                };
                match next {
                    Some(Chunk::Data(data)) => *pending = data,
                    Some(Chunk::End) => *ended = true,
                    None => {
                        *fetch = Fetch::Failed;
                        return Err(E_ABORT);
                    }
                }
                continue;
            }
            let count = pending.len().min(out.len() - copied);
            out[copied..copied + count].copy_from_slice(&pending.split_to(count));
            copied += count;
        }
        *position += copied as u64;
        let at_end = *ended && pending.is_empty();
        Ok((copied, at_end))
    }

    fn position(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1
    }
}

impl ISequentialStream_Impl for RemoteStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return E_POINTER;
        }
        // SAFETY: COM's contract: `pv` points to `cb` writable bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let (copied, at_end) = match self.fill(out) {
            Ok(filled) => filled,
            Err(error) => return error,
        };
        if !pcbread.is_null() {
            // SAFETY: a non-null `pcbread` points to a u32 the caller owns.
            unsafe { *pcbread = copied as u32 };
        }
        if copied < out.len() && at_end {
            S_FALSE
        } else {
            S_OK
        }
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _written: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for RemoteStream_Impl {
    /// Only asking where the stream is (or going back to its start before
    /// anything was read) is possible: the bytes come in order.
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let position = self.position();
        let stays = dlibmove == 0
            && (dworigin == STREAM_SEEK_CUR || (dworigin == STREAM_SEEK_SET && position == 0));
        if !stays {
            return Err(STG_E_INVALIDFUNCTION.into());
        }
        if !plibnewposition.is_null() {
            // SAFETY: a non-null pointer the caller owns.
            unsafe { *plibnewposition = position };
        }
        Ok(())
    }

    fn SetSize(&self, _size: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(
        &self,
        _target: Ref<IStream>,
        _cb: u64,
        _read: *mut u64,
        _written: *mut u64,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _flags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn LockRegion(&self, _offset: u64, _cb: u64, _kind: &LOCKTYPE) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn UnlockRegion(&self, _offset: u64, _cb: u64, _kind: u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    /// The size, so Explorer can show progress; no name (Explorer has it
    /// from the file descriptor).
    fn Stat(&self, pstatstg: *mut STATSTG, _flag: &STATFLAG) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(E_POINTER.into());
        }
        let stat = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.entry.size,
            ..Default::default()
        };
        // SAFETY: a non-null pointer to a STATSTG the caller owns.
        unsafe { *pstatstg = stat };
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::STATFLAG_NONAME;

    use super::*;

    /// A stream whose fetch is answered by the test, and the request's
    /// channel.
    fn stream(
        size: u64,
    ) -> (
        IStream,
        mpsc::UnboundedReceiver<crate::transfer::PasteRequest>,
    ) {
        let (link, requests) = PasteLink::new();
        let stream: IStream =
            RemoteStream::new(link, OfferId(1), 0, FileEntry::file("a.bin", size)).into();
        (stream, requests)
    }

    fn read(stream: &IStream, cb: usize) -> (HRESULT, Vec<u8>) {
        let mut buffer = vec![0u8; cb];
        let mut read = 0u32;
        // SAFETY: the buffer is `cb` bytes long and outlives the call.
        let result = unsafe { stream.Read(buffer.as_mut_ptr().cast(), cb as u32, Some(&mut read)) };
        buffer.truncate(read as usize);
        (result, buffer)
    }

    #[test]
    fn bytes_are_asked_for_on_the_first_read_and_end_with_s_false() {
        let (stream, mut requests) = stream(5);
        assert!(requests.try_recv().is_err(), "nothing asked before a read");
        let feeder = std::thread::spawn(move || {
            let request = requests.blocking_recv().unwrap();
            assert_eq!((request.offer, request.index), (OfferId(1), 0));
            request
                .chunks
                .blocking_send(Chunk::Data(Bytes::from_static(b"ciao!")))
                .unwrap();
            request.chunks.blocking_send(Chunk::End).unwrap();
        });
        let (result, bytes) = read(&stream, 3);
        assert_eq!((result, bytes.as_slice()), (S_OK, &b"cia"[..]));
        feeder.join().unwrap();
        let (result, bytes) = read(&stream, 10);
        assert_eq!((result, bytes.as_slice()), (S_FALSE, &b"o!"[..]));
        let (result, bytes) = read(&stream, 10);
        assert_eq!((result, bytes.len()), (S_FALSE, 0));
    }

    #[test]
    fn a_failed_transfer_fails_every_read() {
        let (stream, mut requests) = stream(5);
        let feeder = std::thread::spawn(move || {
            let request = requests.blocking_recv().unwrap();
            request
                .chunks
                .blocking_send(Chunk::Data(Bytes::from_static(b"ci")))
                .unwrap();
            // Dropped without the end: cancelled.
        });
        // The first read asks for the file, which the feeder answers.
        assert_eq!(read(&stream, 2).0, S_OK);
        feeder.join().unwrap();
        assert_eq!(read(&stream, 2).0, E_ABORT);
        assert_eq!(read(&stream, 2).0, E_ABORT);
    }

    #[test]
    fn a_session_that_is_over_fails_the_read() {
        let (stream, requests) = stream(5);
        drop(requests);
        assert_eq!(read(&stream, 2).0, E_ABORT);
    }

    #[test]
    fn stat_gives_the_size_and_seek_only_reports_the_position() {
        let (stream, _requests) = stream(1234);
        let mut stat = STATSTG::default();
        // SAFETY: `stat` is a valid STATSTG.
        unsafe { stream.Stat(&mut stat, STATFLAG_NONAME) }.unwrap();
        assert_eq!(stat.cbSize, 1234);
        let mut position = 99;
        // SAFETY: as above.
        unsafe { stream.Seek(0, STREAM_SEEK_CUR, Some(&mut position)) }.unwrap();
        assert_eq!(position, 0);
        // SAFETY: as above.
        assert!(unsafe { stream.Seek(10, STREAM_SEEK_SET, None) }.is_err());
    }
}
