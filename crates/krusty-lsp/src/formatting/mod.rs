//! Formatting component of the language server.
//!
//! Owns everything `textDocument/formatting` needs beyond the krusty engine itself:
//! resolving the `.editorconfig` chain for the document, combining it with the client's
//! `FormattingOptions`, and invoking the engine with the effective settings. The server
//! handler stays a thin adapter over this module so formatting behavior is testable without
//! an LSP session.
//!
//! Byte compatibility with ktlint is verified by `tests/formatting_ktlint_diff.rs` against
//! committed before/after fixtures blessed with the official ktlint CLI
//! (`tools/bless-formatting.sh`).

pub mod editorconfig;

use std::path::Path;

use krusty::source::FormattingOptions;

/// Formatting options supplied by the LSP client in a `textDocument/formatting` request.
/// Used only where `.editorconfig` does not pin a value, mirroring ktlint's precedence.
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

/// Effective engine options after combining `.editorconfig` with the client options.
/// A `None` document path (e.g. an untitled buffer) skips `.editorconfig` resolution.
pub fn effective_options(
    document_path: Option<&Path>,
    client: &ClientOptions,
) -> FormattingOptions {
    let config = document_path.map(editorconfig::resolve).unwrap_or_default();
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

/// Formats `text` as it would be served for the document at `document_path`, or `None`
/// when the bounded engine declines the input. `.editorconfig` files along the document
/// path take precedence over the client-supplied options; a `None` path (e.g. an untitled
/// buffer) skips `.editorconfig` resolution.
pub fn format_document(
    document_path: Option<&Path>,
    text: &str,
    client: &ClientOptions,
) -> Option<String> {
    krusty::source::format_kotlin(text, effective_options(document_path, client))
}
