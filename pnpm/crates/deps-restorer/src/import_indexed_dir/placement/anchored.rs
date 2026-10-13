//! Placing a package's files through open directories.

use crate::link_file::anchored::{AnchoredImport, import_anchored};
use pnpm_config::PackageImportMethod;
use pnpm_fs::{DirHandle, file_mode::is_cas_file_path};
use pnpm_reporter::Reporter;
use std::{
    collections::HashMap,
    ffi::{CString, OsStr},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicU8,
};

/// The open package directory and store that
/// [`populate_dir`](super::populate_dir) names a package's directories
/// and files against.
///
/// Every method is an attempt. One that reports failure has the caller
/// do the same work by path, which is where errors are reported.
pub(super) struct PackageAnchors<'a> {
    package_dir: DirHandle,
    /// The store's `files` directory, which `cas_paths` point into.
    store_files_dir: &'a Path,
    store: DirHandle,
}

impl<'a> PackageAnchors<'a> {
    /// `None` when `cas_paths` holds no store entry, or a directory
    /// cannot be opened.
    pub(super) fn open(dir_path: &Path, cas_paths: &'a HashMap<String, PathBuf>) -> Option<Self> {
        let store_files_dir = cas_paths
            .values()
            .find(|store_path| is_cas_file_path(store_path))?
            .parent()?
            .parent()?;
        Some(PackageAnchors {
            package_dir: DirHandle::open(dir_path).ok()?,
            store_files_dir,
            store: DirHandle::open(store_files_dir).ok()?,
        })
    }

    /// Whether this call created the directory at `relative`.
    pub(super) fn create_dir(&self, relative: &str) -> bool {
        c_string(relative.as_ref())
            .is_some_and(|relative| self.package_dir.create_dir(&relative).is_ok())
    }

    /// The directory at `relative` to place files in, the package
    /// directory itself for `None`.
    pub(super) fn target_dir(&self, relative: Option<&Path>) -> Option<TargetDir<'_>> {
        let dir = match relative.filter(|relative| !relative.as_os_str().is_empty()) {
            Some(relative) => Some(
                self.package_dir
                    .open_child(&c_string(relative.as_ref())?)
                    .ok()?,
            ),
            None => None,
        };
        Some(TargetDir { anchors: self, dir })
    }
}

pub(super) struct TargetDir<'a> {
    anchors: &'a PackageAnchors<'a>,
    /// `None` for the package directory.
    dir: Option<DirHandle>,
}

impl TargetDir<'_> {
    /// Whether `entry` is now in place, as
    /// [`import_anchored`] reports.
    pub(super) fn import<Reporter: self::Reporter>(
        &self,
        logged_methods: &AtomicU8,
        import_method: PackageImportMethod,
        entry: &str,
        store_path: &Path,
    ) -> bool {
        let Some(target_name) = Path::new(entry).file_name().and_then(c_string) else {
            return false;
        };
        let Some(store_entry) = store_path
            .strip_prefix(self.anchors.store_files_dir)
            .ok()
            .and_then(|relative| c_string(relative.as_ref()))
        else {
            return false;
        };
        import_anchored::<Reporter>(
            logged_methods,
            import_method,
            store_path,
            &AnchoredImport {
                store: &self.anchors.store,
                store_entry: &store_entry,
                target_dir: self.dir.as_ref().unwrap_or(&self.anchors.package_dir),
                target_name: &target_name,
            },
        )
    }
}

fn c_string(path: &OsStr) -> Option<CString> {
    CString::new(path.as_bytes()).ok()
}
