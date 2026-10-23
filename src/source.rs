use std::path::Path;

mod formatting;

pub use formatting::{format_kotlin, FormattingOptions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Kotlin,
    KotlinScript,
    Java,
}

impl SourceKind {
    pub fn is_batch_compilable(self) -> bool {
        matches!(self, Self::Kotlin | Self::KotlinScript | Self::Java)
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

/// One fragment of a refined multiplatform hierarchy compiled as its platform leaf: the fragment a
/// source belongs to and the fragments whose declarations it references (its own and each one it
/// refines, directly or transitively). kotlinc analyzes every fragment as its own module, so a
/// common source never sees a declaration of the fragments that refine it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceFragment {
    member: u64,
    visible: u64,
}

impl SourceFragment {
    /// The most fragments one compilation distinguishes.
    pub const MAX: usize = u64::BITS as usize;

    /// Fragment `index` (below [`Self::MAX`]) seeing itself and the fragments `refined`.
    pub fn new(index: usize, refined: impl IntoIterator<Item = usize>) -> Self {
        let bit = |index: usize| {
            assert!(
                index < Self::MAX,
                "fragment {index} is beyond the fragment limit"
            );
            1u64 << index
        };
        Self {
            member: bit(index),
            visible: refined
                .into_iter()
                .fold(bit(index), |visible, refined| visible | bit(refined)),
        }
    }

    /// Whether a source in `from` sees a declaration of a source in `declared_in`. A source outside
    /// every fragment is seen by, and sees, every source.
    pub fn sees(from: Option<Self>, declared_in: Option<Self>) -> bool {
        match (from, declared_in) {
            (Some(from), Some(declared_in)) => from.visible & declared_in.member != 0,
            _ => true,
        }
    }
}

/// A top-level `actual` that stands for an `expect` declared in another source.
pub(crate) struct ExpectStandIn {
    pub(crate) actual: crate::fir::DeclarationId,
    /// The identity of an `actual typealias`, which is found by identity, not declaration.
    pub(crate) alias: Option<crate::types::TypeName>,
    /// The source declaring the `expect`.
    pub(crate) expect_source: u32,
}

/// Each source file's [`SourceFragment`], by source index, and the `expect` each top-level
/// `actual` stands for: a source that sees the `expect` sees the `actual` that replaces it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceFragments {
    fragments: Vec<Option<SourceFragment>>,
    expect_sources: crate::fx_hash::HashMap<crate::fir::DeclarationId, u32>,
    alias_sources: crate::fx_hash::HashMap<crate::types::TypeName, u32>,
}

impl SourceFragments {
    pub fn new(fragments: impl IntoIterator<Item = Option<SourceFragment>>) -> Self {
        Self {
            fragments: fragments.into_iter().collect(),
            expect_sources: Default::default(),
            alias_sources: Default::default(),
        }
    }

    /// Record the source of the `expect` each `actual` declaration actualizes.
    /// An `actual typealias` also carries its identity, which replaces the alias's own source.
    pub(crate) fn publish_expect_sources(
        &mut self,
        expect_sources: impl IntoIterator<Item = ExpectStandIn>,
    ) {
        if !self.fragments.iter().any(Option::is_some) {
            return;
        }
        for ExpectStandIn {
            actual,
            alias,
            expect_source,
        } in expect_sources
        {
            self.expect_sources.insert(actual, expect_source);
            if let Some(alias) = alias {
                self.alias_sources.insert(alias, expect_source);
            }
        }
    }

    /// Record the source declaring each top-level `typealias` identity.
    pub(crate) fn publish_alias_sources(
        &mut self,
        alias_sources: impl IntoIterator<Item = (crate::types::TypeName, u32)>,
    ) {
        if self.fragments.iter().any(Option::is_some) {
            self.alias_sources.extend(alias_sources);
        }
    }

    /// Whether a reference in source `from` sees the `typealias` `identity`.
    pub(crate) fn sees_alias(&self, from: Option<u32>, identity: crate::types::TypeName) -> bool {
        self.sees(from, self.alias_sources.get(&identity).copied(), None)
    }

