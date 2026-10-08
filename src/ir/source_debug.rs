//! Source-file identities a class's source map needs when it inlines another file of this module.
//!
//! Common IR keeps the callee's source identity and source name. A target obtains physical owners
//! from its realization tables; this module does not reconstruct one from a source spelling.

use std::sync::Arc;

use crate::fir::SourceFileId;

/// One compilation input, in source-id order: its simple name (`Lib.kt`).
#[derive(Clone, Debug)]
pub(crate) struct SourceFileDebug {
    pub(crate) name: Arc<str>,
}

/// The module's source files, addressable by [`SourceFileId`].
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceDebug {
    files: Vec<SourceFileDebug>,
    current: Option<SourceFileId>,
}

impl SourceDebug {
    pub(crate) fn from_map(map: &crate::fir::SourceMap, current: SourceFileId) -> Self {
        let files = (0..map.len())
            .map(|raw| {
                let id = SourceFileId::from_raw(raw as u32);
                let file = map
                    .get(id)
                    .expect("source ids are allocated densely from zero");
                SourceFileDebug {
                    name: Arc::clone(&file.path),
                }
            })
            .collect();
        Self {
            files,
            current: Some(current),
        }
    }

    pub(crate) fn file(&self, source: SourceFileId) -> Option<&SourceFileDebug> {
        self.files.get(source.raw() as usize)
    }

    pub(crate) fn is_current(&self, source: SourceFileId) -> bool {
        self.current == Some(source)
    }
}
