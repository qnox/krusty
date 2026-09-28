//! The project library catalog (`libs.versions.toml` or `gradle/libs.versions.toml`).
//!
//! `$libs.<alias>` becomes the Maven coordinate that alias names. Fetching the coordinate is a
//! separate step; this module only reads the catalog. `[plugins]` is ignored. `[bundles]` is refused.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub(super) struct Catalog {
    present: bool,
    libraries: BTreeMap<String, String>,
}

impl Catalog {
    pub(super) fn load(root: &Path) -> Result<Self, String> {
        let at_root = root.join("libs.versions.toml");
        let in_gradle = root.join("gradle").join("libs.versions.toml");
        match (at_root.is_file(), in_gradle.is_file()) {
            (true, true) => Err(format!(
                "project catalog is declared in both {} and {}",
                at_root.display(),
                in_gradle.display()
            )),
            (false, false) => Ok(Self::default()),
            (true, false) => read_catalog(&at_root),
            (false, true) => read_catalog(&in_gradle),
        }
    }

    /// Replace a `$catalog.alias` dependency with its Maven coordinate.
    pub(super) fn resolve(&self, file: &Path, notation: &str) -> Result<String, String> {
        let Some(body) = notation.strip_prefix('$') else {
            return Err(format!(
                "{}: dependency '{notation}' is not a catalog reference",
                file.display()
            ));
        };
        let Some((name, alias)) = body.split_once('.') else {
            return Err(format!(
                "{}: dependency '{notation}' is not a catalog reference",
                file.display()
            ));
        };
        if name.is_empty() || alias.is_empty() || alias.ends_with('.') || alias.contains("..") {
            return Err(format!(
                "{}: dependency '{notation}' is not a catalog reference",
                file.display()
            ));
        }
        if name != "libs" {
            return Err(format!(
                "{}: dependency '{notation}' uses catalog '{name}'; krusty-toolchain build resolves the project catalog 'libs' only",
                file.display()
            ));
        }
        if !self.present {
            return Err(format!(
                "{}: dependency '{notation}' needs a project catalog; looked for libs.versions.toml and gradle/libs.versions.toml",
                file.display()
            ));
        }
        let alias = normalize_alias(alias);
        self.libraries.get(&alias).cloned().ok_or_else(|| {
            format!(
                "{}: dependency '{notation}' is not in the project catalog",
                file.display()
            )
        })
    }
}

fn read_catalog(path: &Path) -> Result<Catalog, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&text).map_err(|error| format!("{}: {error}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParseError {
    line: usize,
    message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "line {}: {}", self.line, self.message)
        }
    }
}

#[derive(Clone, Debug)]
enum Value {
    String(String),
    Table(Vec<(String, String)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    None,
    Versions,
    Libraries,
    Plugins,
}

struct Scan<'a> {
    text: &'a str,
    pos: usize,
    line: usize,
}

impl<'a> Scan<'a> {
    fn new(text: &'a str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        Self {
            text,
            pos: 0,
            line: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            line: self.line,
            message: message.into(),
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while let Some(ch) = self.bump() {
                if ch == '\n' {
                    break;
                }
            }
        }
    }
}

fn parse(text: &str) -> Result<Catalog, ParseError> {
    let mut scan = Scan::new(text);
    let mut section = Section::None;
    let mut versions: Vec<(usize, String, String)> = Vec::new();
    let mut libraries: Vec<(usize, String, Value)> = Vec::new();
    loop {
        scan.skip_spaces();
        match scan.peek() {
            None => break,
            Some('\n') => {
                scan.bump();
            }
            Some('#') => scan.skip_comment(),
            Some('[') => section = table_header(&mut scan)?,
            _ => assignment(&mut scan, section, &mut versions, &mut libraries)?,
        }
    }
    assemble(versions, libraries)
}

fn table_header(scan: &mut Scan<'_>) -> Result<Section, ParseError> {
    let line = scan.line;
    scan.bump();
    scan.skip_spaces();
    let name = bare_key(scan)?;
    scan.skip_spaces();
    if scan.bump() != Some(']') {
        return Err(scan.error("expected ']'"));
    }
    finish_line(scan)?;
    match name.as_str() {
        "versions" => Ok(Section::Versions),
        "libraries" => Ok(Section::Libraries),
        "plugins" => Ok(Section::Plugins),
        "bundles" => Err(ParseError {
            line,
            message: "[bundles] is not supported".to_string(),
        }),
        other => Err(ParseError {
            line,
            message: format!("unsupported catalog table '{other}'"),
        }),
    }
}

