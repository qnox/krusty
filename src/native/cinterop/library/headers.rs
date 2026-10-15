//! Which headers are the library's own, and how its manifest names them.
//!
//! A header is named by its path below the include directory it was found in (`zlib.h`,
//! `SDL2/SDL.h`). With a `headerFilter`, the own headers are those whose name matches one of its
//! globs; without one, every header is. The `.def` file's own code is always the library's.
//!
//! The manifest's `includedHeaders` names each own header by its path below the target's sysroot
//! when it is in it, else by the SHA-256 of its contents, as Kotlin/Native does.

use std::path::Path;

use super::super::preprocessor::IncludeDir;
use super::sha256::sha256_hex;

/// The `headerFilter` globs; none means every header is own.
pub(super) struct HeaderFilter {
    globs: Vec<String>,
}

impl HeaderFilter {
    pub(super) fn new(globs: Vec<String>) -> Self {
        Self { globs }
    }

    pub(super) fn is_filtered(&self) -> bool {
        !self.globs.is_empty()
    }

    /// Whether the header found as `name` is own.
    pub(super) fn includes(&self, name: &str) -> bool {
        self.globs.is_empty() || self.globs.iter().any(|glob| glob_matches(glob, name))
    }
}

/// A `java.nio` path glob: `**` matches anything, `*` and `?` anything but a `/`.
fn glob_matches(glob: &str, text: &str) -> bool {
    let glob = glob.as_bytes();
    let text = text.as_bytes();
    fn matches(glob: &[u8], text: &[u8]) -> bool {
        match glob.first() {
            None => text.is_empty(),
            Some(b'*') if glob.get(1) == Some(&b'*') => {
                (0..=text.len()).any(|skip| matches(&glob[2..], &text[skip..]))
            }
            Some(b'*') => (0..=text.len())
                .take_while(|&skip| skip == 0 || text[skip - 1] != b'/')
                .any(|skip| matches(&glob[1..], &text[skip..])),
            Some(b'?') => text
                .first()
                .is_some_and(|&c| c != b'/' && matches(&glob[1..], &text[1..])),
            Some(&c) => text.first() == Some(&c) && matches(&glob[1..], &text[1..]),
        }
    }
    matches(glob, text)
}

/// The name a header at `path` was found as, below the first include directory holding it.
pub(super) fn header_name(dirs: &[IncludeDir], path: &str) -> Option<String> {
    for (index, dir) in dirs.iter().enumerate() {
        match dir {
            IncludeDir::Disk(root) => {
                if let Ok(relative) = Path::new(path).strip_prefix(root) {
                    return Some(relative.to_string_lossy().into_owned());
                }
            }
            IncludeDir::Builtin(_) => {
                if let Some(name) = path.strip_prefix(&format!("<builtin:{index}>/")) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// The manifest's id of the header at `path`.
pub(super) fn header_id(dirs: &[IncludeDir], sysroot: Option<&Path>, path: &str) -> String {
    if let Some(relative) = sysroot.and_then(|root| Path::new(path).strip_prefix(root).ok()) {
        return relative.to_string_lossy().into_owned();
    }
    for (index, dir) in dirs.iter().enumerate() {
        if let IncludeDir::Builtin(files) = dir {
            if let Some(name) = path.strip_prefix(&format!("<builtin:{index}>/")) {
                if let Some((_, source)) = files.iter().find(|(file, _)| *file == name) {
                    return sha256_hex(source.as_bytes());
                }
            }
        }
    }
    sha256_hex(&std::fs::read(path).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_star_stays_in_one_directory() {
        let filter = HeaderFilter::new(vec!["zlib.h".into(), "SDL2/*.h".into(), "x/**".into()]);
        assert!(filter.includes("zlib.h"));
        assert!(!filter.includes("zconf.h"));
        assert!(filter.includes("SDL2/SDL.h"));
        assert!(!filter.includes("SDL2/sub/SDL.h"));
        assert!(filter.includes("x/a/b/c.h"));
        assert!(HeaderFilter::new(Vec::new()).includes("anything.h"));
    }
}
