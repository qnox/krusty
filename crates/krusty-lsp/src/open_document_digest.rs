//! Hashes of open-document text, reused while the editor version still identifies that text.
//!
//! The analysis fingerprint mixes every in-group buffer. A keystroke bumps one document's version,
//! so the other open buffers keep the hash computed for their current version and length. A URI
//! that leaves the open set drops its entry. Close and reopen of the same URI can both happen
//! between analysis jobs, with the editor reusing the version and the new text keeping the same
//! length, so the cache is also keyed by the document lifetime assigned at open. Versions are
//! installed only for the interactive analysis call; background indexing hashes the bytes it just
//! read.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

struct InstalledDocument {
    version: i64,
    lifetime: u64,
}

struct CachedDigest {
    lifetime: u64,
    version: i64,
    len: usize,
    hash: u64,
}

thread_local! {
    static VERSIONS: RefCell<HashMap<String, InstalledDocument>> = RefCell::new(HashMap::new());
    static DIGESTS: RefCell<HashMap<String, CachedDigest>> = RefCell::new(HashMap::new());
}

static NEXT_DOCUMENT_LIFETIME: AtomicU64 = AtomicU64::new(1);

/// Identity of one open from `didOpen` until `didClose`. A later open of the same URI is a new
/// lifetime even when the editor reuses the version number.
pub fn next_document_lifetime() -> u64 {
    NEXT_DOCUMENT_LIFETIME.fetch_add(1, Ordering::Relaxed)
}

thread_local! {
    static DIGEST_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HASHED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Versions for the open documents of one interactive analysis.
///
/// Dropping the guard clears the versions so a later index of the same URI cannot reuse an open
/// buffer's hash for different bytes. Cached digests stay for the same document lifetime, version,
/// and length. A new lifetime drops the previous entry even when the URI is open again.
pub struct OpenDocumentVersions {
    _private: (),
}

impl OpenDocumentVersions {
    pub fn install(documents: &[(&str, i64, u64)]) -> Self {
        let mut versions = HashMap::with_capacity(documents.len());
        for &(uri, version, lifetime) in documents {
            versions.insert(uri.to_string(), InstalledDocument { version, lifetime });
        }
        DIGESTS.with(|digests| {
            digests.borrow_mut().retain(|uri, cached| {
                versions
                    .get(uri)
                    .is_some_and(|installed| installed.lifetime == cached.lifetime)
            });
        });
        VERSIONS.with(|slot| *slot.borrow_mut() = versions);
        Self { _private: () }
    }
}

impl Drop for OpenDocumentVersions {
    fn drop(&mut self) {
        VERSIONS.with(|versions| versions.borrow_mut().clear());
    }
}

/// Hash `text`, or the hash already stored for this document lifetime, version, and length.
pub fn text_hash(uri: &str, text: &str) -> u64 {
    let installed = VERSIONS.with(|versions| {
        versions
            .borrow()
            .get(uri)
            .map(|installed| InstalledDocument {
                version: installed.version,
                lifetime: installed.lifetime,
            })
    });
    let Some(installed) = installed else {
        return digest_text(text);
    };
    let cached = DIGESTS.with(|digests| {
        digests.borrow().get(uri).and_then(|cached| {
            (cached.lifetime == installed.lifetime
                && cached.version == installed.version
                && cached.len == text.len())
            .then_some(cached.hash)
        })
    });
    if let Some(hash) = cached {
        return hash;
    }
    let hash = digest_text(text);
    DIGESTS.with(|digests| {
        digests.borrow_mut().insert(
            uri.to_string(),
            CachedDigest {
                lifetime: installed.lifetime,
                version: installed.version,
                len: text.len(),
                hash,
            },
        );
    });
    hash
}

fn digest_text(text: &str) -> u64 {
    DIGEST_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    HASHED_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(text.len())));
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[doc(hidden)]
pub fn digest_calls() -> usize {
    DIGEST_CALLS.with(|calls| calls.get())
}

#[doc(hidden)]
pub fn hashed_bytes() -> usize {
    HASHED_BYTES.with(|bytes| bytes.get())
}

#[doc(hidden)]
pub fn reset_digest_probe() {
    VERSIONS.with(|versions| versions.borrow_mut().clear());
    DIGESTS.with(|digests| digests.borrow_mut().clear());
    DIGEST_CALLS.with(|calls| calls.set(0));
    HASHED_BYTES.with(|bytes| bytes.set(0));
}

#[cfg(test)]
mod tests {
    use super::{digest_calls, hashed_bytes, reset_digest_probe, text_hash, OpenDocumentVersions};

