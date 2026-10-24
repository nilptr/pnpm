use super::{
    CafsFileInfo, HashMap, StoreDir, VerifiedFilesCache, is_modified_since, verify_file,
    verify_modified_file,
};
use pnpm_fs::file_mode::is_executable;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// The on-disk check of one store-index row's files, split off its
/// map building by [`defer_pkg_files_integrity`](super::defer_pkg_files_integrity).
#[derive(Debug)]
pub struct PendingFilesCheck {
    pub(super) files: HashMap<String, CafsFileInfo>,
    pub(super) algo: String,
}

impl PendingFilesCheck {
    /// Whether every file the row records is still on disk with its
    /// recorded content. A file already in `verified_files_cache` is
    /// trusted without a stat; one that passes here is added to it.
    ///
    /// Every file is checked, so a run reports all of a row's problems.
    /// A row with many files checks them in parallel: each check is a
    /// stat, and on a network filesystem the stats of one package would
    /// otherwise queue behind each other for seconds.
    #[must_use]
    pub fn verify(self, store_dir: &StoreDir, verified_files_cache: &VerifiedFilesCache) -> bool {
        let check = |(filename, info): (&String, &CafsFileInfo)| {
            // A malformed digest already failed the row's maps.
            let Some(path) = store_dir.cas_file_path_by_mode(&info.digest, info.mode) else {
                return false;
            };
            if verified_files_cache.contains(&path) {
                return true;
            }
            let verified = verify_file(&path, filename, info, &self.algo);
            if verified {
                verified_files_cache.insert(path);
            }
            verified
        };
        let failed = if self.files.len() < PARALLEL_FILES_CHECK_MIN {
            self.files
                .iter()
                .filter(|file| !check(*file))
                .count()
        } else {
            self.files
                .par_iter()
                .filter(|file| !check(*file))
                .count()
        };
        failed == 0
    }
}

/// From how many files a row's check runs them in parallel.
const PARALLEL_FILES_CHECK_MIN: usize = 32;

/// A row's [`PendingFilesCheck`] for the importer that places the row's
/// files: each file's check runs on the metadata the import reads
/// anyway, so the files are not stat-ed a second time ahead of it.
///
/// Files are looked up by their store path, since an import may rename
/// the entries it places but never the store entries it places them
/// from.
#[derive(Debug)]
pub struct StoreFilesCheck<'a> {
    /// By the file name [`StoreDir::cas_file_path_by_mode`] gives the
    /// file: the digest past its shard, and whether it is executable.
    files: HashMap<(&'a str, bool), (&'a str, &'a CafsFileInfo)>,
    algo: &'a str,
    verified_files_cache: &'a VerifiedFilesCache,
}

impl<'a> StoreFilesCheck<'a> {
    #[must_use]
    pub fn new(files: &'a PendingFilesCheck, verified_files_cache: &'a VerifiedFilesCache) -> Self {
        StoreFilesCheck {
            files: files.files
                .iter()
                .filter_map(|(filename, info)| {
                    let (_, digest_tail) = info.digest.split_at_checked(2)?;
                    Some(((digest_tail, is_executable(info.mode)), (filename.as_str(), info)))
                })
                .collect(),
            algo: &files.algo,
            verified_files_cache,
        }
    }

    /// The check of the store file at `store_path`, `None` for a path
    /// the row does not record.
    #[must_use]
    pub fn file(&self, store_path: &Path) -> Option<StoreFileCheck<'a>> {
        let name = store_path.file_name()?.to_str()?;
        let shard = store_path
            .parent()?
            .file_name()?
            .to_str()?;
        let digest_tail = name.strip_suffix("-exec");
        let (filename, info) =
            self.files.get(&(digest_tail.unwrap_or(name), digest_tail.is_some()))?;
        if !info.digest.starts_with(shard) {
            return None;
        }
        Some(StoreFileCheck {
            filename,
            info,
            algo: self.algo,
            verified_files_cache: self.verified_files_cache,
        })
    }

    /// A file of `cas_paths` that fails its check, read by its path here,
    /// for an import that cannot check its files as it places them.
    /// `None` when every file passes; a file the row does not record
    /// passes. Every file is checked, as [`PendingFilesCheck::verify`]
    /// does, and for the same reason in parallel from the same size.
    #[must_use]
    pub fn stale_file<'p>(&self, cas_paths: &'p HashMap<String, PathBuf>) -> Option<&'p Path> {
        let stale = |path: &'p PathBuf| {
            self.file(path)
                .is_some_and(|check| !check.passes_at(path))
        };
        if cas_paths.len() < PARALLEL_FILES_CHECK_MIN {
            cas_paths
                .values()
                .filter(|path| stale(path))
                .last()
                .map(PathBuf::as_path)
        } else {
            cas_paths
                .par_iter()
                .filter_map(|(_, path)| stale(path).then_some(path.as_path()))
                .reduce_with(|first, _| first)
        }
    }
}

/// The check of one recorded store file, run on metadata the caller read.
#[derive(Clone, Copy, Debug)]
pub struct StoreFileCheck<'a> {
    filename: &'a str,
    info: &'a CafsFileInfo,
    algo: &'a str,
    verified_files_cache: &'a VerifiedFilesCache,
}

impl StoreFileCheck<'_> {
    /// The size the row records for the file.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.info.size
    }

    /// Whether the store file at `path`, last modified at `mtime_ms`,
    /// still holds its recorded content. A file whose modification time
    /// did not move past the recorded check passes without a read. One
    /// whose time did is hashed, under the lock and with the cache
    /// [`PendingFilesCheck::verify`] uses.
    #[must_use]
    pub fn passes(&self, path: &Path, mtime_ms: u64) -> bool {
        if !is_modified_since(mtime_ms, self.info.checked_at) {
            return true;
        }
        self.cached_or(path, || verify_modified_file(path, self.filename, self.info, self.algo))
    }

    /// [`Self::passes`], reading the file's metadata here. A file that
    /// is missing or unreadable fails.
    #[must_use]
    pub fn passes_at(&self, path: &Path) -> bool {
        self.cached_or(path, || verify_file(path, self.filename, self.info, self.algo))
    }

    fn cached_or(&self, path: &Path, verify: impl FnOnce() -> bool) -> bool {
        if self.verified_files_cache.contains(path) {
            return true;
        }
        let passed = verify();
        if passed {
            self.verified_files_cache.insert(path.to_path_buf());
        }
        passed
    }
}