    /// Whether a reference in source `from` sees `declaration` of source `declared_in`. A
    /// reference outside every file, or a declaration outside every source, is unrestricted; an
    /// `actual` is seen where the `expect` it actualizes is.
    pub(crate) fn sees(
        &self,
        from: Option<u32>,
        declared_in: Option<u32>,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> bool {
        let declared_in = declaration
            .and_then(|declaration| self.expect_sources.get(&declaration).copied())
            .or(declared_in);
        let fragment =
            |source: Option<u32>| self.fragments.get(source? as usize).copied().flatten();
        SourceFragment::sees(fragment(from), fragment(declared_in))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    pub kind: SourceKind,
    pub text: &'a str,
    /// This file belongs to a common source set folded into a platform compilation through
    /// multiplatform `dependsOn`. Optional expectations without a target actual are visible only
    /// in such files.
    pub is_common: bool,
    /// The refined multiplatform fragment this file belongs to, when the caller compiles a fragment
    /// hierarchy as its platform leaf. A file without one sees, and is seen by, every file.
    pub fragment: Option<SourceFragment>,
    /// Source-file stem when the caller has one. Declaration identities whose language-defined
    /// generated name is file-scoped must be assigned before signature collection, so the frontend
    /// cannot recover this later from an emitted facade name.
    pub file_stem: Option<&'a str>,
}

impl<'a> SourceInput<'a> {
    pub fn new(kind: SourceKind, text: &'a str) -> Self {
        Self {
            kind,
            text,
            is_common: false,
            fragment: None,
            file_stem: None,
        }
    }

    pub fn common(mut self) -> Self {
        self.is_common = true;
        self
    }

    pub fn with_fragment(mut self, fragment: SourceFragment) -> Self {
        self.fragment = Some(fragment);
        self
    }

    pub fn with_file_stem(mut self, file_stem: &'a str) -> Self {
        self.file_stem = Some(file_stem);
        self
    }

    pub fn kotlin(text: &'a str) -> Self {
        Self::new(SourceKind::Kotlin, text)
    }

    pub fn kotlin_script(text: &'a str) -> Self {
        Self::new(SourceKind::KotlinScript, text)
    }

    pub fn java(text: &'a str) -> Self {
        Self::new(SourceKind::Java, text)
    }
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &["kt", "kts", "java"];

pub fn kind(path: &Path) -> Option<SourceKind> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("kt") => Some(SourceKind::Kotlin),
        Some("kts") => Some(SourceKind::KotlinScript),
        Some("java") => Some(SourceKind::Java),
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
        SourceKind::Kotlin | SourceKind::KotlinScript => kotlin_dependency_candidates(text),
    };
    candidates.sort();
    candidates.dedup();
    candidates
}

fn kotlin_dependency_candidates(text: &str) -> Vec<String> {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex(text, &mut diagnostics);
    // Package and import directives are read from tokens. The file AST is not needed to name the
    // dependencies a source can pull in, and scripts use the same directives as files.
    let header = scan_file_imports(text, &tokens);
    candidates_from_header(text, &tokens, &header)
}

struct ScannedImport {
    path: String,
    wildcard: bool,
    imported_name: Option<String>,
}

struct ScannedFileHeader {
    package: Option<String>,
    imports: Vec<ScannedImport>,
}

/// Package and import directives that sit outside parentheses, brackets, and braces.
///
/// An empty `package` that opens the file is recorded, matching the parser's preamble. A later
/// empty `package` is not, and a `package` or `import` nested in a body is not a file directive.
fn scan_file_imports(text: &str, tokens: &[crate::token::Token]) -> ScannedFileHeader {
    use crate::token::TokenKind;

    let mut cursor = 0;
    while tokens
        .get(cursor)
        .is_some_and(|token| token.kind == TokenKind::Newline)
    {
        cursor += 1;
    }
    let package_opens_file = tokens
        .get(cursor)
        .is_some_and(|token| token.kind == TokenKind::KwPackage);

    let mut depth = 0i32;
    let mut index = 0;
    let mut package = None;
    let mut saw_package = false;
    let mut imports = Vec::new();
    while index < tokens.len() {
        match tokens[index].kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => {
                depth += 1;
                index += 1;
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                depth -= 1;
                index += 1;
            }
            TokenKind::KwPackage if depth == 0 => {
                index += 1;
                let (segments, next) = scan_dotted_path(text, tokens, index);
                index = next;
                let name = segments.join(".");
                if !saw_package {
                    saw_package = true;
                    if package_opens_file || !name.is_empty() {
                        package = Some(name);
                    }
                } else if package.is_none() && !name.is_empty() {
                    package = Some(name);
                }
            }
            TokenKind::KwImport if depth == 0 => {
                index += 1;
                let (segments, next) = scan_dotted_path(text, tokens, index);
                index = next;
                if segments.is_empty() {
                    continue;
                }
                let mut wildcard = false;
                if tokens
                    .get(index)
                    .is_some_and(|token| token.kind == TokenKind::Star)
                {
                    wildcard = true;
                    index += 1;
                }
                let mut alias = None;
                if tokens
                    .get(index)
                    .is_some_and(|token| token.kind == TokenKind::Ident && token.text(text) == "as")
                {
                    index += 1;
                    if tokens
                        .get(index)
                        .is_some_and(|token| token.kind == TokenKind::Ident)
                    {
                        alias = Some(tokens[index].text(text).to_string());
                        index += 1;
                    }
                }
                let imported_name = if wildcard {
                    None
                } else {
                    Some(alias.unwrap_or_else(|| segments[segments.len() - 1].clone()))
                };
                imports.push(ScannedImport {
                    path: segments.join("."),
                    wildcard,
                    imported_name,
                });
            }
            _ => index += 1,
        }
    }
    ScannedFileHeader { package, imports }
}

