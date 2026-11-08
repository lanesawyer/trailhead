//! The control plane's log file: what `run` prints to the terminal, kept in `<data dir>/factory/factory.log` so a
//! problem can still be checked after the scrollback is gone. It rotates by size, keeping a few older files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The log file's name, in the factory's state directory.
pub const FILE_NAME: &str = "factory.log";
/// Rotate when the file reaches this size.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Older files kept: `factory.log.1` (newest) to `factory.log.N`.
const KEEP: u32 = 5;

/// Where the log file is, given the data directory.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("factory").join(FILE_NAME)
}

/// An append-only file that moves itself to `.1` (shifting the older ones up, dropping the oldest) when it passes
/// its size limit.
pub struct Rotating {
    path: PathBuf,
    file: File,
    size: u64,
    max: u64,
    keep: u32,
}

impl Rotating {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::with_limits(path, MAX_BYTES, KEEP)
    }

    fn with_limits(path: &Path, max: u64, keep: u32) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        Ok(Self { path: path.to_path_buf(), file, size, max, keep })
    }

    fn numbered(&self, n: u32) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        for n in (1..self.keep).rev() {
            let from = self.numbered(n);
            if from.exists() {
                fs::rename(&from, self.numbered(n + 1))?;
            }
        }
        fs::rename(&self.path, self.numbered(1))?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for Rotating {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size >= self.max {
            // A failed rotation shouldn't lose the line or stop logging; keep appending to the current file.
            let _ = self.rotate();
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_past_the_limit_and_keeps_a_few() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory").join("factory.log");
        let mut log = Rotating::with_limits(&path, 10, 2).unwrap();
        for line in ["aaaaaaaaaa\n", "bbbbbbbbbb\n", "cccccccccc\n", "dddddddddd\n"] {
            log.write_all(line.as_bytes()).unwrap();
        }
        let read = |p: &Path| fs::read_to_string(p).unwrap();
        assert_eq!(read(&path), "dddddddddd\n");
        assert_eq!(read(&log.numbered(1)), "cccccccccc\n");
        assert_eq!(read(&log.numbered(2)), "bbbbbbbbbb\n");
        assert!(!log.numbered(3).exists());
    }

    #[test]
    fn appends_to_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory.log");
        Rotating::open(&path).unwrap().write_all(b"one\n").unwrap();
        Rotating::open(&path).unwrap().write_all(b"two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    }
}