    #[test]
    fn an_unchanged_version_reuses_the_text_hash() {
        reset_digest_probe();
        let _versions = OpenDocumentVersions::install(&[("digest-test:a", 1, 1)]);
        let first = text_hash("digest-test:a", "fun same() {}");
        let second = text_hash("digest-test:a", "fun same() {}");
        assert_eq!(first, second);
        assert_eq!(digest_calls(), 1);
        // The editor version is the identity of the text: did_change drops a non-increasing
        // version, so a repeated version is the same buffer and is not hashed again.
        assert_eq!(text_hash("digest-test:a", "fun edit() {}"), first);
        assert_eq!(digest_calls(), 1);
    }

    #[test]
    fn a_new_version_or_length_hashes_again() {
        reset_digest_probe();
        let first = {
            let _versions = OpenDocumentVersions::install(&[("digest-test:a", 1, 1)]);
            text_hash("digest-test:a", "fun v1() {}")
        };
        assert_eq!(digest_calls(), 1);
        let second = {
            let _versions = OpenDocumentVersions::install(&[("digest-test:a", 2, 1)]);
            let hash = text_hash("digest-test:a", "fun v2() {}");
            assert_eq!(text_hash("digest-test:a", "fun v2() {}"), hash);
            hash
        };
        assert_ne!(first, second);
        assert_eq!(digest_calls(), 2);

        let _versions = OpenDocumentVersions::install(&[("digest-test:a", 2, 1)]);
        let longer = text_hash("digest-test:a", "fun v2() {}!");
        assert_ne!(longer, second);
        assert_eq!(digest_calls(), 3);
    }

    #[test]
    fn a_uri_that_leaves_the_open_set_is_hashed_again() {
        reset_digest_probe();
        let hash = {
            let _versions =
                OpenDocumentVersions::install(&[("digest-test:a", 4, 4), ("digest-test:b", 1, 5)]);
            text_hash("digest-test:a", "fun a() {}")
        };
        assert_eq!(digest_calls(), 1);
        let _versions = OpenDocumentVersions::install(&[("digest-test:b", 1, 5)]);
        let again = {
            let _versions = OpenDocumentVersions::install(&[("digest-test:a", 4, 4)]);
            text_hash("digest-test:a", "fun a() {}")
        };
        assert_eq!(again, hash);
        assert_eq!(digest_calls(), 2);
    }

    #[test]
    fn hashing_without_a_version_does_not_reuse_or_fill_the_cache() {
        reset_digest_probe();
        let first = text_hash("digest-test:a", "fun a() {}");
        let second = text_hash("digest-test:a", "fun a() {}");
        assert_eq!(first, second);
        assert_eq!(digest_calls(), 2);
        let _versions = OpenDocumentVersions::install(&[("digest-test:a", 1, 1)]);
        let _ = text_hash("digest-test:a", "fun a() {}");
        assert_eq!(digest_calls(), 3);
    }

    #[test]
    fn close_reopen_with_the_same_version_and_length_hashes_the_new_lifetime() {
        reset_digest_probe();
        let first = {
            let _versions = OpenDocumentVersions::install(&[("file:///a.kt", 1, 1)]);
            text_hash("file:///a.kt", "old")
        };
        assert_eq!(digest_calls(), 1);
        // Close and reopen both happen before the next install, so the URI never looks absent.
        let _versions = OpenDocumentVersions::install(&[("file:///a.kt", 1, 2)]);
        let second = text_hash("file:///a.kt", "new");
        assert_ne!(first, second);
        assert_eq!(digest_calls(), 2);
        assert_eq!(text_hash("file:///a.kt", "new"), second);
        assert_eq!(digest_calls(), 2);
    }

    #[test]
    fn unchanged_large_buffers_hash_no_further_bytes() {
        reset_digest_probe();
        const BUFFERS: usize = 4;
        const BUFFER_BYTES: usize = 64 * 1024;
        let buffers = (0..BUFFERS)
            .map(|index| (format!("file:///{index}.kt"), "x".repeat(BUFFER_BYTES)))
            .collect::<Vec<_>>();
        let versions = buffers
            .iter()
            .enumerate()
            .map(|(index, (uri, _))| (uri.as_str(), 1, (index as u64) + 1))
            .collect::<Vec<_>>();
        let _versions = OpenDocumentVersions::install(&versions);
        for (uri, text) in &buffers {
            let _ = text_hash(uri, text);
        }
        let hashed = hashed_bytes();
        assert_eq!(digest_calls(), BUFFERS);
        assert_eq!(hashed, BUFFERS * BUFFER_BYTES);
        for (uri, text) in &buffers {
            let _ = text_hash(uri, text);
        }
        assert_eq!(digest_calls(), BUFFERS);
        assert_eq!(
            hashed_bytes(),
            hashed,
            "a second pass over unchanged open buffers hashes 0 additional bytes"
        );
    }
}
