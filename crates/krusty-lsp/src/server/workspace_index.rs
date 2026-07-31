//! Diagnostics retained for workspace files that are not open.
//!
//! Files are `u32` ids into parallel `Vec`s and entries live in one flat buffer with per-file
//! offsets, so the store is a handful of allocations rather than one per file. Messages are
//! interned once globally: Kotlin compiler diagnostics repeat heavily across a workspace, so a
//! shared table collapses tens of thousands of duplicates.

use std::collections::HashMap;

use super::super::{
    workspace_index_uri_bytes, IndexedFile, MAX_WORKSPACE_INDEX_FILES,
    MAX_WORKSPACE_INDEX_URI_BYTES,
};
use krusty::diag::{DiagnosticKind, Severity};

/// `(start line, start UTF-16 column, end line, end UTF-16 column, packed severity + message id)`.
pub(crate) type WorkspaceEntry = [u32; 5];

use super::implementation::{
    DIAGNOSTIC_INSPECTION_BIT, DIAGNOSTIC_MESSAGE_MASK, DIAGNOSTIC_WARNING_BIT,
};

/// Ceiling on live retained entries, so a workspace whose every file fails to compile cannot
/// exhaust memory. A changed file replaces its old slice with a bounded prefix; it never keeps
/// stale diagnostics merely because the store is full.
const MAX_RETAINED_ENTRIES: usize = 512 * 1024;
/// Companion allocation ceiling on the interned message table and its lookup keys.
const MAX_RETAINED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

fn retained_message_bytes(message: &str) -> usize {
    // Every distinct message owns one payload in `messages` and another as the `message_ids` key.
    // Include both plus their fixed records; counting only `message.len()` understated the actual
    // retained allocation by roughly half before hash-table overhead.
    message
        .len()
        .saturating_mul(2)
        .saturating_add(std::mem::size_of::<String>())
        .saturating_add(std::mem::size_of::<Box<str>>())
        .saturating_add(std::mem::size_of::<u32>())
}

fn retained_file_uri_bytes(uri: &str) -> usize {
    // Reuse the producer's URI unit and add the map value. This is conservative for the store's
    // `Box<str>` key, whose fixed record is smaller than the producer's `String`.
    workspace_index_uri_bytes(uri).saturating_add(std::mem::size_of::<u32>())
}

pub(crate) struct WorkspaceDiagnostics<'a> {
    pub entries: &'a [WorkspaceEntry],
    pub messages: &'a [String],
}

pub(crate) struct WorkspaceMergeOutcome {
    pub accepted: bool,
    pub changed: bool,
    pub newly_truncated: bool,
}

