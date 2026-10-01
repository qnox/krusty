//! Digest identity for a cached project source.
//!
//! A Kotlin support file's digest covers its URI and text. A Java stub's digest covers the text
//! only. The two are different types, so a Java value cannot be handed to a Kotlin support slot
//! with the other domain's digest.

use std::hash::{Hash, Hasher};

#[cfg(test)]
thread_local! {
    static DIGEST_OPERATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn digest_operations() -> u64 {
    DIGEST_OPERATIONS.with(std::cell::Cell::get)
}

fn note_digest() {
    #[cfg(test)]
    DIGEST_OPERATIONS.with(|operations| operations.set(operations.get() + 1));
}

/// One cached Kotlin source and the URI-inclusive digest fixed when its text was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestedSource {
    uri: String,
    text: String,
    digest: u64,
}

impl DigestedSource {
    pub fn kotlin(uri: impl Into<String>, text: impl Into<String>) -> Self {
        let uri = uri.into();
        let text = text.into();
        let digest = digest_kotlin(&uri, &text);
        Self { uri, text, digest }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn digest(&self) -> u64 {
        self.digest
    }

    pub fn as_support(&self) -> SupportText<'_> {
        SupportText {
            uri: &self.uri,
            text: &self.text,
            digest: self.digest,
        }
    }
}

/// A cached Java stub. The URI is inventory; it does not enter the digest and this value has no
/// conversion into Kotlin [`SupportText`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestedJavaSource {
    uri: String,
    text: String,
    digest: u64,
}

impl DigestedJavaSource {
    pub fn at(uri: impl Into<String>, text: impl Into<String>) -> Self {
        let uri = uri.into();
        let text = text.into();
        let digest = digest_java(&text);
        Self { uri, text, digest }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn digest(&self) -> u64 {
        self.digest
    }
}

impl AsRef<str> for DigestedJavaSource {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl serde::Serialize for DigestedJavaSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

/// Borrowed Kotlin support text plus the digest that belongs to that URI and text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportText<'a> {
    uri: &'a str,
    text: &'a str,
    digest: u64,
}

impl<'a> SupportText<'a> {
    pub fn kotlin(uri: &'a str, text: &'a str) -> Self {
        Self {
            uri,
            text,
            digest: digest_kotlin(uri, text),
        }
    }

    /// Support supplied by another open module. Its text digest follows the editor lifetime and
    /// version cache; the URI is mixed in here so this remains the Kotlin digest domain.
    pub fn open_document(uri: &'a str, text: &'a str) -> Self {
        let text_digest = crate::open_document_digest::text_hash(uri, text);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        uri.hash(&mut hasher);
        text_digest.hash(&mut hasher);
        Self {
            uri,
            text,
            digest: hasher.finish(),
        }
    }

    pub fn uri(self) -> &'a str {
        self.uri
    }

    pub fn text(self) -> &'a str {
        self.text
    }

    pub fn digest(self) -> u64 {
        self.digest
    }
}

fn digest_kotlin(uri: &str, text: &str) -> u64 {
    note_digest();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

fn digest_java(text: &str) -> u64 {
    note_digest();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_same_text_at_a_different_uri_changes_the_digest() {
        let text = "fun a() = 1\n";
        let left = DigestedSource::kotlin("file:///a.kt", text);
        let right = DigestedSource::kotlin("file:///b.kt", text);
        assert_ne!(left.digest(), right.digest());
        assert_eq!(left.as_support().digest(), left.digest());
        assert_eq!(
            SupportText::kotlin(left.uri(), left.text()).digest(),
            left.digest()
        );
    }

    #[test]
    fn open_document_support_reuses_the_editor_text_digest() {
        let before = digest_operations();
        let left = SupportText::open_document("file:///a.kt", "fun same() {}\n");
        let right = SupportText::open_document("file:///b.kt", "fun same() {}\n");
        assert_ne!(left.digest(), right.digest());
        assert_eq!(digest_operations(), before);
    }

    #[test]
    fn java_same_text_at_a_different_uri_keeps_the_digest() {
        let text = "package p; class A {}\n";
        let left = DigestedJavaSource::at("file:///a/A.java", text);
        let right = DigestedJavaSource::at("file:///b/A.java", text);
        assert_eq!(left.digest(), right.digest());
        assert_eq!(left.uri(), "file:///a/A.java");
        assert_ne!(left.uri(), right.uri());
    }

    #[test]
    fn java_text_replacement_changes_the_digest() {
        let uri = "file:///p/A.java";
        let before = DigestedJavaSource::at(uri, "class A {}\n");
        let after = DigestedJavaSource::at(uri, "class B {}\n");
        assert_ne!(before.digest(), after.digest());
    }

    #[test]
    fn a_java_source_is_not_kotlin_support_for_the_same_characters() {
        let java = DigestedJavaSource::at("file:///p/A.java", "class A {}\n");
        let kotlin = DigestedSource::kotlin(java.uri(), java.text());
        assert_ne!(kotlin.as_support().digest(), java.digest());
        assert_eq!(
            SupportText::kotlin(java.uri(), java.text()).digest(),
            kotlin.digest()
        );
    }
}
