//! The ktlint configuration provider: `.editorconfig` resolution for the formatting
//! component.
//!
//! ktlint itself reads `.editorconfig` through ec4j; to stay byte compatible with ktlint
//! output the same properties must reach the krusty formatting engine. This module resolves
//! the editorconfig property chain for a document path: files closer to the document override
//! farther ones, `root = true` stops the upward walk, and within one file a later matching
//! section overrides an earlier one.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use super::StyleProperties;

const MAX_EDITORCONFIG_BYTES: u64 = 256 * 1024;

/// Resolves the editorconfig chain for `document_path`, or `None` when no `.editorconfig`
/// file exists along it — autodetection then falls through to the next provider. Unreadable
/// files are skipped; resolution never fails.
pub fn resolve(document_path: &Path) -> Option<StyleProperties> {
    let document_dir = document_path.parent()?;
    // Nearest file first; merged farthest-first so nearer files win.
    let mut chain: Vec<(PathBuf, ParsedFile)> = Vec::new();
    for dir in document_dir.ancestors() {
        let candidate = dir.join(".editorconfig");
        let Some(text) = read_bounded(&candidate) else {
            continue;
        };
        let parsed = parse(&text);
        let stop = parsed.root;
        chain.push((dir.to_path_buf(), parsed));
        if stop {
            break;
        }
    }
    if chain.is_empty() {
        return None;
    }
    let mut properties = BTreeMap::new();
    for (dir, parsed) in chain.iter().rev() {
        let Ok(relative) = document_path.strip_prefix(dir) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        for section in &parsed.sections {
            if glob_matches(&section.glob, &relative) {
                for (key, value) in &section.properties {
                    properties.insert(key.clone(), value.clone());
                }
            }
        }
    }
    Some(StyleProperties { properties })
}

fn read_bounded(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_EDITORCONFIG_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_EDITORCONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_EDITORCONFIG_BYTES).then_some(text)
}

struct ParsedFile {
    root: bool,
    sections: Vec<Section>,
}

struct Section {
    glob: String,
    properties: BTreeMap<String, String>,
}

fn parse(text: &str) -> ParsedFile {
    let mut root = false;
    let mut sections = Vec::new();
    let mut current: Option<Section> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some(Section {
                glob: line[1..line.len() - 1].trim().to_string(),
                properties: BTreeMap::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        if let Some(section) = current.as_mut() {
            section.properties.insert(key, value);
        } else if key == "root" {
            root = value == "true";
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }
    ParsedFile { root, sections }
}

/// Matches an editorconfig glob against a `/`-separated relative path.
/// Supports `*` (within a segment), `**` (across segments), `?`, `[abc]` / `[!abc]`,
/// and `{a,b,c}` alternation.
fn glob_matches(glob: &str, path: &str) -> bool {
    // EditorConfig matches a section without `/` against the file name at any depth.
    // A section containing `/` is relative to the directory that owns the config;
    // a leading slash only makes that anchoring explicit.
    let (glob, path) = if glob.contains('/') {
        (glob.strip_prefix('/').unwrap_or(glob), path)
    } else {
        (glob, path.rsplit('/').next().unwrap_or(path))
    };
    let glob = glob.as_bytes();
    let path = path.as_bytes();
    match_alternatives(glob, path)
}

fn match_alternatives(glob: &[u8], path: &[u8]) -> bool {
    // Split the glob on top-level `,` inside the first `{...}` group, trying each
    // alternative by expanding one group at a time.
    let mut depth = 0usize;
    let mut group_start = None;
    for (index, &byte) in glob.iter().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    group_start = Some(index);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(start) = group_start {
                        let inner = &glob[start + 1..index];
                        for alternative in split_commas(inner) {
                            let mut expanded = Vec::with_capacity(glob.len() + alternative.len());
                            expanded.extend_from_slice(&glob[..start]);
                            expanded.extend_from_slice(alternative);
                            expanded.extend_from_slice(&glob[index + 1..]);
                            if match_alternatives(&expanded, path) {
                                return true;
                            }
                        }
                        return false;
                    }
                }
            }
            _ => {}
        }
    }
    match_here(glob, path)
}

