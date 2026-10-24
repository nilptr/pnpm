use super::{
    super::{
        LinkFileError, import_fresh, is_stale_store_file,
        tests::{RecordedStoreFiles, now_ms},
    },
    AtImport, AtPath, FileImport, StoreFilesDir,
};
use pnpm_config::PackageImportMethod;
use pnpm_fs::file_mode::{current_umask, store_entry_mode};
use pnpm_reporter::SilentReporter;
use pnpm_store_dir::StoreFileCheck;
use std::{
    fs, io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::AtomicU8,
};
use tempfile::{TempDir, tempdir};

/// A non-executable store entry below the store's `files` directory.
const STORE_ENTRY: &str = "1b/59d9e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a9f8";

struct Fixture {
    root: TempDir,
    source_path: PathBuf,
    store_files: Option<StoreFilesDir>,
    package_dir: OwnedFd,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let source_path = root
            .path()
            .join("store/files")
            .join(STORE_ENTRY);
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "content").unwrap();
        fs::create_dir(root.path().join("pkg")).unwrap();
        let store_files = StoreFilesDir::open([source_path.as_path()].into_iter());
        assert!(store_files.is_some(), "the fixture lays the store entry out as the store does");
        let package_dir = AtPath::cwd(&root.path().join("pkg")).open_dir().unwrap();
        Fixture { root, source_path, store_files, package_dir }
    }

    fn import_of<'a>(&'a self, source_path: &'a Path) -> AtImport<'a> {
        self.import_checked(source_path, None)
    }

    fn import_checked<'a>(
        &'a self,
        source_path: &'a Path,
        check: Option<StoreFileCheck<'a>>,
    ) -> AtImport<'a> {
        AtImport::new(
            source_path,
            StoreFilesDir::locate(self.store_files.as_ref(), source_path),
            AtPath::new(self.package_dir.as_fd(), Path::new("index.js")),
            check,
        )
    }

    fn import(&self) -> AtImport<'_> {
        self.import_of(&self.source_path)
    }

    /// The row recording the store entry as checked at `checked_at`. The
    /// entry's content is not what its digest says, so a check that
    /// hashes it fails.
    fn recorded(&self, checked_at: Option<u64>) -> RecordedStoreFiles {
        RecordedStoreFiles::new(
            &self.root.path().join("store"),
            &[("index.js", &self.source_path, 7, checked_at)],
        )
    }

    fn target(&self) -> PathBuf {
        self.root.path().join("pkg/index.js")
    }

    fn set_source_mode(&self, mode: u32) {
        fs::set_permissions(&self.source_path, fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o7777
}

fn desired_mode() -> u32 {
    store_entry_mode(false, current_umask())
}

#[test]
fn a_hard_link_shares_the_store_inode_and_keeps_its_mode() {
    let fixture = Fixture::new();
    fixture.set_source_mode(desired_mode());

    fixture.import().hard_link().unwrap();

    let source = fs::metadata(&fixture.source_path).unwrap();
    let target = fs::metadata(fixture.target()).unwrap();
    assert_eq!((target.dev(), target.ino()), (source.dev(), source.ino()));
    assert!(
        fixture
            .import()
            .linked_mode_is_linkable()
            .unwrap()
    );
}

#[test]
fn a_store_entry_written_under_another_umask_is_copied_at_the_desired_mode() {
    let fixture = Fixture::new();
    fixture.set_source_mode(0o600);
    let import = fixture.import();

    import.hard_link().unwrap();
    assert!(!import.linked_mode_is_linkable().unwrap());
    import.remove_target().unwrap();
    import.copy().unwrap();

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "content");
    assert_ne!(
        fs::metadata(fixture.target()).unwrap().ino(),
        fs::metadata(&fixture.source_path).unwrap().ino(),
    );
    assert_eq!(mode_of(&fixture.target()), desired_mode());
    assert_eq!(mode_of(&fixture.source_path), 0o600);
}

#[test]
fn a_copy_of_a_file_outside_the_store_keeps_its_own_mode() {
    let fixture = Fixture::new();
    let local = fixture.root.path().join("project/index.js");
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    fs::write(&local, "local").unwrap();
    fs::set_permissions(&local, fs::Permissions::from_mode(0o640)).unwrap();

    fixture
        .import_of(&local)
        .copy()
        .unwrap();

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "local");
    assert_eq!(mode_of(&fixture.target()), 0o640);
}

