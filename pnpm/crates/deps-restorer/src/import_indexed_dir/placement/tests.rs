use super::{
    FsRemoveDirAll, FsRemoveNonDirDirent, Placement, clear_dir_blocking_file,
    clear_dirent_blocking_dir, dir_fits_at, entries_by_target_dir, file_fits_at, populate_dir,
    symlinks::{SymlinkRoots, final_link_target},
};
use pnpm_config::PackageImportMethod;
use pnpm_reporter::SilentReporter;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError, atomic::AtomicU8},
};
use tempfile::tempdir;

/// Keyed by absolute path, so tests sharing the process only ever read
/// their own directories.
static DIRECTORY_WRITERS: Mutex<BTreeMap<PathBuf, DirectoryWriters>> = Mutex::new(BTreeMap::new());

#[derive(Default)]
pub(super) struct DirectoryWriters {
    current: usize,
    max: usize,
}

impl DirectoryWriters {
    /// Count one worker into the directory `cleaned_entry` lands in until
    /// the guard drops.
    pub(super) fn enter(dir_path: &Path, cleaned_entry: &str) -> DirectoryWriter {
        let directory =
            dir_path.join(Path::new(cleaned_entry).parent().unwrap_or_else(|| Path::new("")));
        let mut writers = DIRECTORY_WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
        let writers = writers.entry(directory.clone()).or_default();
        writers.current += 1;
        writers.max = writers.max.max(writers.current);
        DirectoryWriter { directory }
    }

    fn max_at_once(directory: &Path) -> usize {
        DIRECTORY_WRITERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(directory)
            .map_or(0, |writers| writers.max)
    }
}

pub(super) struct DirectoryWriter {
    directory: PathBuf,
}

impl Drop for DirectoryWriter {
    fn drop(&mut self) {
        let mut writers = DIRECTORY_WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(writers) = writers.get_mut(&self.directory) {
            writers.current -= 1;
        }
    }
}

#[test]
fn a_directory_has_one_writer_at_a_time() {
    let root = tempdir().unwrap();
    let store = root.path().join("store");
    fs::create_dir(&store).unwrap();
    let mut cas_paths: HashMap<String, PathBuf> = HashMap::new();
    for directory in ["lib", "lib/esm"] {
        for index in 0..200 {
            let store_path = store.join(format!("{}-{index}", directory.replace('/', "-")));
            fs::write(&store_path, b"payload").unwrap();
            cas_paths.insert(format!("{directory}/{index}.js"), store_path);
        }
    }
    let manifest = store.join("package.json");
    fs::write(&manifest, b"{}").unwrap();
    cas_paths.insert("package.json".to_string(), manifest);
    let target = root.path().join("pkg");

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Copy,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    for directory in ["lib", "lib/esm"] {
        assert_eq!(DirectoryWriters::max_at_once(&target.join(directory)), 1, "{directory}");
    }
    assert!(target.join("lib/199.js").is_file());
    assert!(target.join("lib/esm/199.js").is_file());
    assert!(target.join("package.json").is_file());
}

#[test]
fn directories_with_more_entries_come_first() {
    let cas_paths: HashMap<String, PathBuf> = ["a/1", "b/1", "b/2", "b/3", "c/1", "c/2", "d"]
        .into_iter()
        .map(|entry| (entry.to_string(), PathBuf::from("store").join(entry)))
        .collect();

    let sizes: Vec<usize> = entries_by_target_dir(&cas_paths, None)
        .iter()
        .map(|group| group.entries.len())
        .collect();

    assert_eq!(sizes, [3, 2, 1, 1]);
}

#[test]
fn entries_are_grouped_by_the_directory_they_land_in() {
    let cas_paths: HashMap<String, PathBuf> =
        ["package.json", "README.md", "lib/index.js", "lib/util.js", "lib/esm/index.js"]
            .into_iter()
            .map(|entry| (entry.to_string(), PathBuf::from("store").join(entry)))
            .collect();

    let groups: BTreeSet<BTreeSet<&str>> = entries_by_target_dir(&cas_paths, Some("package.json"))
        .iter()
        .map(|group| {
            group.entries
                .iter()
                .map(|(entry, store_path)| {
                    assert_eq!(group.target_dir, Path::new(entry).parent());
                    assert_eq!(*store_path, cas_paths[*entry]);
                    *entry
                })
                .collect()
        })
        .collect();

    assert_eq!(
        groups,
        BTreeSet::from([
            BTreeSet::from(["README.md"]),
            BTreeSet::from(["lib/index.js", "lib/util.js"]),
            BTreeSet::from(["lib/esm/index.js"]),
        ]),
    );
}

