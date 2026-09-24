//! Source signature collection.
//!
//! This phase turns a parsed source set plus its compact Pass-1 headers into the semantic
//! `SymbolTable` that overload selection and body checking read. It owns declared shape
//! validation, annotation occurrence binding, the compact-header projection of packages and
//! imports, the declaration walk itself, and the whole-module decisions that can only be made once
//! every declaration is known: supertype cycles and conflicting top-level overloads.
//!
//! It does not own body checking, overload selection, or lowering.

use crate::plugins::registry::NativePlugins;
use crate::resolve::*;

mod annotation_occurrences;
mod collection;
mod compact_source_projection;
mod declaration_facts;
mod declaration_validation;
mod declared_classifier_inventory;
mod member_signatures;
mod source_declaration_spellings;
mod supertype_cycles;
mod top_level_overload_conflicts;
mod type_universe;

pub(in crate::resolve) use annotation_occurrences::*;
pub(in crate::resolve) use collection::*;
pub(in crate::resolve) use compact_source_projection::*;
pub(in crate::resolve) use declaration_facts::*;
pub(in crate::resolve) use declaration_validation::*;
pub(in crate::resolve) use declared_classifier_inventory::*;
pub(in crate::resolve) use member_signatures::*;
pub(in crate::resolve) use source_declaration_spellings::*;
pub(in crate::resolve) use supertype_cycles::*;
pub(in crate::resolve) use top_level_overload_conflicts::*;
pub(in crate::resolve) use type_universe::*;

/// Stage C: collect top-level function + class signatures across all files. Two passes so that a
/// class type can be referenced before its declaration (and across files).
/// Convenience wrapper — uses an empty classpath (no stdlib type scanning).
pub fn collect_signatures(files: &[File], diags: &mut DiagSink) -> SymbolTable {
    collect_signatures_with_cp(files, Box::new(EmptySymbolSource), diags)
}

/// Like `collect_signatures` but also seeds class names and type aliases from the target's
/// libraries (a JVM classpath, a klib), eliminating the need for any hardcoded type lists. No native
/// compiler plugin runs; see [`collect_signatures_with_cp_and_plugins`].
pub fn collect_signatures_with_cp(
    files: &[File],
    libraries: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> SymbolTable {
    collect_signatures_with_cp_and_plugins(files, libraries, NativePlugins::none(), diags)
}

/// [`collect_signatures_with_cp`] with the native compiler plugins the compilation runs: they add
/// their generated declarations (a `@Serializable` class's `serializer()`) to the collected table,
/// which keeps the selection for every later phase.
pub(crate) fn collect_signatures_with_cp_and_plugins(
    files: &[File],
    libraries: Box<dyn SemanticPlatform>,
    native_plugins: NativePlugins,
    diags: &mut DiagSink,
) -> SymbolTable {
    // Signature collection structurally infers expression-body literal types (`infer_lit_ty_p`, a
    // per-operand recursion over the body), so a deep expression recurses here BEFORE any wrapped
    // check runs — it needs the same grown stack segment (see [`crate::wide_stack`]).
    crate::wide_stack::on_wide_stack(move || {
        let host = native_plugins.host("main");
        let mut table = collect_signatures_with_cp_impl(files, libraries, &host, diags, None, None);
        table.native_plugins = native_plugins;
        table
    })
}

/// Test-only entry for exercising compact headers without the production local-class context.
/// Production always supplies that context through
/// [`collect_signatures_with_cp_headers_and_local_contexts`].
#[cfg(test)]
pub(crate) fn collect_signatures_with_cp_headers(
    files: &[File],
    headers: &crate::fir::StreamedHeaderModule,
    libraries: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> SymbolTable {
    crate::wide_stack::on_wide_stack(move || {
        let no_plugins = crate::plugins::PluginHost::new();
        collect_signatures_with_cp_impl(files, libraries, &no_plugins, diags, Some(headers), None)
    })
}

/// Production compact-header entry with body-derived lexical classifier context extracted while
/// each source was the active Pass-1 parse. The context owns no expression or statement identity.
pub(crate) fn collect_signatures_with_cp_headers_and_local_contexts(
    files: &[File],
    headers: &crate::fir::StreamedHeaderModule,
    local_contexts: &[PassOneLocalClassContext],
    libraries: Box<dyn SemanticPlatform>,
    native_plugins: NativePlugins,
    diags: &mut DiagSink,
) -> SymbolTable {
    crate::wide_stack::on_wide_stack(move || {
        let host = native_plugins.host("main");
        let mut table = collect_signatures_with_cp_impl(
            files,
            libraries,
            &host,
            diags,
            Some(headers),
            Some(local_contexts),
        );
        table.native_plugins = native_plugins;
        table
    })
}
