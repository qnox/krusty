use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Kotlin,
    KotlinScript,
    Java,
}

impl SourceKind {
    pub fn is_batch_compilable(self) -> bool {
        matches!(self, Self::Kotlin)
    }

    pub fn wire_code(self) -> u8 {
        match self {
            Self::Kotlin => 0,
            Self::KotlinScript => 1,
            Self::Java => 2,
        }
    }

    pub fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Kotlin),
            1 => Some(Self::KotlinScript),
            2 => Some(Self::Java),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    pub kind: SourceKind,
    pub text: &'a str,
}

impl<'a> SourceInput<'a> {
    pub fn new(kind: SourceKind, text: &'a str) -> Self {
        Self { kind, text }
    }

    pub fn kotlin(text: &'a str) -> Self {
        Self::new(SourceKind::Kotlin, text)
    }

    pub fn java(text: &'a str) -> Self {
        Self::new(SourceKind::Java, text)
    }
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &["kt", "kts"];

pub fn kind(path: &Path) -> Option<SourceKind> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("kt") => Some(SourceKind::Kotlin),
        Some("kts") => Some(SourceKind::KotlinScript),
        _ => None,
    }
}

pub fn is_supported_path(path: &Path) -> bool {
    kind(path).is_some()
}

pub fn is_batch_compilable_path(path: &Path) -> bool {
    kind(path).is_some_and(SourceKind::is_batch_compilable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_source_paths() {
        assert!(is_supported_path(Path::new("Main.kt")));
        assert!(is_supported_path(Path::new("build.gradle.kts")));
        assert!(!is_supported_path(Path::new("Main.java")));
        assert!(!is_supported_path(Path::new("README.md")));
        assert!(is_batch_compilable_path(Path::new("Main.kt")));
        assert!(!is_batch_compilable_path(Path::new("script.kts")));
        assert_eq!(
            SourceKind::from_wire_code(SourceKind::KotlinScript.wire_code()),
            Some(SourceKind::KotlinScript)
        );
        assert_eq!(SourceKind::from_wire_code(u8::MAX), None);
    }
}
