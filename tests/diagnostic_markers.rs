//! The diagnostic markers of Kotlin's diagnostics test data: `<!NAME!>` … `<!>` around the range
//! kotlinc reports `NAME` on, several names separated by commas (`<!A, B("argument")!>`), nested to
//! any depth. JetBrains' runner compares every diagnostic kotlinc reports with them, so a case's
//! markers are kotlinc's ledger for it: each diagnostic's name and source range.

use std::ops::Range;

/// A case's source without its markers, and the diagnostics they mark, in that text's byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkedSource {
    pub text: String,
    pub markers: Vec<Marker>,
}

/// One diagnostic a marker names, over the range of the text it encloses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub name: String,
    pub range: Range<usize>,
}

/// Strip `src`'s markers and read the diagnostics they name. An opening marker is `<!`, an
/// uppercase letter or `_`, and everything up to the next `!>` on the same line; anything else
/// starting with `<` is source text. A close `<!>` ends the innermost open marker.
pub fn parse(src: &str) -> MarkedSource {
    let bytes = src.as_bytes();
    let mut text: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut open: Vec<(usize, Vec<String>)> = Vec::new();
    let mut markers = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!>") {
            if let Some((start, names)) = open.pop() {
                markers.extend(names.into_iter().map(|name| Marker {
                    name,
                    range: start..text.len(),
                }));
            }
            i += 3;
            continue;
        }
        if bytes[i..].starts_with(b"<!")
            && bytes
                .get(i + 2)
                .is_some_and(|&byte| byte.is_ascii_uppercase() || byte == b'_')
        {
            let mut j = i + 2;
            while j + 1 < bytes.len() && bytes[j] != b'\n' && !bytes[j..].starts_with(b"!>") {
                j += 1;
            }
            if bytes[j..].starts_with(b"!>") {
                open.push((text.len(), names(&src[i + 2..j])));
                i = j + 2;
                continue;
            }
        }
        // A real `<` or any other byte (including UTF-8 continuation bytes, which are never `<`/`!`)
        // is copied verbatim.
        text.push(bytes[i]);
        i += 1;
    }
    markers.sort_by(|left, right| {
        (left.range.start, left.range.end, &left.name).cmp(&(
            right.range.start,
            right.range.end,
            &right.name,
        ))
    });
    MarkedSource {
        text: String::from_utf8(text).expect("stripping ASCII markers preserves UTF-8"),
        markers,
    }
}

/// The diagnostic names of one opening marker's content: its top-level comma-separated entries,
/// each a name optionally followed by its rendered arguments (`NAME("a, b")`) or an old frontend
/// tag (`NAME{LT}`).
fn names(content: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, c) in content.char_indices() {
        if quoted {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                entries.push(&content[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    entries.push(&content[start..]);
    entries
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.trim();
            let end = entry
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(entry.len());
            (end > 0).then(|| entry[..end].to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(name: &str, range: Range<usize>) -> Marker {
        Marker {
            name: name.to_string(),
            range,
        }
    }

    #[test]
    fn nested_markers_name_their_enclosed_ranges() {
        let parsed = parse("val x = <!A, B(\"p, q\")!>f(<!C{LT}!>y<!>)<!> < 2 // <!x!>\n<!>");
        assert_eq!(parsed.text, "val x = f(y) < 2 // <!x!>\n");
        assert_eq!(
            parsed.markers,
            vec![marker("A", 8..12), marker("B", 8..12), marker("C", 10..11)]
        );
    }

    #[test]
    fn an_empty_marker_names_an_empty_range() {
        let parsed = parse("fun f(<!SYNTAX!><!>) {}");
        assert_eq!(parsed.text, "fun f() {}");
        assert_eq!(parsed.markers, vec![marker("SYNTAX", 6..6)]);
    }

    #[test]
    fn an_unterminated_opening_marker_is_source_text() {
        let parsed = parse("a <!B\n!> c");
        assert_eq!(parsed.text, "a <!B\n!> c");
        assert_eq!(parsed.markers, Vec::new());
    }
}
