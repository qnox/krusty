//! Diagnostics retained for workspace files that are not open.
//!
//! Files are `u32` ids into parallel `Vec`s and entries live in one flat buffer with per-file
//! offsets, so the store is a handful of allocations rather than one per file. Messages are
//! interned once globally: Kotlin compiler diagnostics repeat heavily across a workspace, so a
//! shared table collapses tens of thousands of duplicates.

use std::collections::HashMap;

use super::super::IndexedFile;
use krusty::diag::{DiagnosticKind, Severity};

/// `(start line, start UTF-16 column, end line, end UTF-16 column, packed severity + message id)`.
pub(crate) type WorkspaceEntry = [u32; 5];

use super::implementation::{
    DIAGNOSTIC_INSPECTION_BIT, DIAGNOSTIC_MESSAGE_MASK, DIAGNOSTIC_WARNING_BIT,
};

/// Ceiling on retained entries, so a workspace whose every file fails to compile cannot exhaust
/// memory. Reaching it stops admitting new files; already-indexed files keep their result ids.
const MAX_RETAINED_ENTRIES: usize = 512 * 1024;
/// Companion byte ceiling on the interned message table.
const MAX_RETAINED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct WorkspaceDiagnostics<'a> {
    pub entries: &'a [WorkspaceEntry],
    pub messages: &'a [String],
}

#[derive(Default)]
pub(crate) struct WorkspaceDiagnosticStore {
    files: HashMap<Box<str>, u32>,
    ranges: Vec<(u32, u32)>,
    text_hashes: Vec<u64>,
    entries: Vec<WorkspaceEntry>,
    messages: Vec<String>,
    message_ids: HashMap<Box<str>, u32>,
    message_bytes: usize,
    /// Generation the retained data belongs to. A batch from an older model is rejected outright.
    generation: u64,
    truncated: bool,
}

impl WorkspaceDiagnosticStore {
    /// Adopt a new project-model generation, discarding everything the previous model produced.
    pub(crate) fn reset_to(&mut self, generation: u64) {
        *self = WorkspaceDiagnosticStore {
            generation,
            ..WorkspaceDiagnosticStore::default()
        };
    }

    /// Merge one chunk. `attempted` is every URI the chunk tried; any that produced no result was
    /// deleted, unreadable, or rejected, so its retained data is removed rather than left stale.
    pub(crate) fn merge(
        &mut self,
        generation: u64,
        attempted: &[String],
        conclusive: bool,
        files: Vec<IndexedFile>,
        resolve: impl Fn(&str, &[[u32; 3]]) -> Vec<WorkspaceEntry>,
    ) -> bool {
        if generation < self.generation {
            return false;
        }
        if generation > self.generation {
            self.reset_to(generation);
        }
        let mut produced = std::collections::HashSet::with_capacity(files.len());
        for file in files {
            produced.insert(file.uri.clone());
            if self.entries.len() >= MAX_RETAINED_ENTRIES
                || self.message_bytes >= MAX_RETAINED_MESSAGE_BYTES
            {
                self.truncated = true;
                continue;
            }
            if self.text_hash(&file.uri) == Some(file.text_hash) {
                continue;
            }
            let pending: Vec<[u32; 3]> = file
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    // Same protocol-boundary casing the open-document path applies, so a message
                    // does not change appearance depending on whether its file happens to be open.
                    let message_id = self.intern(&super::implementation::lsp_diagnostic_message(
                        diagnostic.msg.clone(),
                    ));
                    let severity = match diagnostic.severity {
                        Severity::Error => 0,
                        Severity::Warning => DIAGNOSTIC_WARNING_BIT,
                    };
                    let kind = if diagnostic.kind == DiagnosticKind::Inspection {
                        DIAGNOSTIC_INSPECTION_BIT
                    } else {
                        0
                    };
                    let span = diagnostic.editor_span.unwrap_or(diagnostic.span);
                    [span.lo, span.hi.max(span.lo), severity | kind | message_id]
                })
                .collect();
            // Resolve while the text is still in hand: the store keeps no text, and re-reading the
            // file at pull time would race whatever the sweep is writing.
            let resolved = resolve(&file.text, &pending);
            let start = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
            self.entries.extend(resolved);
            let end = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
            let id = self.file_id(&file.uri);
            self.ranges[id as usize] = (start, end);
            self.text_hashes[id as usize] = file.text_hash;
        }
        // Only a run that actually happened may delete. An inconclusive chunk -- a worker restart,
        // say -- would otherwise erase a whole chunk's good diagnostics with no re-sweep to
        // restore them.
        if !conclusive {
            return true;
        }
        for uri in attempted {
            if !produced.contains(uri) {
                self.forget(uri);
            }
        }
        true
    }

    /// Remove a file's retained data. Its entries stay in the flat buffer until the next reset;
    /// dropping the range is what makes them unreachable, and compaction is not worth a memmove
    /// per deleted file.
    pub(crate) fn forget(&mut self, uri: &str) {
        if let Some(id) = self.files.remove(uri) {
            self.ranges[id as usize] = (0, 0);
            self.text_hashes[id as usize] = 0;
        }
    }

    pub(crate) fn diagnostics(&self, uri: &str) -> Option<WorkspaceDiagnostics<'_>> {
        let &id = self.files.get(uri)?;
        let (start, end) = self.ranges[id as usize];
        Some(WorkspaceDiagnostics {
            entries: &self.entries[start as usize..end as usize],
            messages: &self.messages,
        })
    }

    /// Files currently carrying retained data, for the workspace-wide report.
    pub(crate) fn indexed_uris(&self) -> Vec<String> {
        let mut uris: Vec<String> = self.files.keys().map(|uri| uri.to_string()).collect();
        uris.sort();
        uris
    }

    pub(crate) fn text_hash(&self, uri: &str) -> Option<u64> {
        let &id = self.files.get(uri)?;
        Some(self.text_hashes[id as usize])
    }

    fn file_id(&mut self, uri: &str) -> u32 {
        if let Some(&id) = self.files.get(uri) {
            return id;
        }
        let id = u32::try_from(self.ranges.len()).unwrap_or(u32::MAX);
        self.files.insert(uri.into(), id);
        self.ranges.push((0, 0));
        self.text_hashes.push(0);
        id
    }

    fn intern(&mut self, message: &str) -> u32 {
        if let Some(&id) = self.message_ids.get(message) {
            return id;
        }
        let id = u32::try_from(self.messages.len()).unwrap_or(DIAGNOSTIC_MESSAGE_MASK)
            & DIAGNOSTIC_MESSAGE_MASK;
        self.message_bytes = self.message_bytes.saturating_add(message.len());
        self.messages.push(message.to_string());
        self.message_ids.insert(message.into(), id);
        id
    }
}
