//! The log of the unattended-access service, in its protected folder: who
//! connected or was refused, and what went wrong. The agent writes to the
//! same file (its standard error). Started afresh when it grows too big.

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result};

use crate::trust::Timestamp;

/// Beyond this size the log starts again when the service starts.
const MAX_BYTES: u64 = 1024 * 1024;

pub(super) struct ServiceLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl ServiceLog {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES);
        let file = OpenOptions::new()
            .create(true)
            .append(!too_big)
            .write(true)
            .truncate(too_big)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    /// Appends one line, dated. A log that cannot be written is no reason
    /// to stop the service: the failure is dropped.
    pub(super) fn write(&self, line: impl Display) {
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(file, "{} {line}", Timestamp::now());
    }

    /// The same file opened again for appending, e.g. as the standard
    /// error of the agent.
    pub(super) fn reopen(&self) -> Result<File> {
        OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_appended_and_a_big_log_starts_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.log");
        let log = ServiceLog::open(&path).unwrap();
        log.write("first");
        writeln!(log.reopen().unwrap(), "from the agent").unwrap();
        log.write("second");
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].ends_with(" first"), "{text}");
        assert_eq!(lines[1], "from the agent");
        assert!(lines[2].ends_with(" second"), "{text}");

        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        ServiceLog::open(&path).unwrap().write("fresh");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "started again");
    }
}