#[derive(Default)]
pub(crate) struct WorkspaceDiagnosticStore {
    files: HashMap<Box<str>, u32>,
    ranges: Vec<(u32, u32)>,
    text_hashes: Vec<u64>,
    /// Tombstoned slots are reused so a long session creating and deleting distinct paths cannot
    /// grow the parallel file metadata forever while the live `files` map stays small.
    free_file_ids: Vec<u32>,
    file_uri_bytes: usize,
    entries: Vec<WorkspaceEntry>,
    messages: Vec<String>,
    message_ids: HashMap<Box<str>, u32>,
    message_bytes: usize,
    /// Entries reachable through current file ranges. `entries.len()` may be larger between
    /// compactions because replacing one file appends its new compact slice.
    live_entries: usize,
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
    ) -> WorkspaceMergeOutcome {
        if generation < self.generation {
            return WorkspaceMergeOutcome {
                accepted: false,
                changed: false,
                newly_truncated: false,
            };
        }
        let was_truncated = self.truncated;
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
            if !self.files.contains_key(file.uri.as_str()) {
                let retained_bytes = retained_file_uri_bytes(&file.uri);
                if self.files.len() >= MAX_WORKSPACE_INDEX_FILES
                    || retained_bytes
                        > MAX_WORKSPACE_INDEX_URI_BYTES.saturating_sub(self.file_uri_bytes)
                {
                    self.truncated = true;
                    continue;
                }
            }
            self.compact_if_wasteful();
            let previous_entries = self
                .files
                .get(file.uri.as_str())
                .map(|&id| {
                    let (start, end) = self.ranges[id as usize];
                    (end - start) as usize
                })
                .unwrap_or_default();
            let available_entries = MAX_RETAINED_ENTRIES
                .saturating_sub(self.live_entries.saturating_sub(previous_entries));
            let mut pending = Vec::with_capacity(file.diagnostics.len().min(available_entries));
            // Same dedup the open-document path applies: an exact repeat must not render twice in
            // the client, and must not burn the retention budget or trip truncation spuriously.
            // Interning first is safe: a duplicate's message is already interned by its first
            // occurrence, so a dropped entry never adds to the message table.
            let mut seen = std::collections::HashSet::with_capacity(pending.capacity());
            for diagnostic in file.diagnostics {
                // Same protocol-boundary casing the open-document path applies, so a message does
                // not change appearance depending on whether its file happens to be open.
                let message = super::implementation::lsp_diagnostic_message(diagnostic.msg);
                let Some(message_id) = self.try_intern(&message) else {
                    self.truncated = true;
                    break;
                };
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
                let entry = [span.lo, span.hi.max(span.lo), severity | kind | message_id];
                if !seen.insert(entry) {
                    continue;
                }
                if pending.len() >= available_entries {
                    self.truncated = true;
                    break;
                }
                pending.push(entry);
            }
            // Resolve while the text is still in hand: the store keeps no text, and re-reading the
            // file at pull time would race whatever the sweep is writing.
            let resolved = resolve(&file.text, &pending);
            let start = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
            self.entries.extend(resolved);
            let end = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
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
                newly_truncated: !was_truncated && self.truncated,
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
            newly_truncated: !was_truncated && self.truncated,
        }
    }

    /// Remove a file's retained data. Dropping the range makes its entries unreachable immediately;
    /// batched compaction reclaims them once stale slices become material, avoiding a memmove for
    /// every individual delete.
    pub(crate) fn forget(&mut self, uri: &str) -> bool {
        if let Some((stored_uri, id)) = self.files.remove_entry(uri) {
            let (start, end) = self.ranges[id as usize];
            self.live_entries = self.live_entries.saturating_sub((end - start) as usize);
            self.ranges[id as usize] = (0, 0);
            self.text_hashes[id as usize] = 0;
            self.file_uri_bytes = self
                .file_uri_bytes
                .saturating_sub(retained_file_uri_bytes(&stored_uri));
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
                .expect("the workspace file-count ceiling fits in a u32");
            self.ranges.push((0, 0));
            self.text_hashes.push(0);
            id
        });
        self.file_uri_bytes = self
            .file_uri_bytes
            .saturating_add(retained_file_uri_bytes(uri));
        self.files.insert(uri.into(), id);
        id
    }

    fn try_intern(&mut self, message: &str) -> Option<u32> {
        if let Some(&id) = self.message_ids.get(message) {
            return Some(id);
        }
        let retained_bytes = retained_message_bytes(message);
        if retained_bytes > MAX_RETAINED_MESSAGE_BYTES.saturating_sub(self.message_bytes) {
            return None;
        }
        let id = u32::try_from(self.messages.len()).ok()?;
        if id > DIAGNOSTIC_MESSAGE_MASK {
            return None;
        }
        self.message_bytes = self.message_bytes.saturating_add(retained_bytes);
        self.messages.push(message.to_string());
        self.message_ids.insert(message.into(), id);
        Some(id)
    }

    /// Reclaim entries and messages made unreachable by file replacement or deletion.
    ///
    /// Updates append so readers always see one contiguous slice per file. Compacting once stale
    /// storage is materially larger than live storage preserves that layout without letting normal
    /// edits consume the lifetime admission budget.
    fn compact_if_wasteful(&mut self) {
        let stale_entries = self.entries.len().saturating_sub(self.live_entries);
        if stale_entries == 0
            || (stale_entries < 1024 && self.message_bytes < MAX_RETAINED_MESSAGE_BYTES)
        {
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
            let start = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            for entry in &self.entries[old_start as usize..old_end as usize] {
                let old_message = entry[4] & DIAGNOSTIC_MESSAGE_MASK;
                let message = &self.messages[old_message as usize];
                let message_id = if let Some(&message_id) = message_ids.get(message.as_str()) {
                    message_id
                } else {
                    let message_id =
                        u32::try_from(messages.len()).unwrap_or(DIAGNOSTIC_MESSAGE_MASK);
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
            let end = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            self.ranges[id as usize] = (start, end);
        }
        self.message_bytes = messages
            .iter()
            .map(|message| retained_message_bytes(message))
            .sum();
        self.entries = entries;
        self.live_entries = self.entries.len();
        self.messages = messages;
        self.message_ids = message_ids;
    }
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
    fn identical_diagnostics_are_retained_once() {
        let mut store = WorkspaceDiagnosticStore::default();
        let uri = "file:///workspace/Duplicated.kt";
        let mut file = indexed(uri, 1);
        let duplicate = file.diagnostics[0].clone();
        file.diagnostics.push(duplicate);

        let outcome = store.merge(0, &[uri.to_string()], true, vec![file], resolve);

        assert!(outcome.accepted);
        assert_eq!(
            store.live_entries, 1,
            "an exact repeat must be retained once, not once per occurrence"
        );
        assert_eq!(store.messages, ["Broken"]);
        assert!(!outcome.newly_truncated);
    }

    #[test]
    fn deleted_file_slots_and_uri_bytes_are_reused() {
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
        assert_eq!(store.file_uri_bytes, 0);
        assert_eq!(
            store.ranges.len(),
            1,
            "distinct deleted paths must reuse tombstoned metadata instead of growing forever"
        );
        assert_eq!(store.free_file_ids, [0]);
    }
}
