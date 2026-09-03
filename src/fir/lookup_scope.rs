//! Temporary packed source spellings and file scopes used only during Pass-1 lookup.

use std::collections::HashMap;

use crate::ast::File;

use super::identities::{next_id, LookupNameId, SourceFileId};

#[derive(Default)]
pub struct LookupNames {
    names: Vec<Box<str>>,
    by_spelling: HashMap<Box<str>, LookupNameId>,
}

impl LookupNames {
    pub fn intern(&mut self, spelling: &str) -> LookupNameId {
        if let Some(id) = self.by_spelling.get(spelling) {
            return *id;
        }
        let id = LookupNameId::from_raw(next_id(self.names.len(), "declaration lookup names"));
        let spelling: Box<str> = spelling.into();
        self.names.push(spelling.clone());
        self.by_spelling.insert(spelling, id);
        id
    }

    pub fn get(&self, id: LookupNameId) -> Option<&str> {
        self.names.get(id.raw() as usize).map(AsRef::as_ref)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.names.iter().map(|name| name.len()).sum()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LookupNameRange {
    pub(super) start: u32,
    pub(super) len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderImportRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderImport {
    pub path: LookupNameRange,
    pub wildcard: bool,
    pub alias: Option<LookupNameId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderFileScope {
    pub source: SourceFileId,
    pub package: LookupNameRange,
    pub imports: HeaderImportRange,
    /// Common/platform source role used by multiplatform annotation policy while compact headers
    /// are the only module-wide Pass-1 source view.
    pub is_common: bool,
    /// Whether this file enables explicit context arguments. Candidate applicability needs this
    /// language-mode fact during signature solving after the parser-owned file has been released.
    pub explicit_context_arguments: bool,
}

#[derive(Default)]
pub struct HeaderScopeArena {
    path_segments: Vec<LookupNameId>,
    imports: Vec<HeaderImport>,
    files: Vec<HeaderFileScope>,
}

impl HeaderScopeArena {
    fn add_path(&mut self, segments: impl IntoIterator<Item = LookupNameId>) -> LookupNameRange {
        let start = next_id(self.path_segments.len(), "header path segments");
        self.path_segments.extend(segments);
        let end = next_id(self.path_segments.len(), "header path segments");
        LookupNameRange {
            start,
            len: end - start,
        }
    }

    pub fn add_file(
        &mut self,
        source: SourceFileId,
        file: &File,
        is_common: bool,
        names: &mut LookupNames,
    ) {
        assert!(
            self.files.iter().all(|scope| scope.source != source),
            "a source file may publish its compact import scope only once"
        );
        let package_segments = file
            .package
            .as_deref()
            .into_iter()
            .flat_map(|package| package.split(['.', '/']))
            .filter(|segment| !segment.is_empty())
            .map(|segment| names.intern(segment))
            .collect::<Vec<_>>();
        let package = self.add_path(package_segments);
        let import_start = next_id(self.imports.len(), "header imports");
        for import in &file.import_paths {
            let segments = import
                .segments
                .iter()
                .map(|(segment, _)| names.intern(segment))
                .collect::<Vec<_>>();
            let path = self.add_path(segments);
            self.imports.push(HeaderImport {
                path,
                wildcard: import.wildcard,
                alias: import.alias.as_deref().map(|alias| names.intern(alias)),
            });
        }
        let import_end = next_id(self.imports.len(), "header imports");
        self.files.push(HeaderFileScope {
            source,
            package,
            imports: HeaderImportRange {
                start: import_start,
                len: import_end - import_start,
            },
            is_common,
            explicit_context_arguments: file.explicit_context_arguments,
        });
    }

    pub fn file(&self, source: SourceFileId) -> Option<HeaderFileScope> {
        self.files
            .iter()
            .find(|scope| scope.source == source)
            .copied()
    }

    pub fn path(&self, range: LookupNameRange) -> &[LookupNameId] {
        let start = range.start as usize;
        &self.path_segments[start..start + range.len as usize]
    }

    pub fn imports(&self, range: HeaderImportRange) -> &[HeaderImport] {
        let start = range.start as usize;
        &self.imports[start..start + range.len as usize]
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.path_segments.len() * std::mem::size_of::<LookupNameId>()
            + self.imports.len() * std::mem::size_of::<HeaderImport>()
            + self.files.len() * std::mem::size_of::<HeaderFileScope>()
    }
}