#[test]
fn nothing_at_a_path_fits_either_kind() {
    let root = tempdir().unwrap();
    let absent = root.path().join("absent");

    assert!(dir_fits_at(&absent));
    assert!(file_fits_at(&absent));
}

#[test]
fn a_directory_fits_only_where_a_directory_belongs() {
    let root = tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();

    assert!(dir_fits_at(&target));
    assert!(!file_fits_at(&target));
}

#[test]
fn a_file_fits_only_where_a_file_belongs() {
    let root = tempdir().unwrap();
    let target = root.path().join("target");
    fs::write(&target, b"payload").unwrap();

    assert!(file_fits_at(&target));
    assert!(!dir_fits_at(&target));
}

// Each fake stands in for the installer sharing the slot, which can finish
// the same work between this one's inspection and its removal. The removal
// always fails, because that is what the loser of the race sees; what the
// path holds by then is what the fakes vary.
struct ReplacedByWhatBelongs;
struct RemovedByTheOtherInstaller;
struct LeavesTheBlocker;

impl FsRemoveDirAll for ReplacedByWhatBelongs {
    fn remove_dir_all(path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)?;
        fs::write(path, b"payload")?;
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

impl FsRemoveDirAll for RemovedByTheOtherInstaller {
    fn remove_dir_all(path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)?;
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

impl FsRemoveDirAll for LeavesTheBlocker {
    fn remove_dir_all(_: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

impl FsRemoveNonDirDirent for ReplacedByWhatBelongs {
    fn remove_non_dir_dirent(path: &Path, _: fs::FileType) -> io::Result<()> {
        fs::remove_file(path)?;
        fs::create_dir(path)?;
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

impl FsRemoveNonDirDirent for RemovedByTheOtherInstaller {
    fn remove_non_dir_dirent(path: &Path, _: fs::FileType) -> io::Result<()> {
        fs::remove_file(path)?;
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

impl FsRemoveNonDirDirent for LeavesTheBlocker {
    fn remove_non_dir_dirent(_: &Path, _: fs::FileType) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

#[test]
fn a_directory_replaced_by_the_file_counts_as_cleared() {
    let root = tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();

    clear_dir_blocking_file::<ReplacedByWhatBelongs>(&target)
        .expect("the path holds the file the clearing was for");
}

#[test]
fn a_directory_removed_by_the_other_installer_counts_as_cleared() {
    let root = tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();

    clear_dir_blocking_file::<RemovedByTheOtherInstaller>(&target)
        .expect("nothing stands where the file belongs");
}

#[test]
fn a_directory_still_standing_fails_the_clearing() {
    let root = tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();

    clear_dir_blocking_file::<LeavesTheBlocker>(&target)
        .expect_err("the blocker is still in the way");
}

#[test]
fn a_file_replaced_by_the_directory_counts_as_cleared() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("nested"), b"blocker").unwrap();

    clear_dirent_blocking_dir::<ReplacedByWhatBelongs>(root.path(), "nested")
        .expect("the path holds the directory the clearing was for");
}

#[test]
fn a_file_removed_by_the_other_installer_counts_as_cleared() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("nested"), b"blocker").unwrap();

    clear_dirent_blocking_dir::<RemovedByTheOtherInstaller>(root.path(), "nested")
        .expect("nothing stands where the directory belongs");
}

#[test]
fn a_file_still_standing_fails_the_clearing() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("nested"), b"blocker").unwrap();

    clear_dirent_blocking_dir::<LeavesTheBlocker>(root.path(), "nested")
        .expect_err("the blocker is still in the way");
}

#[test]
fn final_link_target_points_into_the_final_directory() {
    let imported = HashSet::new();
    let roots = SymlinkRoots {
        written_dir: Path::new("/stage"),
        final_dir: Path::new("/pkg"),
        imported: &imported,
    };
    assert_eq!(
        final_link_target(Path::new("/stage/lib/link"), Path::new("../sub"), roots),
        PathBuf::from("/pkg/sub"),
    );
    assert_eq!(
        final_link_target(Path::new("/stage/link"), Path::new("sub/dir"), roots),
        PathBuf::from("/pkg/sub/dir"),
    );
}

/// A store under `root` holding `entries`, each written at `mode`, and the
/// `cas_paths` that import them.
#[cfg(unix)]
fn store_with_entries(root: &Path, entries: &[(&str, &str, u32)]) -> HashMap<String, PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    entries
        .iter()
        .enumerate()
        .map(|(index, (entry, suffix, mode))| {
            let store_path = root.join(format!("store/files/0{index}/{:040x}{suffix}", index + 1));
            fs::create_dir_all(store_path.parent().unwrap()).unwrap();
            fs::write(&store_path, entry.as_bytes()).unwrap();
            fs::set_permissions(&store_path, fs::Permissions::from_mode(*mode)).unwrap();
            (entry.to_string(), store_path)
        })
        .collect()
}

#[cfg(unix)]
fn store_mode(executable: bool) -> u32 {
    pnpm_fs::file_mode::store_entry_mode(executable, pnpm_fs::file_mode::current_umask())
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

#[cfg(unix)]
#[test]
fn a_fresh_import_links_store_entries_into_nested_directories() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[
            ("package.json", "", store_mode(false)),
            ("lib/esm/deep/index.js", "", store_mode(false)),
            ("bin/cli", "-exec", store_mode(true)),
        ],
    );
    let target = root.path().join("pkg");

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Hardlink,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    for (entry, store_path) in &cas_paths {
        assert!(same_file::is_same_file(target.join(entry), store_path).unwrap(), "{entry}");
    }
}

#[cfg(unix)]
#[test]
fn a_fresh_copy_carries_the_store_entry_mode() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[("package.json", "", 0o600), ("lib/index.js", "", 0o600), ("bin/cli", "-exec", 0o700)],
    );
    let target = root.path().join("pkg");

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Copy,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    for (entry, executable) in [("package.json", false), ("lib/index.js", false), ("bin/cli", true)]
    {
        assert_eq!(fs::read_to_string(target.join(entry)).unwrap(), entry);
        assert_eq!(mode_of(&target.join(entry)), store_mode(executable), "{entry}");
    }
}

