use super::DirHandle;
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use tempfile::tempdir;

#[test]
fn hard_link_from_links_between_two_open_directories() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("store")).unwrap();
    fs::write(root.path().join("store/blob"), "content").unwrap();
    let handle = DirHandle::open(root.path()).unwrap();
    handle.create_dir(c"pkg").unwrap();
    let store = handle.open_child(c"store").unwrap();
    let pkg = handle.open_child(c"pkg").unwrap();

    pkg.hard_link_from(c"index.js", &store, c"blob").unwrap();

    let source = fs::metadata(root.path().join("store/blob")).unwrap();
    let target = fs::metadata(root.path().join("pkg/index.js")).unwrap();
    assert_eq!((target.dev(), target.ino()), (source.dev(), source.ino()));
    let error = pkg.hard_link_from(c"index.js", &store, c"blob").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs a filesystem that reflinks, which the ext4 of CI is not"
)]
#[test]
fn reflink_from_clones_into_a_new_inode() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("blob"), "content").unwrap();
    let handle = DirHandle::open(root.path()).unwrap();

    handle.reflink_from(c"clone", &handle, c"blob").unwrap();

    assert_eq!(fs::read_to_string(root.path().join("clone")).unwrap(), "content");
    let source = fs::metadata(root.path().join("blob")).unwrap();
    let target = fs::metadata(root.path().join("clone")).unwrap();
    assert_ne!(target.ino(), source.ino());
}

#[test]
fn regular_file_mode_reports_permission_bits_of_regular_files_only() {
    let root = tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, "").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("file", root.path().join("link")).unwrap();
    let handle = DirHandle::open(root.path()).unwrap();

    assert_eq!(handle.regular_file_mode(c"file").unwrap(), Some(0o640));
    assert_eq!(handle.regular_file_mode(c"link").unwrap(), None);
    assert_eq!(handle.regular_file_mode(c".").unwrap(), None);
    assert_eq!(
        handle
            .regular_file_mode(c"missing")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
    );
}

#[test]
fn open_rejects_a_file() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("file"), "").unwrap();
    DirHandle::open(&root.path().join("file")).unwrap_err();
    DirHandle::open(root.path())
        .unwrap()
        .open_child(c"file")
        .unwrap_err();
}
