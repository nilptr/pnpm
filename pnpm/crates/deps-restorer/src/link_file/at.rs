//! [`FileImport`] relative to open directories, so importing a file
//! resolves only the path below its directory, not its full path.

use super::{
    FileImport,
    target_mode::{desired_mode, mode_is_linkable},
};
use pnpm_fs::{
    AtPath,
    file_mode::{is_cas_file_path, widen_mode},
};
use rustix::fs::AtFlags;
use std::{
    fs, io,
    os::fd::{AsFd, OwnedFd},
    path::{Path, PathBuf},
};

/// An open store `files` directory. A store entry's source then resolves
/// as `<shard>/<digest>` below it.
pub(crate) struct StoreFilesDir {
    path: PathBuf,
    fd: OwnedFd,
}

impl StoreFilesDir {
    /// Open the `files` directory of the first store entry among
    /// `sources`. `None` when none is a store entry or the directory does
    /// not open, which leaves every source to resolve by its own path.
    pub(crate) fn open<'a>(mut sources: impl Iterator<Item = &'a Path>) -> Option<Self> {
        let path = sources
            .find(|source| is_cas_file_path(source))?
            .parent()?
            .parent()?;
        let fd = AtPath::cwd(path).open_dir().ok()?;
        Some(StoreFilesDir { path: path.to_path_buf(), fd })
    }

    /// Where `source` resolves: below this directory when it is inside it.
    pub(crate) fn locate<'a>(dir: Option<&'a Self>, source: &'a Path) -> AtPath<'a> {
        dir.and_then(|dir| {
            source
                .strip_prefix(&dir.path)
                .ok()
                .map(|relative| AtPath::new(dir.fd.as_fd(), relative))
        })
        .unwrap_or_else(|| AtPath::cwd(source))
    }
}

/// [`FileImport`] between two [`AtPath`]s.
pub(crate) struct AtImport<'a> {
    /// The source by its full path, which decides its store-entry mode.
    source_path: &'a Path,
    source: AtPath<'a>,
    target: AtPath<'a>,
}

impl<'a> AtImport<'a> {
    pub(crate) fn new(source_path: &'a Path, source: AtPath<'a>, target: AtPath<'a>) -> Self {
        AtImport { source_path, source, target }
    }

    /// The mode a copy of the source carries: the store-entry mode, or the
    /// open source's own permission bits.
    fn copy_mode(&self, source: impl AsFd) -> io::Result<u32> {
        match desired_mode(self.source_path) {
            Some(mode) => Ok(mode),
            None => Ok(widen_mode(rustix::fs::fstat(source)?.st_mode) & 0o7777),
        }
    }

    /// Remove a target this import created and failed to finish, unless a
    /// concurrent writer has since replaced it with a complete file.
    fn remove_created(&self, created: impl AsFd) {
        if self.target.still_names(created) {
            let _ = self.target.remove_file();
        }
    }
}

impl FileImport for AtImport<'_> {
    fn hard_link(&self) -> io::Result<()> {
        Ok(rustix::fs::linkat(
            self.source.dir(),
            self.source.path(),
            self.target.dir(),
            self.target.path(),
            AtFlags::empty(),
        )?)
    }

    #[cfg(target_os = "macos")]
    fn reflink(&self) -> io::Result<()> {
        use rustix::{io::Errno, path::Arg};
        use std::os::fd::AsRawFd;
        // rustix wraps only `fclonefileat`, which costs an extra open and
        // close of the source per file.
        let clone = |source: &std::ffi::CStr, target: &std::ffi::CStr| {
            // SAFETY: both strings are NUL-terminated and outlive the call,
            // and both descriptors are borrowed for it.
            let result = unsafe {
                libc::clonefileat(
                    self.source.dir().as_raw_fd(),
                    source.as_ptr(),
                    self.target.dir().as_raw_fd(),
                    target.as_ptr(),
                    rustix::fs::CloneFlags::NOOWNERCOPY.bits(),
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(Errno::from_io_error(&io::Error::last_os_error()).unwrap_or(Errno::IO))
            }
        };
        Ok(self.source
            .path()
            .into_with_c_str(|source| {
                self.target.path().into_with_c_str(|target| clone(source, target))
            })?)
    }

    /// `FICLONE` onto a target created at the mode it finishes with, so
    /// [`Self::align_target_mode`] has nothing left to do.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn reflink(&self) -> io::Result<()> {
        use pnpm_fs::file_mode::narrow_mode;
        use rustix::fs::Mode;
        let source = self.source.open_read()?;
        let mode = self.copy_mode(&source)?;
        let target = self.target.create_new(mode)?;
        let cloned = rustix::fs::ioctl_ficlone(&target, &source)
            .map_err(io::Error::from)
            .and_then(|()| match desired_mode(self.source_path) {
                Some(_) => Ok(()),
                // The umask may have narrowed a mode not derived from it.
                None => Ok(rustix::fs::fchmod(&target, Mode::from_raw_mode(narrow_mode(mode)?))?),
            });
        if cloned.is_err() {
            self.remove_created(&target);
        }
        cloned
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    fn reflink(&self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn copy(&self) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut source = fs::File::from(self.source.open_read()?);
        let mode = self.copy_mode(&source)?;
        let mut target = fs::File::from(self.target.create_new(mode)?);
        let permissions = fs::Permissions::from_mode(mode);
        pnpm_fs::copy_into_created(&mut source, &mut target, &permissions, |_| Ok(()))
            .inspect_err(|_| self.remove_created(&target))
    }

    /// `clonefile` gives the clone the source's mode. Elsewhere the clone
    /// was created at the desired mode.
    fn align_target_mode(&self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(mode) = desired_mode(self.source_path) {
            use pnpm_fs::file_mode::narrow_mode;
            let stat = self.target.stat()?;
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                != rustix::fs::FileType::RegularFile
            {
                // The permission change must not reach what a squatting
                // symlink names.
                return Err(rustix::io::Errno::LOOP.into());
            }
            if widen_mode(stat.st_mode) & 0o7777 != mode {
                rustix::fs::chmodat(
                    self.target.dir(),
                    self.target.path(),
                    rustix::fs::Mode::from_raw_mode(narrow_mode(mode)?),
                    AtFlags::SYMLINK_NOFOLLOW,
                )?;
            }
        }
        Ok(())
    }

    fn linked_mode_is_linkable(&self) -> io::Result<bool> {
        mode_is_linkable(self.source_path, || {
            self.target.stat().map(|stat| widen_mode(stat.st_mode))
        })
    }

    fn remove_target(&self) -> io::Result<()> {
        self.target.remove_file()
    }
}

#[cfg(test)]
mod tests;
