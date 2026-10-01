use std::collections::HashMap;

/// Open-document slots for one analysis pass.
///
/// Support registration used to scan the open list for every support file. The map is built once
/// per pass; the first occurrence of a URI wins, matching that scan.
pub(super) struct OpenDocumentSlots<'a> {
    by_uri: HashMap<&'a str, usize>,
    len: usize,
}

impl<'a> OpenDocumentSlots<'a> {
    pub(super) fn from_documents(documents: &'a [(&'a str, &'a str)]) -> Self {
        let mut by_uri = HashMap::with_capacity(documents.len());
        for (index, (uri, _)) in documents.iter().enumerate() {
            by_uri.entry(*uri).or_insert(index);
        }
        Self {
            by_uri,
            len: documents.len(),
        }
    }
}

pub(super) trait CanonicalSupport {
    fn uri(&self) -> &str;
    fn text(&self) -> &str;
}

impl CanonicalSupport for (&str, &str) {
    fn uri(&self) -> &str {
        self.0
    }

    fn text(&self) -> &str {
        self.1
    }
}

impl CanonicalSupport for krusty_lsp::SupportText<'_> {
    fn uri(&self) -> &str {
        krusty_lsp::SupportText::uri(*self)
    }

    fn text(&self) -> &str {
        krusty_lsp::SupportText::text(*self)
    }
}

pub(super) fn register_canonical_support<S: CanonicalSupport>(
    open_documents: &OpenDocumentSlots<'_>,
    group_support: &[S],
    support_documents: &mut Vec<(String, String)>,
    support_indices: &mut HashMap<String, usize>,
    mut remaining_bytes: usize,
    mut remaining_entries: usize,
    next_discarded_file: &mut u32,
) -> (Vec<(u32, u32)>, usize) {
    let mut added_bytes = 0usize;
    let mut remaps = Vec::with_capacity(group_support.len());
    for (local_index, source) in group_support.iter().enumerate() {
        let uri = source.uri();
        let source = source.text();
        let canonical = if let Some(&index) = open_documents.by_uri.get(uri) {
            index as u32
        } else if let Some(&index) = support_indices.get(uri) {
            (open_documents.len + index) as u32
        } else if remaining_entries > 0 && source.len() <= remaining_bytes {
            let index = support_documents.len();
            support_indices.insert(uri.to_string(), index);
            support_documents.push((uri.to_string(), source.to_string()));
            remaining_bytes -= source.len();
            remaining_entries -= 1;
            added_bytes += source.len();
            (open_documents.len + index) as u32
        } else {
            let discarded = *next_discarded_file;
            *next_discarded_file = next_discarded_file.saturating_sub(1);
            discarded
        };
        remaps.push(((open_documents.len + local_index) as u32, canonical));
    }
    (remaps, added_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_open_uri_keeps_the_first_slot() {
        let documents = [
            ("file:///a.kt", "fun a() {}"),
            ("file:///a.kt", "fun later() {}"),
        ];
        let open_documents = OpenDocumentSlots::from_documents(&documents);
        let support = [
            ("file:///a.kt", "fun a() {}"),
            ("file:///new.kt", "fun new() {}"),
        ];
        let mut support_documents = Vec::new();
        let mut support_indices = HashMap::new();
        let mut next_discarded = u32::MAX;

        let (remaps, added_bytes) = register_canonical_support(
            &open_documents,
            &support,
            &mut support_documents,
            &mut support_indices,
            usize::MAX,
            usize::MAX,
            &mut next_discarded,
        );

        assert_eq!(remaps, [(2, 0), (3, 2)]);
        assert_eq!(added_bytes, "fun new() {}".len());
        assert_eq!(
            support_documents,
            [(support[1].0.to_string(), support[1].1.to_string())]
        );
    }
}
