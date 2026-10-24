#[cfg(unix)]
use super::{
    super::{
        Host, LINK_STATE_COPY, LINK_STATE_HARDLINK, LinkFileError, PathImport, auto_link,
        clone_or_copy_link, import_checked_into_fresh_target,
    },
    RecordedStoreFiles, now_ms, write_source,
};
#[cfg(unix)]
use pnpm_config::PackageImportMethod;
#[cfg(unix)]
use pnpm_reporter::SilentReporter;
#[cfg(unix)]
use pretty_assertions::assert_eq;
#[cfg(unix)]
use std::{
    fs,
    sync::atomic::{AtomicU8, Ordering},
};
#[cfg(unix)]
use tempfile::tempdir;

/// Once `Auto`'s state is `COPY`, we use `fs::copy` and must not
/// re-attempt reflink / hardlink. Observable: a successful link
/// with state pre-seeded to `COPY` has independent inodes (copy
/// semantics), not shared ones (hardlink).
#[test]
#[cfg(unix)]
fn auto_respects_cached_copy_state() {
    use std::os::unix::fs::MetadataExt;

    let state = AtomicU8::new(LINK_STATE_COPY);
    let tmp = tempdir().unwrap();
    let src = write_source(tmp.path(), "src.txt", b"cached-copy");
    let dst = tmp.path().join("dst.txt");

    auto_link::<SilentReporter, _>(&AtomicU8::new(0), &state, &PathImport::<Host>::new(&src, &dst))
        .expect("copy should succeed");

    assert_eq!(fs::read(&dst).unwrap(), b"cached-copy");
    assert_ne!(
        fs::metadata(&src).unwrap().ino(),
        fs::metadata(&dst).unwrap().ino(),
        "state=COPY must not hardlink",
    );
    assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_COPY, "state must not drift");
}
/// State=HARDLINK means Auto skips the reflink attempt and jumps
/// straight to `fs::hard_link`. Observable: shared inode on unix.
#[test]
#[cfg(unix)]
fn auto_respects_cached_hardlink_state() {
    use std::os::unix::fs::MetadataExt;

    let state = AtomicU8::new(LINK_STATE_HARDLINK);
    let tmp = tempdir().unwrap();
    let src = write_source(tmp.path(), "src.txt", b"cached-hardlink");
    let dst = tmp.path().join("dst.txt");

    auto_link::<SilentReporter, _>(&AtomicU8::new(0), &state, &PathImport::<Host>::new(&src, &dst))
        .expect("hardlink should succeed on same-FS tempdir");

    assert_eq!(
        fs::metadata(&src).unwrap().ino(),
        fs::metadata(&dst).unwrap().ino(),
        "state=HARDLINK must hardlink, not copy",
    );
    assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_HARDLINK, "state must not drift");
}
/// Pre-seed `CloneOrCopy` state to `COPY` and verify it uses
/// `fs::copy` — mirrors [`auto_respects_cached_copy_state`]. Also
/// confirms we skip the hardlink tier entirely (pnpm
/// `createCloneOrCopyImporter` has no hardlink fallback).
#[test]
#[cfg(unix)]
fn clone_or_copy_respects_cached_copy_state() {
    use std::os::unix::fs::MetadataExt;

    let state = AtomicU8::new(LINK_STATE_COPY);
    let tmp = tempdir().unwrap();
    let src = write_source(tmp.path(), "src.txt", b"cached");
    let dst = tmp.path().join("dst.txt");

    clone_or_copy_link::<SilentReporter, _>(
        &AtomicU8::new(0),
        &state,
        &PathImport::<Host>::new(&src, &dst),
    )
    .expect("copy should succeed");

    assert_ne!(
        fs::metadata(&src).unwrap().ino(),
        fs::metadata(&dst).unwrap().ino(),
        "state=COPY must not hardlink",
    );
    assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_COPY, "state must not drift");
}

/// The path-based import checks the store file the way the `*at` import
/// does: a hard link to a stale file is unlinked, a stale source is not
/// copied, and an unmodified file is linked without a read.
#[test]
#[cfg(unix)]
fn a_path_import_checks_the_store_file_as_it_places_it() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().unwrap();
    let src = write_source(tmp.path(), super::STORE_ENTRY, b"content");
    let dst = tmp.path().join("pkg/index.js");
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let import = |method, recorded: &RecordedStoreFiles| {
        import_checked_into_fresh_target::<SilentReporter>(
            &AtomicU8::new(0),
            method,
            &src,
            &dst,
            recorded.check().file(&src),
        )
    };

    let stale = RecordedStoreFiles::new(tmp.path(), &[("index.js", &src, 7, Some(0))]);
    for method in [PackageImportMethod::Hardlink, PackageImportMethod::Copy] {
        let error = import(method, &stale).unwrap_err();
        assert!(matches!(error, LinkFileError::StaleStoreFile { .. }), "{method:?}: {error}");
        assert!(!dst.exists(), "{method:?} must leave nothing at the target");
    }

    let unmodified = RecordedStoreFiles::new(tmp.path(), &[("index.js", &src, 7, Some(now_ms()))]);
    import(PackageImportMethod::Hardlink, &unmodified).unwrap();
    assert_eq!(fs::metadata(&dst).unwrap().ino(), fs::metadata(&src).unwrap().ino());
}