fn scan_dotted_path(
    text: &str,
    tokens: &[crate::token::Token],
    mut index: usize,
) -> (Vec<String>, usize) {
    use crate::token::TokenKind;

    let mut segments = Vec::new();
    if tokens
        .get(index)
        .is_some_and(|token| token.kind == TokenKind::Ident)
    {
        segments.push(tokens[index].text(text).to_string());
        index += 1;
        while tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Dot)
        {
            let name_index = index + 1;
            if tokens
                .get(name_index)
                .is_some_and(|token| token.kind == TokenKind::Ident)
            {
                segments.push(tokens[name_index].text(text).to_string());
                index = name_index + 1;
            } else {
                index += 1;
                break;
            }
        }
    }
    (segments, index)
}

fn candidates_from_header(
    text: &str,
    tokens: &[crate::token::Token],
    header: &ScannedFileHeader,
) -> Vec<String> {
    use crate::token::TokenKind;

    let mut candidates = header
        .imports
        .iter()
        .map(|import| {
            let mut path = import.path.clone();
            if import.wildcard {
                path.push_str(".*");
            }
            path
        })
        .collect::<Vec<_>>();
    let wildcard_imports = header
        .imports
        .iter()
        .filter(|import| import.wildcard)
        .map(|import| import.path.as_str())
        .collect::<Vec<_>>();
    for token in tokens {
        let name = token.text(text);
        if token.kind == TokenKind::Ident && name.chars().next().is_some_and(char::is_uppercase) {
            let explicitly_imported = header
                .imports
                .iter()
                .any(|import| import.imported_name.as_deref() == Some(name));
            if !explicitly_imported {
                if let Some(package) = &header.package {
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
        if tokens[start].kind != TokenKind::Ident {
            start += 1;
            continue;
        }
        let mut end = start;
        while tokens
            .get(end + 1)
            .is_some_and(|token| token.kind == TokenKind::Dot)
            && tokens
                .get(end + 2)
                .is_some_and(|token| token.kind == TokenKind::Ident)
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
        assert!(is_supported_path(Path::new("Main.java")));
        assert!(!is_supported_path(Path::new("README.md")));
        assert!(is_batch_compilable_path(Path::new("Main.kt")));
        assert!(is_batch_compilable_path(Path::new("Main.java")));
        assert!(is_batch_compilable_path(Path::new("script.kts")));
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

    #[test]
    fn dependency_candidates_follow_file_directives_not_nested_ones() {
        let source = "\
@file:JvmName(\"Demo\")
package demo
import q.Widget as Renamed
import r.*
fun use(x: Local): Renamed = helper(java.util.List::class)
fun hidden() {
    import q.Nested
    import hidden.*
    val value: Shown = value
}
val text = \"import q.Quoted\"
";
        let candidates = dependency_candidates(SourceKind::Kotlin, source);
        assert!(candidates.iter().any(|candidate| candidate == "q.Widget"));
        assert!(candidates.iter().any(|candidate| candidate == "r.*"));
        assert!(candidates
            .iter()
            .any(|candidate| candidate == "java.util.List"));
        assert!(candidates.iter().any(|candidate| candidate == "demo.Local"));
        assert!(candidates.iter().any(|candidate| candidate == "demo.Shown"));
        assert!(
            !candidates.iter().any(|candidate| {
                candidate == "demo.Renamed"
                    || candidate == "r.Renamed"
                    || candidate == "hidden.*"
                    || candidate.contains("Quoted")
            }),
            "{candidates:?}"
        );
    }

    #[test]
    fn scanned_dependency_candidates_match_the_parser() {
        let sources = [
            "package p\n// q.Ignored\nimport q.Base\nfun use(x: Widget): Base = Base()",
            "package\nclass Foo\nfun use(x: Widget) = x",
            "@file:JvmName(\"Demo\")\npackage\npackage demo\nimport a.B\nfun use(x: T) = x",
            "package demo\nimport q.Widget as Renamed\nimport r.*\nfun use(x: Local) = java.util.List::class",
            "package p\nimport q.Top\nfun f() {\n    import q.Hidden\n    import q.*\n    val x: Shown = x\n}\n",
            "package p\nval text = \"import q.Hidden\"\nimport q.Shown\nfun use(x: Widget) = x",
            "fun render() = 1\nimport q.Later\nval value: Shown = value\n",
            "package p\nclass Outer {\n    import q.Inner\n}\nimport q.After\nfun use(x: Shown) = x\n",
            "",
        ];
        for source in sources {
            assert_eq!(
                dependency_candidates(SourceKind::Kotlin, source),
                parsed_dependency_candidates(SourceKind::Kotlin, source),
                "{source}"
            );
            assert_eq!(
                dependency_candidates(SourceKind::KotlinScript, source),
                parsed_dependency_candidates(SourceKind::KotlinScript, source),
                "{source}"
            );
        }
    }

    fn parsed_dependency_candidates(kind: SourceKind, text: &str) -> Vec<String> {
        let mut diagnostics = crate::diag::DiagSink::new();
        let tokens = crate::lexer::lex(text, &mut diagnostics);
        let file = match kind {
            SourceKind::KotlinScript => crate::parser::parse_script_with_features(
                text,
                &tokens,
                &mut diagnostics,
                &crate::features::LangFeatures::default(),
                "script",
                &crate::script_definitions::ScriptDefinition::standard(),
            ),
            _ => crate::parser::parse(text, &tokens, &mut diagnostics),
        };
        let header = ScannedFileHeader {
            package: file.package,
            imports: file
                .import_paths
                .iter()
                .map(|import| ScannedImport {
                    path: import.path(),
                    wildcard: import.wildcard,
                    imported_name: import.imported_name().map(str::to_string),
                })
                .collect(),
        };
        let mut candidates = candidates_from_header(text, &tokens, &header);
        candidates.sort();
        candidates.dedup();
        candidates
    }
}