fn split_commas(inner: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, &byte) in inner.iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&inner[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    parts
}

fn match_here(glob: &[u8], path: &[u8]) -> bool {
    if glob.is_empty() {
        return path.is_empty();
    }
    match glob[0] {
        b'*' if glob.get(1) == Some(&b'*') => {
            // `**` crosses directory boundaries; also allow it to consume a `/`.
            let rest = &glob[2..];
            let rest = if rest.first() == Some(&b'/') {
                &rest[1..]
            } else {
                rest
            };
            for skip in 0..=path.len() {
                if match_alternatives(rest, &path[skip..]) {
                    return true;
                }
            }
            match_alternatives(&glob[2..], path)
        }
        b'*' => {
            for skip in 0..=path.len() {
                if path[..skip].contains(&b'/') {
                    break;
                }
                if match_alternatives(&glob[1..], &path[skip..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !path.is_empty() && path[0] != b'/' && match_alternatives(&glob[1..], &path[1..]),
        b'[' => match character_class(glob) {
            Some((matched, rest)) => {
                !path.is_empty() && matched(path[0]) && match_alternatives(rest, &path[1..])
            }
            None => {
                !path.is_empty() && path[0] == b'[' && match_alternatives(&glob[1..], &path[1..])
            }
        },
        byte => !path.is_empty() && path[0] == byte && match_alternatives(&glob[1..], &path[1..]),
    }
}

/// Parses `[abc]`, `[!abc]`, `[a-z]`; returns (predicate, rest of glob).
fn character_class(glob: &[u8]) -> Option<(impl Fn(u8) -> bool, &[u8])> {
    let mut index = 1;
    let negated = glob.get(1) == Some(&b'!');
    if negated {
        index = 2;
    }
    let mut ranges: Vec<(u8, u8)> = Vec::new();
    while let Some(&byte) = glob.get(index) {
        if byte == b']' && !ranges.is_empty() {
            let rest = &glob[index + 1..];
            return Some((
                move |candidate| {
                    let inside = ranges
                        .iter()
                        .any(|&(lo, hi)| lo <= candidate && candidate <= hi);
                    inside != negated
                },
                rest,
            ));
        }
        if glob.get(index + 1) == Some(&b'-') && glob.get(index + 2).is_some_and(|&b| b != b']') {
            ranges.push((byte, glob[index + 2]));
            index += 3;
        } else {
            ranges.push((byte, byte));
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(glob: &str, path: &str) -> bool {
        glob_matches(glob, path)
    }

    #[test]
    fn glob_star_stays_within_a_segment() {
        assert!(matches("*.kt", "Main.kt"));
        assert!(matches("*.kt", "src/main/Main.kt"));
        assert!(!matches("*.kt", "src/main/Main.java"));
        assert!(matches("/src/*.kt", "src/Main.kt"));
        assert!(!matches("/src/*.kt", "nested/src/Main.kt"));
    }

    #[test]
    fn glob_double_star_crosses_segments() {
        assert!(matches("**/Main.kt", "src/main/Main.kt"));
        assert!(matches("**/*.kt", "Main.kt"));
        assert!(matches("src/**", "src/a/b/c.kt"));
    }

    #[test]
    fn glob_alternation_and_classes() {
        assert!(matches("*.{kt,kts}", "build.gradle.kts"));
        assert!(matches("*.{kt,kts}", "Main.kt"));
        assert!(!matches("*.{kt,kts}", "Main.java"));
        assert!(matches("[Mm]ain.kt", "main.kt"));
        assert!(!matches("[!Mm]ain.kt", "Main.kt"));
        assert!(matches("?.kt", "A.kt"));
    }

    #[test]
    fn parse_sections_and_root() {
        let parsed = parse("root = true\n\n[*.{kt,kts}]\nindent_size = 2\n");
        assert!(parsed.root);
        assert_eq!(parsed.sections.len(), 1);
        assert_eq!(
            parsed.sections[0].properties.get("indent_size"),
            Some(&"2".to_string())
        );
    }
}
