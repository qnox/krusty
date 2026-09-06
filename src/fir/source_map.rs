//! Persistent source identities and origins without retaining source text or syntax.

use std::sync::Arc;

use super::body::OriginStore;
use super::identities::{next_id, SourceFileId};
use crate::types::{type_name, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    pub path: Arc<str>,
    pub package: TypeName,
}

#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    origins: OriginStore,
}

impl SourceMap {
    /// Identity is per SOURCE INPUT, not per path. Two inputs may legitimately carry the same file
    /// name — a multiplatform `B.kt` in a common module and again in a platform module — and giving
    /// them one id collapses two files into a single set of anchors, body-work items, and
    /// diagnostics. It also breaks the positional `SourceFileId::from_raw(file_index)` that the rest
    /// of the pipeline builds, because ids would stop matching input order.
    pub fn insert(&mut self, path: impl Into<String>) -> SourceFileId {
        let id = SourceFileId::from_raw(next_id(self.files.len(), "source files"));
        self.files.push(SourceFile {
            path: path.into().into(),
            package: TypeName::ROOT,
        });
        id
    }

    pub fn get(&self, id: SourceFileId) -> Option<&SourceFile> {
        self.files.get(id.raw() as usize)
    }

    pub fn set_package(&mut self, id: SourceFileId, package: Option<&str>) {
        let file = self
            .files
            .get_mut(id.raw() as usize)
            .expect("a source package requires an interned source identity");
        file.package = package
            .filter(|package| !package.is_empty())
            .map(|package| type_name(&package.replace('.', "/")))
            .unwrap_or(TypeName::ROOT);
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn origins(&self) -> &OriginStore {
        &self.origins
    }

    pub fn origins_mut(&mut self) -> &mut OriginStore {
        &mut self.origins
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.files
            .iter()
            .map(|file| std::mem::size_of::<SourceFile>() + file.path.len())
            .sum::<usize>()
            + self.origins.storage_payload_bytes()
    }

    pub(super) fn path_payload_bytes(&self) -> usize {
        self.files.iter().map(|file| file.path.len()).sum()
    }
}
