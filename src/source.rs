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

pub fn dependency_candidates(kind: SourceKind, text: &str) -> Vec<String> {
    let mut candidates = match kind {
        SourceKind::Java => java_dependency_candidates(text),
        SourceKind::Kotlin | SourceKind::KotlinScript => kotlin_dependency_candidates(kind, text),
    };
    candidates.sort();
    candidates.dedup();
    candidates
}

fn kotlin_dependency_candidates(kind: SourceKind, text: &str) -> Vec<String> {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex(text, &mut diagnostics);
    let file = match kind {
        SourceKind::KotlinScript => crate::parser::parse_script_with_features(
            text,
            &tokens,
            &mut diagnostics,
            &crate::features::LangFeatures::default(),
        ),
        _ => crate::parser::parse(text, &tokens, &mut diagnostics),
    };
    let mut candidates = file.imports.clone();
    let wildcard_imports = file
        .imports
        .iter()
        .filter_map(|import| import.strip_suffix(".*"))
        .collect::<Vec<_>>();
    for token in &tokens {
        let name = token.text(text);
        if token.kind == crate::token::TokenKind::Ident
            && name.chars().next().is_some_and(char::is_uppercase)
        {
            let explicitly_imported =
                file.imports.iter().any(|import| {
                    !import.ends_with(".*") && import.rsplit('.').next() == Some(name)
                }) || file.import_aliases.iter().any(|(alias, _)| alias == name);
            if !explicitly_imported {
                if let Some(package) = &file.package {
                    candidates.push(format!("{package}.{name}"));
                }
                for package in &wildcard_imports {
                    candidates.push(format!("{package}.{name}"));
                }
            }
        }
    }

    let mut start = 0;
    while start < tokens.len() {
        if tokens[start].kind != crate::token::TokenKind::Ident {
            start += 1;
            continue;
        }
        let mut end = start;
        while tokens
            .get(end + 1)
            .is_some_and(|token| token.kind == crate::token::TokenKind::Dot)
            && tokens
                .get(end + 2)
                .is_some_and(|token| token.kind == crate::token::TokenKind::Ident)
        {
            end += 2;
        }
        if end > start {
            candidates.push(
                tokens[start..=end]
                    .iter()
                    .map(|token| token.text(text))
                    .collect(),
            );
        }
        start = end + 1;
    }
    candidates
}

fn java_dependency_candidates(text: &str) -> Vec<String> {
    let Some(file) = crate::java_source::parse_source_file(text) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for import in &file.imports {
        if import.is_static {
            let owner = if import.wildcard {
                import.path.as_str()
            } else {
                import.path.rsplit_once('.').map_or("", |(owner, _)| owner)
            };
            candidates.push(owner.to_string());
        } else if import.wildcard {
            candidates.push(format!("{}.*", import.path));
        } else {
            candidates.push(import.path.clone());
        }
    }
    for reference in &file.references {
        let head = reference.path.split('.').next().unwrap_or_default();
        if file.imports.iter().any(|import| {
            !import.is_static
                && !import.wildcard
                && (import.path == reference.path || import.path.rsplit('.').next() == Some(head))
        }) {
            continue;
        }
        if reference.path.contains('.') {
            candidates.push(reference.path.clone());
        }
        if !file.package.is_empty() {
            candidates.push(format!(
                "{}.{}",
                file.package.replace('/', "."),
                reference.path
            ));
        }
        for import in &file.imports {
            if !import.is_static && import.wildcard {
                candidates.push(format!("{}.{}", import.path, reference.path));
            }
        }
    }
    candidates
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

    #[test]
    fn dependency_candidates_ignore_comments_and_resolve_source_context() {
        assert_eq!(
            dependency_candidates(
                SourceKind::Kotlin,
                "package p\n// q.Ignored\nimport q.Base\nfun use(x: Widget): Base = Base()",
            ),
            ["p.Widget", "q.Base"]
        );
        assert_eq!(
            dependency_candidates(
                SourceKind::Java,
                "package p; // import q.Ignored;\nimport q.Base; class Use extends Local {}",
            ),
            ["p.Local", "q.Base"]
        );
    }
}