fn assignment(
    scan: &mut Scan<'_>,
    section: Section,
    versions: &mut Vec<(usize, String, String)>,
    libraries: &mut Vec<(usize, String, Value)>,
) -> Result<(), ParseError> {
    let line = scan.line;
    let key = bare_key(scan)?;
    scan.skip_spaces();
    if scan.bump() != Some('=') {
        return Err(scan.error("expected '='"));
    }
    skip_value_space(scan)?;
    let value = value(scan)?;
    finish_line(scan)?;
    match section {
        Section::None => Err(ParseError {
            line,
            message: "expected a table header".to_string(),
        }),
        Section::Plugins => Ok(()),
        Section::Versions => match value {
            Value::String(text) => {
                if text.is_empty() {
                    return Err(ParseError {
                        line,
                        message: format!("version '{key}' must not be empty"),
                    });
                }
                versions.push((line, key, text));
                Ok(())
            }
            Value::Table(_) => Err(ParseError {
                line,
                message: format!("version '{key}' must be a string"),
            }),
        },
        Section::Libraries => {
            libraries.push((line, key, value));
            Ok(())
        }
    }
}

fn value(scan: &mut Scan<'_>) -> Result<Value, ParseError> {
    match scan.peek() {
        Some('"' | '\'') => Ok(Value::String(string(scan)?)),
        Some('{') => inline_table(scan).map(Value::Table),
        _ => Err(scan.error("expected a string or an inline table")),
    }
}

fn inline_table(scan: &mut Scan<'_>) -> Result<Vec<(String, String)>, ParseError> {
    scan.bump();
    let mut entries = Vec::new();
    loop {
        skip_value_space(scan)?;
        if scan.peek() == Some('}') {
            scan.bump();
            break;
        }
        if scan.peek().is_none() {
            return Err(scan.error("unclosed inline table"));
        }
        let key = bare_key(scan)?;
        skip_value_space(scan)?;
        if scan.bump() != Some('=') {
            return Err(scan.error("expected '='"));
        }
        skip_value_space(scan)?;
        let text = match scan.peek() {
            Some('"' | '\'') => string(scan)?,
            Some('{') => {
                return Err(scan.error("catalog values must be strings"));
            }
            _ => return Err(scan.error("expected a string")),
        };
        entries.push((key, text));
        skip_value_space(scan)?;
        match scan.peek() {
            Some(',') => {
                scan.bump();
            }
            Some('}') => {
                scan.bump();
                break;
            }
            _ => return Err(scan.error("expected ',' or '}'")),
        }
    }
    Ok(entries)
}

fn string(scan: &mut Scan<'_>) -> Result<String, ParseError> {
    let quote = scan.bump().ok_or_else(|| scan.error("expected a string"))?;
    let mut text = String::new();
    if quote == '\'' {
        loop {
            match scan.bump() {
                Some('\'') => return Ok(text),
                Some('\n') | None => return Err(scan.error("unclosed string")),
                Some(ch) => text.push(ch),
            }
        }
    }
    loop {
        match scan.bump() {
            Some('"') => return Ok(text),
            Some('\\') => {
                let escaped = scan.bump().ok_or_else(|| scan.error("unclosed string"))?;
                let ch = match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => {
                        return Err(scan.error(format!("unknown string escape '\\{other}'")));
                    }
                };
                text.push(ch);
            }
            Some('\n') | None => return Err(scan.error("unclosed string")),
            Some(ch) => text.push(ch),
        }
    }
}

fn bare_key(scan: &mut Scan<'_>) -> Result<String, ParseError> {
    if matches!(scan.peek(), Some('"' | '\'')) {
        return Err(scan.error("quoted catalog keys are not supported"));
    }
    let mut key = String::new();
    loop {
        let Some(ch) = scan.peek() else {
            break;
        };
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            key.push(ch);
            scan.bump();
            continue;
        }
        if ch == '.' {
            if key.is_empty() || key.ends_with('.') {
                return Err(scan.error("empty key segment"));
            }
            key.push('.');
            scan.bump();
            continue;
        }
        break;
    }
    if key.is_empty() || key.ends_with('.') {
        return Err(scan.error("expected a key"));
    }
    Ok(key)
}

fn skip_value_space(scan: &mut Scan<'_>) -> Result<(), ParseError> {
    loop {
        scan.skip_spaces();
        match scan.peek() {
            Some('\n') => {
                scan.bump();
            }
            Some('#') => scan.skip_comment(),
            _ => return Ok(()),
        }
    }
}

