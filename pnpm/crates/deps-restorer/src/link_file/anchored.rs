//! Importing a store file through open directories.

use super::{
    LINK_STATE_CLONE, LINK_STATE_HARDLINK, NPM_IMPORT_STATE, log_method_once,
    target_mode::desired_mode,
};
use pnpm_config::PackageImportMethod;
use pnpm_fs::{DirHandle, file_mode::store_inode_mode_is_linkable};
use pnpm_reporter::{PackageImportMethod as WireImportMethod, Reporter};
use std::{ffi::CStr, path::Path, sync::atomic::AtomicU8};

/// One store file and the dirent it is imported as, each named relative
/// to an open directory.
#[derive(Clone, Copy)]
pub(crate) struct AnchoredImport<'a> {
    pub(crate) store: &'a DirHandle,
    pub(crate) store_entry: &'a CStr,
    pub(crate) target_dir: &'a DirHandle,
    pub(crate) target_name: &'a CStr,
}

/// Import `source_file`, the store file `import` names, with the tier
/// `method` resolves to right now.
///
/// `false` means the target is not known to be finished, whatever the
/// reason. The caller then runs [`super::import_into_fresh_target`], which
/// owns every fallback, recovery and error, and adopts a target this call
/// did create.
pub(crate) fn import_anchored<Reporter: self::Reporter>(
    logged: &AtomicU8,
    method: PackageImportMethod,
    source_file: &Path,
    import: &AnchoredImport<'_>,
) -> bool {
    let Some(desired) = desired_mode(source_file) else { return false };
    match NPM_IMPORT_STATE.current_tier(method) {
        LINK_STATE_CLONE => clone_anchored::<Reporter>(logged, desired, import),
        LINK_STATE_HARDLINK => hardlink_anchored::<Reporter>(logged, desired, import),
        _ => false,
    }
}

/// A clone carries the store entry's mode, which is `desired` unless
/// another umask wrote the entry (pnpm/pnpm#3807).
fn clone_anchored<Reporter: self::Reporter>(
    logged: &AtomicU8,
    desired: u32,
    import: &AnchoredImport<'_>,
) -> bool {
    let AnchoredImport {
        store,
        store_entry,
        target_dir,
        target_name,
    } = *import;
    if target_dir.reflink_from(target_name, store, store_entry).is_err() {
        return false;
    }
    log_method_once::<Reporter>(logged, WireImportMethod::Clone);
    matches!(target_dir.regular_file_mode(target_name), Ok(Some(mode)) if mode == desired)
}

fn hardlink_anchored<Reporter: self::Reporter>(
    logged: &AtomicU8,
    desired: u32,
    import: &AnchoredImport<'_>,
) -> bool {
    let AnchoredImport {
        store,
        store_entry,
        target_dir,
        target_name,
    } = *import;
    let linkable = matches!(
        store.regular_file_mode(store_entry),
        Ok(Some(mode)) if store_inode_mode_is_linkable(mode, desired),
    );
    if !linkable || target_dir.hard_link_from(target_name, store, store_entry).is_err() {
        return false;
    }
    log_method_once::<Reporter>(logged, WireImportMethod::Hardlink);
    true
}

#[cfg(test)]
mod tests;
