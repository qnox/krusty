//! Uses retained from closed-file chunks.
//!
//! Each use names its declaration by URI and name span, the same identity a parse-only symbol
//! records. File indexes from the chunk that produced the use are not stored: they do not match
//! the next source set.

use std::collections::{HashMap, HashSet};

use super::line_index::{byte_offset_to_position, Position};
use crate::{DeclarationIdentity, IndexedFile};
use krusty::diag::Span;

pub(crate) struct WorkspaceReferenceHit {
    pub uri: String,
    pub start: Position,
    pub end: Position,
    pub declaration: DeclarationIdentity,
    pub use_span: Span,
}

#[derive(Default)]
pub(crate) struct WorkspaceReferenceStore {
    generation: u64,
    by_file: HashMap<String, Vec<StoredUse>>,
}

struct StoredUse {
    start: Position,
    end: Position,
    use_span: Span,
    declaration_uri: String,
    declaration_span: Span,
}

impl WorkspaceReferenceStore {
    pub(crate) fn reset_to(&mut self, generation: u64) {
        *self = Self {
            generation,
            by_file: HashMap::new(),
        };
    }

    /// Merge one chunk. `attempted` is every URI the chunk tried. A conclusive chunk drops a URI
    /// it no longer produced; an inconclusive one leaves what was already stored.
    pub(crate) fn merge(
        &mut self,
        generation: u64,
        attempted: &[String],
        conclusive: bool,
        files: &[IndexedFile],
    ) {
        if generation < self.generation {
            return;
        }
        if generation > self.generation {
            self.reset_to(generation);
        }
        let mut produced = HashSet::with_capacity(files.len());
        for file in files {
            produced.insert(file.uri.clone());
            let uses = resolve_uses(file);
            if uses.is_empty() {
                self.by_file.remove(&file.uri);
            } else {
                self.by_file.insert(file.uri.clone(), uses);
            }
        }
        if !conclusive {
            return;
        }
        for uri in attempted {
            if !produced.contains(uri) {
                self.by_file.remove(uri);
            }
        }
    }

    pub(crate) fn matching(
        &self,
        identities: &HashSet<DeclarationIdentity>,
    ) -> Vec<WorkspaceReferenceHit> {
        let mut hits = Vec::new();
        for (uri, uses) in &self.by_file {
            for use_ in uses {
                let declaration = DeclarationIdentity {
                    uri: use_.declaration_uri.clone(),
                    span: use_.declaration_span,
                };
                if !identities.contains(&declaration) {
                    continue;
                }
                hits.push(WorkspaceReferenceHit {
                    uri: uri.clone(),
                    start: use_.start,
                    end: use_.end,
                    declaration,
                    use_span: use_.use_span,
                });
            }
        }
        hits
    }
}

fn resolve_uses(file: &IndexedFile) -> Vec<StoredUse> {
    let mut uses = Vec::new();
    for (span, target) in file.references.occurrences() {
        let Some(declaration_uri) = file.references.file_uri(target.file) else {
            continue;
        };
        let start = byte_offset_to_position(&file.text, span.lo as usize);
        let end = byte_offset_to_position(&file.text, span.hi as usize);
        uses.push(StoredUse {
            start,
            end,
            use_span: span,
            declaration_uri: declaration_uri.to_string(),
            declaration_span: target.span,
        });
    }
    uses
}