fn finish_line(scan: &mut Scan<'_>) -> Result<(), ParseError> {
    scan.skip_spaces();
    if scan.peek() == Some('#') {
        scan.skip_comment();
        return Ok(());
    }
    match scan.peek() {
        None => Ok(()),
        Some('\n') => {
            scan.bump();
            Ok(())
        }
        _ => Err(scan.error("expected end of line")),
    }
}

fn assemble(
    versions: Vec<(usize, String, String)>,
    libraries: Vec<(usize, String, Value)>,
) -> Result<Catalog, ParseError> {
    let mut version_map = BTreeMap::new();
    for (line, key, value) in versions {
        if version_map.insert(key.clone(), value).is_some() {
            return Err(ParseError {
                line,
                message: format!("version '{key}' is declared more than once"),
            });
        }
    }
    let mut resolved = BTreeMap::new();
    for (line, key, value) in libraries {
        let coordinate = coordinate(line, &key, value, &version_map)?;
        let alias = normalize_alias(&key);
        if alias.is_empty() {
            return Err(ParseError {
                line,
                message: format!("library name '{key}' is empty"),
            });
        }
        if resolved.insert(alias.clone(), coordinate).is_some() {
            return Err(ParseError {
                line,
                message: format!("catalog alias '{alias}' is declared more than once"),
            });
        }
    }
    Ok(Catalog {
        present: true,
        libraries: resolved,
    })
}

fn coordinate(
    line: usize,
    key: &str,
    value: Value,
    versions: &BTreeMap<String, String>,
) -> Result<String, ParseError> {
    let fail = |message: String| ParseError { line, message };
    match value {
        Value::String(text) => {
            if !text.contains(':') || text.split(':').any(str::is_empty) {
                return Err(fail(format!("library '{key}' is not a Maven coordinate")));
            }
            Ok(text)
        }
        Value::Table(entries) => {
            let mut module = None;
            let mut group = None;
            let mut name = None;
            let mut version = None;
            let mut version_ref = None;
            for (field, text) in entries {
                match field.as_str() {
                    "module" => module = Some(text),
                    "group" => group = Some(text),
                    "name" => name = Some(text),
                    "version" => version = Some(text),
                    "version.ref" => version_ref = Some(text),
                    other => {
                        return Err(fail(format!(
                            "library '{key}' has unsupported key '{other}'"
                        )));
                    }
                }
            }
            let version = match (version, version_ref) {
                (Some(_), Some(_)) => {
                    return Err(fail(format!("library '{key}' sets a version twice")));
                }
                (Some(version), None) => Some(version),
                (None, Some(reference)) => {
                    Some(versions.get(&reference).cloned().ok_or_else(|| {
                        fail(format!(
                            "library '{key}' version.ref '{reference}' is not in [versions]"
                        ))
                    })?)
                }
                (None, None) => None,
            };
            if let Some(version) = &version {
                if version.is_empty() {
                    return Err(fail(format!("library '{key}' version must not be empty")));
                }
            }
            if let Some(module) = module {
                if group.is_some() || name.is_some() {
                    return Err(fail(format!(
                        "library '{key}' sets both module and group or name"
                    )));
                }
                let parts: Vec<&str> = module.split(':').collect();
                if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
                    return Err(fail(format!("library '{key}' is not a Maven coordinate")));
                }
                if parts.len() >= 3 && version.is_some() {
                    return Err(fail(format!("library '{key}' sets a version twice")));
                }
                if parts.len() == 2 {
                    if let Some(version) = version {
                        return Ok(format!("{module}:{version}"));
                    }
                }
                return Ok(module);
            }
            let (Some(group), Some(name)) = (group, name) else {
                return Err(fail(format!(
                    "library '{key}' needs module, or group and name"
                )));
            };
            if group.is_empty() || name.is_empty() {
                return Err(fail(format!("library '{key}' is not a Maven coordinate")));
            }
            match version {
                Some(version) => Ok(format!("{group}:{name}:{version}")),
                None => Ok(format!("{group}:{name}")),
            }
        }
    }
}

