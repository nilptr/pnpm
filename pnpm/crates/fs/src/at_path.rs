//! Paths resolved relative to an open directory through the `*at`
//! syscalls, so a file below a directory opened once resolves only the
//! path below it, not its full path.

use crate::file_mode::narrow_mode;
use rustix::fs::{AtFlags, CWD, Mode, OFlags, Stat};
use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::Path,
};

/// A path resolved relative to `dir`, or to the working directory for
/// [`AtPath::cwd`].
#[derive(Clone, Copy, Debug)]
pub struct AtPath<'a> {
    dir: BorrowedFd<'a>,
    path: &'a Path,
}

impl<'a> AtPath<'a> {
    #[must_use]
    pub fn new(dir: BorrowedFd<'a>, path: &'a Path) -> Self {
        AtPath { dir, path }
    }

    #[must_use]
    pub fn cwd(path: &'a Path) -> Self {
        AtPath { dir: CWD, path }
    }

    #[must_use]
    pub fn dir(self) -> BorrowedFd<'a> {
        self.dir
    }

    #[must_use]
    pub fn path(self) -> &'a Path {
        self.path
    }

    /// The dirent's metadata, without following a final symlink.
    pub fn stat(self) -> io::Result<Stat> {
        Ok(rustix::fs::statat(self.dir, self.path, AtFlags::SYMLINK_NOFOLLOW)?)
    }

    fn open(self, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
        Ok(rustix::fs::openat(self.dir, self.path, flags | OFlags::CLOEXEC, mode)?)
    }

    /// Open the file for reading.
    pub fn open_read(self) -> io::Result<OwnedFd> {
        self.open(OFlags::RDONLY, Mode::empty())
    }

    /// Create a file nothing occupies yet, at `mode` narrowed by the
    /// umask. `O_EXCL` does not follow a symlink squatting at the path.
    pub fn create_new(self, mode: u32) -> io::Result<OwnedFd> {
        self.open(
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(narrow_mode(mode)?),
        )
    }

    /// Open a directory, for other paths to resolve relative to.
    pub fn open_dir(self) -> io::Result<OwnedFd> {
        self.open(OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
    }

    /// Create a directory whose parent exists. A directory already there,
    /// or a symlink to one, counts as created, as in
    /// [`std::fs::create_dir_all`].
    pub fn create_dir(self) -> io::Result<()> {
        match rustix::fs::mkdirat(self.dir, self.path, Mode::from_raw_mode(0o777)) {
            Err(rustix::io::Errno::EXIST) if self.is_dir() => Ok(()),
            result => Ok(result?),
        }
    }

    fn is_dir(self) -> bool {
        rustix::fs::statat(self.dir, self.path, AtFlags::empty())
            .is_ok_and(|stat| rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir())
    }

    /// Unlink the file.
    pub fn remove_file(self) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(self.dir, self.path, AtFlags::empty())?)
    }

    /// Whether this path still names the file `created` refers to. A path
    /// since replaced, removed, or turned into a symlink answers `false`,
    /// so a partial file is removed only while nothing else took its name.
    pub fn still_names(self, created: impl AsFd) -> bool {
        match (rustix::fs::fstat(created), self.stat()) {
            (Ok(created), Ok(current)) => {
                created.st_ino == current.st_ino && created.st_dev == current.st_dev
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