#[test]
fn an_occupied_target_is_an_error_and_is_left_alone() {
    let fixture = Fixture::new();
    fs::write(fixture.target(), "other").unwrap();
    let import = fixture.import();

    for result in [import.hard_link(), import.copy(), import.reflink()] {
        let error = result.unwrap_err();
        assert!(
            matches!(error.kind(), io::ErrorKind::AlreadyExists | io::ErrorKind::Unsupported),
            "{error}",
        );
    }

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "other");
}

#[test]
fn a_squatting_symlink_is_not_written_through() {
    let fixture = Fixture::new();
    let elsewhere = fixture.root.path().join("elsewhere");
    fs::write(&elsewhere, "untouched").unwrap();
    std::os::unix::fs::symlink(&elsewhere, fixture.target()).unwrap();
    let import = fixture.import();

    let mode_before = mode_of(&elsewhere);

    assert_eq!(import.copy().unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    #[cfg(target_os = "macos")]
    import.align_target_mode().unwrap_err();

    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "untouched");
    assert_eq!(mode_of(&elsewhere), mode_before);
}

#[cfg_attr(
    target_os = "linux",
    ignore = "needs a filesystem that reflinks, which the ext4 of CI is not"
)]
#[test]
fn a_clone_is_aligned_with_the_desired_mode() {
    let fixture = Fixture::new();
    fixture.set_source_mode(0o600);
    let import = fixture.import();

    import.reflink().unwrap();
    import.align_target_mode().unwrap();

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "content");
    assert_ne!(
        fs::metadata(fixture.target()).unwrap().ino(),
        fs::metadata(&fixture.source_path).unwrap().ino(),
    );
    assert_eq!(mode_of(&fixture.target()), desired_mode());
    assert_eq!(mode_of(&fixture.source_path), 0o600);
}

#[test]
fn a_hard_link_to_a_store_file_checked_since_it_changed_passes_without_a_read() {
    let fixture = Fixture::new();
    fixture.set_source_mode(desired_mode());
    let recorded = fixture.recorded(Some(now_ms()));
    let files = recorded.check();
    let import = fixture.import_checked(&fixture.source_path, files.file(&fixture.source_path));

    import.hard_link().unwrap();

    assert!(import.linked_mode_is_linkable().unwrap());
    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "content");
}

#[test]
fn a_stale_store_file_is_not_left_at_the_target_by_any_tier() {
    let fixture = Fixture::new();
    fixture.set_source_mode(desired_mode());
    let recorded = fixture.recorded(Some(0));
    let files = recorded.check();
    let import = fixture.import_checked(&fixture.source_path, files.file(&fixture.source_path));

    import.hard_link().unwrap();
    assert!(is_stale_store_file(&import.linked_mode_is_linkable().unwrap_err()));
    assert!(!fixture.target().exists(), "the stale link is removed");

    assert!(is_stale_store_file(&import.copy().unwrap_err()));
    assert!(!fixture.target().exists(), "nothing is copied from a stale source");

    #[cfg(target_os = "macos")]
    {
        import.reflink().unwrap();
        assert!(is_stale_store_file(&import.align_target_mode().unwrap_err()));
        assert!(!fixture.target().exists(), "the stale clone is removed");
    }
}

#[test]
fn an_import_reports_a_stale_or_missing_store_file_for_the_caller_to_fetch_again() {
    let fixture = Fixture::new();
    let recorded = fixture.recorded(Some(0));
    let files = recorded.check();
    let import = fixture.import_checked(&fixture.source_path, files.file(&fixture.source_path));
    let import_fresh = |import: &AtImport<'_>| {
        import_fresh::<SilentReporter, _>(
            &AtomicU8::new(0),
            PackageImportMethod::Hardlink,
            import,
            &fixture.source_path,
            || fixture.target(),
        )
    };

    let error = import_fresh(&import).unwrap_err();
    assert!(
        matches!(&error, LinkFileError::StaleStoreFile { from } if *from == fixture.source_path)
    );
    assert!(!fixture.target().exists());

    fs::remove_file(&fixture.source_path).unwrap();
    let error = import_fresh(&import).unwrap_err();
    assert!(matches!(error, LinkFileError::StaleStoreFile { .. }), "{error}");

    let unchecked = fixture.import();
    let error = import_fresh(&unchecked).unwrap_err();
    assert!(matches!(error, LinkFileError::Import { .. }), "{error}");
}
