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

/// Superseded entries tolerated before the store compacts. Replacing a file appends its new slice,
/// so readers always see one contiguous slice per file; compaction reclaims the old ones.
const COMPACTION_STALE_ENTRIES: usize = 1024;

pub(crate) struct WorkspaceDiagnostics<'a> {
    pub entries: &'a [WorkspaceEntry],
    pub messages: &'a [String],
}

pub(crate) struct WorkspaceMergeOutcome {
    pub accepted: bool,
    pub changed: bool,
}

#[derive(Default)]
pub(crate) struct WorkspaceDiagnosticStore {
    files: HashMap<Box<str>, u32>,
    ranges: Vec<(u32, u32)>,
    text_hashes: Vec<u64>,
    /// Tombstoned slots are reused so a long session creating and deleting distinct paths cannot
    /// grow the parallel file metadata forever while the live `files` map stays small.
    free_file_ids: Vec<u32>,
    entries: Vec<WorkspaceEntry>,
    messages: Vec<String>,
    message_ids: HashMap<Box<str>, u32>,
    /// Entries reachable through current file ranges. `entries.len()` may be larger between
    /// compactions because replacing one file appends its new compact slice.
    live_entries: usize,
    /// Generation the retained data belongs to. A batch from an older model is rejected outright.
    generation: u64,
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
    ) -> WorkspaceMergeOutcome {
        if generation < self.generation {
            return WorkspaceMergeOutcome {
                accepted: false,
                changed: false,
            };
        }
        let mut changed = false;
        if generation > self.generation {
            self.reset_to(generation);
            changed = true;
        }
        let mut produced = std::collections::HashSet::with_capacity(files.len());
        for file in files {
            produced.insert(file.uri.clone());
            if self.text_hash(&file.uri) == Some(file.text_hash) {
                continue;
            }
            self.compact_if_wasteful();
            let mut pending = Vec::with_capacity(file.diagnostics.len());
            for diagnostic in file.diagnostics {
                // Same protocol-boundary casing the open-document path applies, so a message does
                // not change appearance depending on whether its file happens to be open.
                let message = super::implementation::lsp_diagnostic_message(diagnostic.msg);
                let message_id = self.intern(&message);
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
                pending.push([span.lo, span.hi.max(span.lo), severity | kind | message_id]);
            }
            // Resolve while the text is still in hand: the store keeps no text, and re-reading the
            // file at pull time would race whatever the sweep is writing.
            let resolved = resolve(&file.text, &pending);
            let start = entry_offset(self.entries.len());
            self.entries.extend(resolved);
            let end = entry_offset(self.entries.len());
            let id = self.file_id(&file.uri);
            let (old_start, old_end) = self.ranges[id as usize];
            self.live_entries = self
                .live_entries
                .saturating_sub((old_end - old_start) as usize)
                .saturating_add((end - start) as usize);
            self.ranges[id as usize] = (start, end);
            self.text_hashes[id as usize] = file.text_hash;
            changed = true;
            self.compact_if_wasteful();
        }
        // Only a run that actually happened may delete. An inconclusive chunk -- a worker restart,
        // say -- would otherwise erase a whole chunk's good diagnostics with no re-sweep to
        // restore them.
        if !conclusive {
            return WorkspaceMergeOutcome {
                accepted: true,
                changed,
            };
        }
        for uri in attempted {
            if !produced.contains(uri) {
                changed |= self.forget(uri);
            }
        }
        self.compact_if_wasteful();
        WorkspaceMergeOutcome {
            accepted: true,
            changed,
        }
    }

    /// Remove a file's retained data. Dropping the range makes its entries unreachable immediately;
    /// batched compaction reclaims them once stale slices become material, avoiding a memmove for
    /// every individual delete.
    pub(crate) fn forget(&mut self, uri: &str) -> bool {
        if let Some(id) = self.files.remove(uri) {
            let (start, end) = self.ranges[id as usize];
            self.live_entries = self.live_entries.saturating_sub((end - start) as usize);
            self.ranges[id as usize] = (0, 0);
            self.text_hashes[id as usize] = 0;
            self.free_file_ids.push(id);
            true
        } else {
            false
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
        let id = self.free_file_ids.pop().unwrap_or_else(|| {
            let id = u32::try_from(self.ranges.len())
                .expect("u32 file ids address more workspace files than memory can hold");
            self.ranges.push((0, 0));
            self.text_hashes.push(0);
            id
        });
        self.files.insert(uri.into(), id);
        id
    }

    fn intern(&mut self, message: &str) -> u32 {
        if let Some(&id) = self.message_ids.get(message) {
            return id;
        }
        let id = message_id(self.messages.len());
        self.messages.push(message.to_string());
        self.message_ids.insert(message.into(), id);
        id
    }

    /// Reclaim entries and messages made unreachable by file replacement or deletion.
    ///
    /// Updates append so readers always see one contiguous slice per file; compacting once enough
    /// superseded entries accumulate preserves that layout while reclaiming them.
    fn compact_if_wasteful(&mut self) {
        let stale_entries = self.entries.len().saturating_sub(self.live_entries);
        if stale_entries < COMPACTION_STALE_ENTRIES {
            return;
        }

        let mut active_ids: Vec<u32> = self.files.values().copied().collect();
        active_ids.sort_unstable();
        active_ids.dedup();
        let mut entries = Vec::with_capacity(self.live_entries);
        let mut messages = Vec::<String>::new();
        let mut message_ids = HashMap::<Box<str>, u32>::new();
        for id in active_ids {
            let (old_start, old_end) = self.ranges[id as usize];
            let start = entry_offset(entries.len());
            for entry in &self.entries[old_start as usize..old_end as usize] {
                let old_message = entry[4] & DIAGNOSTIC_MESSAGE_MASK;
                let message = &self.messages[old_message as usize];
                let message_id = if let Some(&message_id) = message_ids.get(message.as_str()) {
                    message_id
                } else {
                    let message_id = message_id(messages.len());
                    messages.push(message.clone());
                    message_ids.insert(message.as_str().into(), message_id);
                    message_id
                };
                entries.push([
                    entry[0],
                    entry[1],
                    entry[2],
                    entry[3],
                    (entry[4] & !DIAGNOSTIC_MESSAGE_MASK) | message_id,
                ]);
            }
            let end = entry_offset(entries.len());
            self.ranges[id as usize] = (start, end);
        }
        self.entries = entries;
        self.live_entries = self.entries.len();
        self.messages = messages;
        self.message_ids = message_ids;
    }
}

