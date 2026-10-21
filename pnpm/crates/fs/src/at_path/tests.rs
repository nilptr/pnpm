use super::AtPath;
use std::{
    fs, io,
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::Path,
};
use tempfile::tempdir;

#[test]
fn create_dir_accepts_a_directory_already_there_but_not_a_file() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("file"), "").unwrap();
    std::os::unix::fs::symlink(".", root.path().join("link-to-dir")).unwrap();
    let dir = AtPath::cwd(root.path()).open_dir().unwrap();

    AtPath::new(dir.as_fd(), Path::new("sub")).create_dir().unwrap();
    AtPath::new(dir.as_fd(), Path::new("sub")).create_dir().unwrap();
    AtPath::new(dir.as_fd(), Path::new("link-to-dir")).create_dir().unwrap();
    let error = AtPath::new(dir.as_fd(), Path::new("file")).create_dir().unwrap_err();

    assert!(root.path().join("sub").is_dir());
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn open_dir_rejects_a_file() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("file"), "").unwrap();

    AtPath::cwd(&root.path().join("file")).open_dir().unwrap_err();
}

#[test]
fn create_new_refuses_an_occupied_path_and_a_squatting_symlink() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("file"), "").unwrap();
    std::os::unix::fs::symlink("missing", root.path().join("link")).unwrap();
    let dir = AtPath::cwd(root.path()).open_dir().unwrap();

    AtPath::new(dir.as_fd(), Path::new("fresh")).create_new(0o644).unwrap();
    let occupied = AtPath::new(dir.as_fd(), Path::new("file")).create_new(0o644).unwrap_err();
    let squatted = AtPath::new(dir.as_fd(), Path::new("link")).create_new(0o644).unwrap_err();

    assert_eq!(occupied.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(squatted.kind(), io::ErrorKind::AlreadyExists);
    assert!(!root.path().join("missing").exists());
}

#[test]
fn still_names_follows_the_dirent_not_the_name() {
    let root = tempdir().unwrap();
    let dir = AtPath::cwd(root.path()).open_dir().unwrap();
    let path = AtPath::new(dir.as_fd(), Path::new("file"));
    let created = path.create_new(0o644).unwrap();
    assert!(path.still_names(&created));
    assert_eq!(
        rustix::fs::fstat(&created).unwrap().st_ino,
        fs::metadata(root.path().join("file")).unwrap().ino(),
    );

    fs::write(root.path().join("other"), "").unwrap();
    fs::rename(root.path().join("other"), root.path().join("file")).unwrap();
    assert!(!path.still_names(&created));

    path.remove_file().unwrap();
    assert!(!path.still_names(&created));
}