/// Gradle's catalog accessor: `-`, `_`, and `.` separate segments and the rest of the spelling stays.
fn normalize_alias(key: &str) -> String {
    let mut alias = String::new();
    let mut separated = false;
    for ch in key.chars() {
        if ch == '-' || ch == '_' || ch == '.' {
            if !alias.is_empty() && !separated {
                alias.push('.');
                separated = true;
            }
            continue;
        }
        separated = false;
        alias.push(ch);
    }
    while alias.ends_with('.') {
        alias.pop();
    }
    alias
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coord(catalog: &Catalog, notation: &str) -> String {
        catalog.resolve(Path::new("module.yaml"), notation).unwrap()
    }

    #[test]
    fn aliases_versions_and_ignored_plugins_resolve() {
        let catalog = parse(
            "\
[versions]
ktor = \"3.3.2\"
commons = '3.14.0'

[plugins]
kotlin-jvm = { id = \"org.jetbrains.kotlin.jvm\", version.ref = \"ktor\" }

[libraries]
ktor-client-auth = { module = \"io.ktor:ktor-client-auth\", version.ref = \"ktor\" }
ktor-client-contentNegotiation = { module = \"io.ktor:ktor-client-content-negotiation\", version.ref = \"ktor\" }
ktor_client_cio = {
  module = \"io.ktor:ktor-client-cio\",
  version.ref = \"ktor\",
}
commons-lang3 = \"org.apache.commons:commons-lang3:3.14.0\" # pinned
group_name = { group = \"com.example\", name = \"widget\", version = \"1.2\" }
no-version = { module = \"com.example:no-version\" }
",
        )
        .unwrap();
        assert_eq!(
            coord(&catalog, "$libs.ktor.client.auth"),
            "io.ktor:ktor-client-auth:3.3.2"
        );
        assert_eq!(
            coord(&catalog, "$libs.ktor.client.contentNegotiation"),
            "io.ktor:ktor-client-content-negotiation:3.3.2"
        );
        assert_eq!(
            coord(&catalog, "$libs.ktor.client.cio"),
            "io.ktor:ktor-client-cio:3.3.2"
        );
        assert_eq!(
            coord(&catalog, "$libs.commons.lang3"),
            "org.apache.commons:commons-lang3:3.14.0"
        );
        assert_eq!(
            coord(&catalog, "$libs.group.name"),
            "com.example:widget:1.2"
        );
        assert_eq!(
            coord(&catalog, "$libs.no.version"),
            "com.example:no-version"
        );
    }

    #[test]
    fn catalog_text_that_cannot_be_a_library_is_rejected() {
        let cases = [
            (
                "[bundles]\nkotor = [\"a\"]\n",
                "line 1: [bundles] is not supported",
            ),
            (
                "[metadata]\nname = \"x\"\n",
                "line 1: unsupported catalog table 'metadata'",
            ),
            ("ktor = \"1\"\n", "line 1: expected a table header"),
            (
                "[versions]\nkotor = { require = \"1.0\" }\n",
                "line 2: version 'kotor' must be a string",
            ),
            (
                "[versions]\nkotor = \"1\"\nkotor = \"2\"\n",
                "line 3: version 'kotor' is declared more than once",
            ),
            (
                "[libraries]\na = { module = \"g:n\", version.ref = \"missing\" }\n",
                "line 2: library 'a' version.ref 'missing' is not in [versions]",
            ),
            (
                "[libraries]\nkotor-client = \"g:n:1\"\nkotor.client = \"g:n:2\"\n",
                "line 3: catalog alias 'kotor.client' is declared more than once",
            ),
            (
                "[libraries]\na = { module = \"g:n:1\", version = \"2\" }\n",
                "line 2: library 'a' sets a version twice",
            ),
            (
                "[libraries]\na = { group = \"g\", version = \"1\" }\n",
                "line 2: library 'a' needs module, or group and name",
            ),
            (
                "[libraries]\na = not-a-string\n",
                "line 2: expected a string or an inline table",
            ),
        ];
        for (text, message) in cases {
            assert_eq!(parse(text).unwrap_err().to_string(), message, "{text}");
        }
    }

    #[test]
    fn a_reference_outside_the_project_catalog_is_rejected() {
        let catalog = parse("[libraries]\na = \"g:n:1\"\n").unwrap();
        let file = Path::new("app/module.yaml");
        assert_eq!(
            catalog.resolve(file, "$compose.ui").unwrap_err(),
            "app/module.yaml: dependency '$compose.ui' uses catalog 'compose'; krusty-toolchain build resolves the project catalog 'libs' only"
        );
        assert_eq!(
            catalog.resolve(file, "$libs").unwrap_err(),
            "app/module.yaml: dependency '$libs' is not a catalog reference"
        );
        assert_eq!(
            catalog.resolve(file, "$libs.missing").unwrap_err(),
            "app/module.yaml: dependency '$libs.missing' is not in the project catalog"
        );
        let empty = Catalog::default();
        assert_eq!(
            empty.resolve(file, "$libs.a").unwrap_err(),
            "app/module.yaml: dependency '$libs.a' needs a project catalog; looked for libs.versions.toml and gradle/libs.versions.toml"
        );
    }
}
