//! `*at` syscalls anchored at an open directory.

use crate::file_mode::widen_mode;
use std::{
    ffi::CStr,
    fs, io,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
};

/// An open directory. Its methods name their entries relative to it, so
/// the kernel resolves each from the directory instead of walking an
/// absolute path once per call.
#[derive(Debug)]
pub struct DirHandle(OwnedFd);

impl DirHandle {
    pub fn open(path: &Path) -> io::Result<Self> {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(path)
            .map(|dir| DirHandle(dir.into()))
    }

    /// Open the directory at `relative`, as [`Self::open`].
    pub fn open_child(&self, relative: &CStr) -> io::Result<Self> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
        // SAFETY: `self` keeps the descriptor open for the call, and
        // `relative` is NUL-terminated.
        let fd = unsafe { libc::openat(self.fd(), relative.as_ptr(), flags) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` just returned `fd`, so nothing else owns it.
        Ok(DirHandle(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Create the directory at `relative`, as [`fs::create_dir`].
    pub fn create_dir(&self, relative: &CStr) -> io::Result<()> {
        // SAFETY: `self` keeps the descriptor open for the call, and
        // `relative` is NUL-terminated.
        check(unsafe { libc::mkdirat(self.fd(), relative.as_ptr(), 0o777) })
    }

    /// Hard-link `source`, relative to `source_dir`, at `name`, as
    /// [`fs::hard_link`].
    pub fn hard_link_from(
        &self,
        name: &CStr,
        source_dir: &DirHandle,
        source: &CStr,
    ) -> io::Result<()> {
        // SAFETY: both handles keep their descriptors open for the call,
        // and both names are NUL-terminated.
        check(unsafe {
            libc::linkat(source_dir.fd(), source.as_ptr(), self.fd(), name.as_ptr(), 0)
        })
    }

    /// Clone `source`, relative to `source_dir`, at `name`, as
    /// [`crate::FsReflink::reflink`]. `Unsupported` where the platform has
    /// no anchored clone.
    pub fn reflink_from(
        &self,
        name: &CStr,
        source_dir: &DirHandle,
        source: &CStr,
    ) -> io::Result<()> {
        clone_at(source_dir.fd(), source, self.fd(), name)
    }

    /// The permission bits of the regular file at `name`, or `None` when
    /// `name` is anything else.
    pub fn regular_file_mode(&self, name: &CStr) -> io::Result<Option<u32>> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `self` keeps the descriptor open for the call, `name` is
        // NUL-terminated, and `stat` is writable for one `libc::stat`.
        check(unsafe {
            libc::fstatat(self.fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW)
        })?;
        // SAFETY: `fstatat` succeeded, so it filled `stat`.
        let mode = widen_mode(unsafe { stat.assume_init() }.st_mode);
        let is_regular_file = mode & widen_mode(libc::S_IFMT) == widen_mode(libc::S_IFREG);
        Ok(is_regular_file.then_some(mode & 0o7777))
    }

    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(target_os = "macos")]
fn clone_at(source_dir: RawFd, source: &CStr, target_dir: RawFd, name: &CStr) -> io::Result<()> {
    // `libc` does not export the flag. `reflink_copy` passes the same one
    // to `clonefile`.
    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    // SAFETY: the callers keep both descriptors open for the call, and
    // both names are NUL-terminated.
    check(unsafe {
        libc::clonefileat(source_dir, source.as_ptr(), target_dir, name.as_ptr(), CLONE_NOOWNERCOPY)
    })
}

/// `FICLONE` shares the extents of two open files, so the target is
/// created first, exclusively and with the source's mode, and removed
/// again when the filesystem refuses the clone.
#[cfg(target_os = "linux")]
fn clone_at(source_dir: RawFd, source: &CStr, target_dir: RawFd, name: &CStr) -> io::Result<()> {
    let source = open_at(source_dir, source, libc::O_RDONLY, 0)?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `source` is open, and `stat` is writable for one `libc::stat`.
    check(unsafe { libc::fstat(source.as_raw_fd(), stat.as_mut_ptr()) })?;
    // SAFETY: `fstat` succeeded, so it filled `stat`.
    let mode = unsafe { stat.assume_init() }.st_mode & 0o7777;
    let target = open_at(target_dir, name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode)?;
    // SAFETY: both descriptors are open, and `FICLONE` takes the source
    // descriptor as its argument.
    let cloned = check(unsafe {
        libc::ioctl(target.as_raw_fd(), libc::FICLONE as libc::Ioctl, source.as_raw_fd())
    });
    if cloned.is_err() {
        // SAFETY: the caller keeps `target_dir` open, and `name` is
        // NUL-terminated.
        unsafe { libc::unlinkat(target_dir, name.as_ptr(), 0) };
    }
    cloned
}

#[cfg(target_os = "linux")]
fn open_at(dir: RawFd, name: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<OwnedFd> {
    // SAFETY: the caller keeps `dir` open for the call, and `name` is
    // NUL-terminated.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` just returned `fd`, so nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_at(_: RawFd, _: &CStr, _: RawFd, _: &CStr) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(test)]
mod tests;