#[cfg(unix)]
#[test]
fn a_fresh_hardlink_of_a_store_entry_with_a_wider_mode_is_replaced_by_a_copy() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[("package.json", "", store_mode(false)), ("index.js", "", 0o666)],
    );
    let target = root.path().join("pkg");

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Hardlink,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    let imported = target.join("index.js");
    assert!(!same_file::is_same_file(&imported, &cas_paths["index.js"]).unwrap());
    assert_eq!(fs::read_to_string(&imported).unwrap(), "index.js");
    assert_eq!(mode_of(&imported), store_mode(false));
    assert_eq!(mode_of(&cas_paths["index.js"]), 0o666);
}

#[cfg(unix)]
#[test]
fn a_fresh_import_adopts_a_file_a_concurrent_importer_placed() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[("package.json", "", store_mode(false)), ("lib/index.js", "", store_mode(false))],
    );
    let target = root.path().join("pkg");
    fs::create_dir_all(target.join("lib")).unwrap();
    fs::hard_link(&cas_paths["lib/index.js"], target.join("lib/index.js")).unwrap();

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Copy,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    assert!(
        same_file::is_same_file(target.join("lib/index.js"), &cas_paths["lib/index.js"]).unwrap()
    );
    assert_eq!(fs::read_to_string(target.join("package.json")).unwrap(), "package.json");
}

#[cfg(unix)]
#[test]
fn a_fresh_import_reports_the_file_where_a_directory_belongs() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[("package.json", "", store_mode(false)), ("lib/index.js", "", store_mode(false))],
    );
    let target = root.path().join("pkg");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("lib"), b"not a directory").unwrap();

    let error = populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::Copy,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap_err();

    dbg!(&error);
    assert!(matches!(
        error,
        super::ImportIndexedDirError::CreateDir { ref dirname, .. } if *dirname == target.join("lib"),
    ));
}

/// Covers both clone tiers: `clonefile` copies the source's wider mode,
/// which is then aligned, and a Linux filesystem without `FICLONE` must
/// remove the empty target it created before the copy tier writes there.
#[cfg(unix)]
#[test]
fn a_fresh_clone_or_copy_carries_the_store_entry_mode() {
    let root = tempdir().unwrap();
    let cas_paths = store_with_entries(
        root.path(),
        &[("package.json", "", 0o666), ("lib/index.js", "", 0o666), ("bin/cli", "-exec", 0o777)],
    );
    let target = root.path().join("pkg");

    populate_dir::<SilentReporter>(
        &AtomicU8::new(0),
        PackageImportMethod::CloneOrCopy,
        &target,
        &cas_paths,
        Placement::Fresh,
        None,
    )
    .unwrap();

    for (entry, executable) in [("package.json", false), ("lib/index.js", false), ("bin/cli", true)]
    {
        assert_eq!(fs::read_to_string(target.join(entry)).unwrap(), entry);
        assert_eq!(mode_of(&target.join(entry)), store_mode(executable), "{entry}");
    }
    assert_eq!(mode_of(&cas_paths["lib/index.js"]), 0o666);
}
