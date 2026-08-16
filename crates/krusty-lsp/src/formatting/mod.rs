//! Formatting component of the language server.
//!
//! Owns everything `textDocument/formatting` needs beyond the krusty engine itself:
//! resolving the formatting configuration for the document, combining it with the client's
//! `FormattingOptions`, and invoking the engine with the effective settings. The server
//! handler stays a thin adapter over this module so formatting behavior is testable without
//! an LSP session.
//!
//! Two configuration providers feed the engine, autodetected per document:
//!
//! - the **ktlint provider** (`editorconfig`) resolves the `.editorconfig` chain, exactly
//!   the format ktlint itself reads;
//! - the **IDEA provider** (`idea`) reads the IntelliJ project code style from the nearest
//!   `.idea/codeStyles/Project.xml`.
//!
//! An `.editorconfig` chain and `.idea` code style can coexist: the merge then follows
//! IDEA's own semantics — the project code style is the base and `.editorconfig` overrides
//! it property by property. With neither present the client options alone drive the engine.
//! Both providers normalize to [`StyleProperties`], keyed by lowercased editorconfig
//! property names.
//!
//! Byte compatibility with ktlint is verified by `tests/formatting_ktlint_diff.rs` against
//! committed before/after fixtures blessed with the official ktlint CLI
//! (`tools/bless-formatting.sh`).

pub mod editorconfig;
pub mod idea;

use std::collections::BTreeMap;
use std::path::Path;

use krusty::source::FormattingOptions;

/// Normalized formatting properties contributed by a configuration provider, keyed by
/// lowercased editorconfig property names regardless of the provider's native format.
#[derive(Clone, Debug, Default)]
pub struct StyleProperties {
    pub(crate) properties: BTreeMap<String, String>,
}

impl StyleProperties {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.properties.get(key).map(String::as_str)
    }
}

/// Formatting options supplied by the LSP client in a `textDocument/formatting` request.
/// Used only where no configuration provider pins a value, mirroring ktlint's precedence.
#[derive(Clone, Copy, Debug)]
pub struct ClientOptions {
    pub tab_size: u32,
    pub insert_spaces: bool,
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
    pub trim_final_newlines: bool,
}

impl Default for ClientOptions {
    fn default() -> Self {
        // ktlint's own defaults for the `ktlint_official` code style.
        Self {
            tab_size: 4,
            insert_spaces: true,
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            trim_final_newlines: true,
        }
    }
}

/// Effective engine options after combining the resolved provider configuration with the
/// client options. A `None` document path (e.g. an untitled buffer) skips provider
/// resolution.
pub fn effective_options(
    document_path: Option<&Path>,
    client: &ClientOptions,
) -> FormattingOptions {
    let config = resolve_properties(document_path);
    let insert_spaces = match config.get("indent_style") {
        Some("tab") => false,
        Some("space") => true,
        _ => client.insert_spaces,
    };
    let indent_size = config
        .get("indent_size")
        .and_then(|value| value.parse::<u32>().ok());
    let tab_width = config
        .get("tab_width")
        .and_then(|value| value.parse::<u32>().ok());
    let tab_size = if insert_spaces {
        indent_size.unwrap_or(client.tab_size)
    } else {
        tab_width.or(indent_size).unwrap_or(client.tab_size)
    };
    let trim_trailing_whitespace = match config.get("trim_trailing_whitespace") {
        Some("true") => true,
        Some("false") => false,
        _ => client.trim_trailing_whitespace,
    };
    let insert_final_newline = match config.get("insert_final_newline") {
        Some("true") => true,
        Some("false") => false,
        _ => client.insert_final_newline,
    };
    FormattingOptions {
        tab_size,
        insert_spaces,
        trim_trailing_whitespace,
        insert_final_newline,
        trim_final_newlines: client.trim_final_newlines,
    }
}