/// An entry offset. `u32` addresses more entries than memory can hold (each is 20 bytes).
fn entry_offset(len: usize) -> u32 {
    u32::try_from(len).expect("u32 entry offsets address more diagnostics than memory can hold")
}

/// A message id, packed beside the severity and kind bits. Its 30 bits address more distinct
/// messages than memory can hold.
fn message_id(len: usize) -> u32 {
    u32::try_from(len)
        .ok()
        .filter(|&id| id <= DIAGNOSTIC_MESSAGE_MASK)
        .expect("30-bit message ids address more distinct diagnostics than memory can hold")
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::diag::{Diagnostic, DiagnosticKind, Severity, Span};

    fn indexed(uri: &str, hash: u64) -> IndexedFile {
        IndexedFile {
            uri: uri.to_string(),
            diagnostics: vec![Diagnostic {
                span: Span::new(0, 1),
                editor_span: None,
                identity: None,
                name: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: "broken".to_string(),
                file: 0,
            }],
            text_hash: hash,
            text: "x".to_string(),
        }
    }

    fn resolve(_text: &str, pending: &[[u32; 3]]) -> Vec<WorkspaceEntry> {
        pending
            .iter()
            .map(|entry| [0, entry[0], 0, entry[1], entry[2]])
            .collect()
    }

    #[test]
    fn repeated_file_updates_compact_superseded_entries() {
        let mut store = WorkspaceDiagnosticStore::default();
        let uri = "file:///workspace/Changing.kt";
        for hash in 1..=1_100 {
            let outcome = store.merge(
                0,
                &[uri.to_string()],
                true,
                vec![indexed(uri, hash)],
                resolve,
            );
            assert!(outcome.accepted);
            assert!(outcome.changed);
        }

        assert_eq!(store.live_entries, 1);
        assert!(
            store.entries.len() < 128,
            "superseded slices must be reclaimed instead of consuming the lifetime entry ceiling"
        );
        assert_eq!(store.messages, ["Broken"]);
    }

    #[test]
    fn unchanged_text_does_not_request_another_client_refresh() {
        let mut store = WorkspaceDiagnosticStore::default();
        let uri = "file:///workspace/Stable.kt";
        let _ = store.merge(0, &[uri.to_string()], true, vec![indexed(uri, 7)], resolve);

        let outcome = store.merge(0, &[uri.to_string()], true, vec![indexed(uri, 7)], resolve);

        assert!(outcome.accepted);
        assert!(!outcome.changed);
    }

    #[test]
    fn deleted_file_slots_are_reused() {
        let mut store = WorkspaceDiagnosticStore::default();
        for index in 0..1_100 {
            let uri = format!("file:///workspace/Generated{index}.kt");
            let _ = store.merge(
                0,
                std::slice::from_ref(&uri),
                true,
                vec![indexed(&uri, 1)],
                resolve,
            );
            let _ = store.merge(0, std::slice::from_ref(&uri), true, Vec::new(), resolve);
        }

        assert!(store.files.is_empty());
        assert_eq!(
            store.ranges.len(),
            1,
            "distinct deleted paths must reuse tombstoned metadata instead of growing forever"
        );
        assert_eq!(store.free_file_ids, [0]);
    }

    fn diagnostic(message: String, at: u32) -> Diagnostic {
        Diagnostic {
            span: Span::new(at, at + 1),
            editor_span: None,
            identity: None,
            name: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: message,
            file: 0,
        }
    }

    /// Every diagnostic of a workspace is retained: past the former 512k-entry ceiling, and with
    /// more distinct message text than the former 8 MiB interning cap.
    #[test]
    fn every_workspace_diagnostic_is_retained() {
        let mut store = WorkspaceDiagnosticStore::default();
        let files = 600usize;
        let per_file = 1_000usize;
        let mut attempted = Vec::with_capacity(files);
        let mut indexed = Vec::with_capacity(files);
        for file in 0..files {
            let uri = format!("file:///workspace/File{file}.kt");
            let diagnostics = (0..per_file)
                .map(|index| {
                    // Every 40th message is distinct and long, so the interned text alone passes
                    // the former cap.
                    let message = if index % 40 == 0 {
                        format!("distinct {file}:{index} {}", "x".repeat(640))
                    } else {
                        "repeated".to_string()
                    };
                    diagnostic(message, index as u32)
                })
                .collect();
            attempted.push(uri.clone());
            indexed.push(IndexedFile {
                uri,
                diagnostics,
                text_hash: 1,
                text: String::new(),
            });
        }

        let outcome = store.merge(0, &attempted, true, indexed, resolve);

        assert!(outcome.accepted && outcome.changed);
        assert_eq!(store.live_entries, files * per_file);
        assert!(store.live_entries > 512 * 1024);
        let interned: usize = store.messages.iter().map(String::len).sum();
        assert!(interned > 8 * 1024 * 1024, "{interned} bytes of messages");
        let last = store
            .diagnostics(&format!("file:///workspace/File{}.kt", files - 1))
            .expect("the last file is retained");
        assert_eq!(last.entries.len(), per_file);
    }
}
