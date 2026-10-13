//! The platforms [`populate_dir`](super::populate_dir) places every
//! entry by path on. Mirrors the items of `anchored.rs`, as types with
//! no value.

use pnpm_config::PackageImportMethod;
use pnpm_reporter::Reporter;
use std::{
    collections::HashMap,
    convert::Infallible,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::atomic::AtomicU8,
};

pub(super) struct PackageAnchors<'a>(Infallible, PhantomData<&'a ()>);

impl<'a> PackageAnchors<'a> {
    pub(super) fn open(_: &Path, _: &'a HashMap<String, PathBuf>) -> Option<Self> {
        None
    }

    pub(super) fn create_dir(&self, _: &str) -> bool {
        match self.0 {}
    }

    pub(super) fn target_dir(&self, _: Option<&Path>) -> Option<TargetDir<'_>> {
        match self.0 {}
    }
}

pub(super) struct TargetDir<'a>(Infallible, PhantomData<&'a ()>);

impl TargetDir<'_> {
    pub(super) fn import<Reporter: self::Reporter>(
        &self,
        _: &AtomicU8,
        _: PackageImportMethod,
        _: &str,
        _: &Path,
    ) -> bool {
        match self.0 {}
    }
}