/// Autodetection over the configuration providers. With no project configuration the client
/// options alone drive the engine. With only one provider its properties apply directly.
/// When both exist the document lives in an IDEA project, so IDEA's own merge semantics
/// apply: the project code style is the base and `.editorconfig` overrides it property by
/// property (anything `.editorconfig` leaves undefined keeps the code style value). ktlint
/// itself never reads `.idea`, which is why the byte-parity fixture corpus exercises the
/// ktlint provider in isolation.
fn resolve_properties(document_path: Option<&Path>) -> StyleProperties {
    let Some(path) = document_path else {
        return StyleProperties::default();
    };
    match (editorconfig::resolve(path), idea::resolve(path)) {
        (Some(mut overlay), Some(mut base)) => {
            base.properties.append(&mut overlay.properties);
            base
        }
        (Some(properties), None) => properties,
        (None, Some(properties)) => properties,
        (None, None) => StyleProperties::default(),
    }
}

/// Formats `text` as it would be served for the document at `document_path`, or `None`
/// when the bounded engine declines the input. Configuration providers along the document
/// path (see [`resolve_properties`]) take precedence over the client-supplied options; a
/// `None` path (e.g. an untitled buffer) skips provider resolution.
pub fn format_document(
    document_path: Option<&Path>,
    text: &str,
    client: &ClientOptions,
) -> Option<String> {
    krusty::source::format_kotlin(text, effective_options(document_path, client))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    const IDEA_PROJECT_XML: &str = r#"
<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <codeStyleSettings language="kotlin">
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="USE_TAB_CHARACTER" value="false" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>
"#;

    const UNFORMATTED: &str = "fun f() {\n        call()\n}\n";

    fn project(test: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("krusty-formatting-autodetect-{test}"));
        let _ = fs::remove_dir_all(&root);
        for (relative, content) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdirs");
            fs::write(path, content).expect("write");
        }
        root
    }

    fn formatted_indent(root: &Path, client: &ClientOptions) -> String {
        let formatted = format_document(Some(&root.join("Main.kt")), UNFORMATTED, client)
            .expect("engine accepts the input");
        formatted
            .lines()
            .nth(1)
            .expect("body line")
            .chars()
            .take_while(char::is_ascii_whitespace)
            .collect()
    }

    #[test]
    fn editorconfig_wins_over_idea_code_style() {
        let root = project(
            "precedence",
            &[
                (".editorconfig", "root = true\n\n[*.kt]\nindent_size = 3\n"),
                (".idea/codeStyles/Project.xml", IDEA_PROJECT_XML),
            ],
        );
        assert_eq!(formatted_indent(&root, &ClientOptions::default()), "   ");
    }

    #[test]
    fn editorconfig_overrides_idea_per_property_and_inherits_the_rest() {
        // IDEA merge semantics: `.editorconfig` overrides the project code style property
        // by property; anything it leaves undefined keeps the code style value.
        let root = project(
            "merge",
            &[
                (".editorconfig", "root = true\n\n[*.kt]\nindent_size = 5\n"),
                (".idea/codeStyles/Project.xml", IDEA_PROJECT_XML),
            ],
        );
        let properties = resolve_properties(Some(&root.join("Main.kt")));
        assert_eq!(properties.get("indent_size"), Some("5"));
        assert_eq!(properties.get("indent_style"), Some("space"));
    }

    #[test]
    fn idea_code_style_drives_formatting_without_editorconfig() {
        let root = project(
            "idea-only",
            &[(".idea/codeStyles/Project.xml", IDEA_PROJECT_XML)],
        );
        assert_eq!(formatted_indent(&root, &ClientOptions::default()), "  ");
    }

    #[test]
    fn client_options_apply_when_no_provider_has_configuration() {
        let root = project("no-config", &[("Main.kt", UNFORMATTED)]);
        let client = ClientOptions {
            tab_size: 6,
            ..ClientOptions::default()
        };
        assert_eq!(formatted_indent(&root, &client), "      ");
    }
}
