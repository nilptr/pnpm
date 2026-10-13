use super::{AnchoredImport, import_anchored};
use pnpm_config::PackageImportMethod;
use pnpm_fs::DirHandle;
use pnpm_reporter::SilentReporter;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicU8,
};
use tempfile::{TempDir, tempdir};

const STORE_ENTRY: &std::ffi::CStr = c"1b/59d9e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a9f8";

struct Fixture {
    root: TempDir,
    source_file: PathBuf,
    store: DirHandle,
    target_dir: DirHandle,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let files = root.path().join("store/files");
        let source_file = files.join(STORE_ENTRY.to_str().unwrap());
        fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        fs::write(&source_file, "content").unwrap();
        fs::create_dir(root.path().join("pkg")).unwrap();
        let store = DirHandle::open(&files).unwrap();
        let target_dir = DirHandle::open(&root.path().join("pkg")).unwrap();
        Fixture { root, source_file, store, target_dir }
    }

    fn import(&self, method: PackageImportMethod, source_file: &Path) -> bool {
        import_anchored::<SilentReporter>(
            &AtomicU8::new(0),
            method,
            source_file,
            &AnchoredImport {
                store: &self.store,
                store_entry: STORE_ENTRY,
                target_dir: &self.target_dir,
                target_name: c"index.js",
            },
        )
    }

    fn target(&self) -> PathBuf {
        self.root.path().join("pkg/index.js")
    }
}

#[test]
fn hardlink_shares_the_store_inode() {
    let fixture = Fixture::new();

    assert!(fixture.import(PackageImportMethod::Hardlink, &fixture.source_file));

    let source = fs::metadata(&fixture.source_file).unwrap();
    let target = fs::metadata(fixture.target()).unwrap();
    assert_eq!((target.dev(), target.ino()), (source.dev(), source.ino()));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs a filesystem that reflinks, which the ext4 of CI is not"
)]
#[test]
fn clone_materializes_an_independent_file() {
    let fixture = Fixture::new();

    assert!(fixture.import(PackageImportMethod::Clone, &fixture.source_file));

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "content");
    let source = fs::metadata(&fixture.source_file).unwrap();
    let target = fs::metadata(fixture.target()).unwrap();
    assert_ne!(target.ino(), source.ino());
    assert_eq!(target.mode(), source.mode());
}

#[test]
fn an_occupied_target_is_left_to_the_caller() {
    let fixture = Fixture::new();
    fs::write(fixture.target(), "other").unwrap();

    assert!(!fixture.import(PackageImportMethod::Hardlink, &fixture.source_file));
    assert!(!fixture.import(PackageImportMethod::Clone, &fixture.source_file));

    assert_eq!(fs::read_to_string(fixture.target()).unwrap(), "other");
}

#[test]
fn a_store_entry_written_under_another_umask_is_not_linked() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.source_file, fs::Permissions::from_mode(0o400)).unwrap();

    assert!(!fixture.import(PackageImportMethod::Hardlink, &fixture.source_file));

    assert!(!fixture.target().exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs a filesystem that reflinks, which the ext4 of CI is not"
)]
#[test]
fn a_clone_carrying_another_umask_is_left_to_the_caller() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.source_file, fs::Permissions::from_mode(0o400)).unwrap();

    assert!(!fixture.import(PackageImportMethod::Clone, &fixture.source_file));
}

#[test]
fn the_copy_method_and_non_store_sources_are_left_to_the_caller() {
    let fixture = Fixture::new();

    assert!(!fixture.import(PackageImportMethod::Copy, &fixture.source_file));
    assert!(!fixture.import(PackageImportMethod::Hardlink, Path::new("project/index.js")));

    assert!(!fixture.target().exists());
}
