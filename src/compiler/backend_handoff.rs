//! The frontend/backend boundary of one checked file: freeze what its backend may read.
//!
//! A backend receives the file's common IR, never the frontend symbol table. Dependency callables
//! the IR selected arrive as a frozen copy of their provider's normalized facts, so a backend has
//! no reason to ask the provider about one. Classifiers are not frozen yet: the classifier view
//! still answers a dependency classifier through the provider.

use crate::backend::{
    BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers, CheckedIrFile,
    CheckedIrUnitKind,
};
use crate::diag::{DiagSink, Span};
use crate::fir::SourceFileId;
use crate::ir::IrFile;
use crate::resolve::PassTwoSymbols;

mod plugin_handoff;
#[cfg(test)]
mod tests;

/// The module-wide facts every file of one emission hands its backend alongside its own IR.
pub(super) struct ModuleEmission<'a> {
    pub module_name: &'a str,
    /// The text of the file being handed off, for plugins that record source facts.
    pub text: &'a str,
    /// Each source file's output stem, by source identity.
    pub stems: &'a [String],
    /// Where each checked node came from, for a backend to place what it reports.
    pub origins: &'a crate::fir::OriginStore,
}

/// Freeze `ir`'s backend view, or report an internal error when a dependency identity the IR holds
/// has no provider answer.
pub(super) fn checked_ir_file<'a>(
    mut ir: IrFile,
    source: SourceFileId,
    module_facts: &'a BackendModuleFacts,
    symbols: &'a PassTwoSymbols,
    module: ModuleEmission<'a>,
    diags: &mut DiagSink,
) -> Option<CheckedIrFile<'a>> {
    let classifiers = CheckedBackendClassifiers::new(module_facts, symbols.semantic_platform());
    // Plugin-generated declarations are part of the file every target receives.
    crate::plugins::declare_enabled(
        &mut ir,
        symbols.native_plugins(),
        module.module_name,
        module_facts.serialization_generated_member_statuses(),
        &classifiers,
    );
    let mut callables = match CheckedBackendCallables::freeze(&ir, symbols.semantic_platform()) {
        Ok(callables) => callables,
        Err(error) => {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot freeze dependency declaration facts: {error:?}"),
            );
            return None;
        }
    };
    let file_name = module
        .stems
        .get(source.raw() as usize)
        .map(|stem| format!("{}.kt", stem.rsplit(['/', '\\']).next().unwrap_or(stem)))
        .unwrap_or_default();
    let mut runtime =
        plugin_handoff::HandoffRuntime::new(symbols.semantic_platform(), &classifiers);
    let rejections = crate::plugins::record_enabled_source_facts(
        &mut ir,
        symbols.native_plugins(),
        module.module_name,
        &crate::plugins::PluginSourceView {
            text: module.text,
            file_name: &file_name,
            source,
            checked: &plugin_handoff::HandoffSourceFacts {
                source,
                origins: module.origins,
                callables: &callables,
            },
        },
        &mut runtime,
    );
    if !rejections.is_empty() {
        for rejection in rejections {
            diags.error(rejection.span, rejection.message);
        }
        return None;
    }
    // The runtime declarations the plugins' generated code calls join the file's frozen ones.
    let provider = symbols.semantic_platform();
    if let Err(error) = callables
        .freeze_plugin_runtime_callables(runtime.into_bound(), |identity| {
            provider.external_callable(identity)
        })
    {
        diags.error(
            Span::new(0, 0),
            format!("internal error: cannot freeze plugin runtime declaration facts: {error:?}"),
        );
        return None;
    }
    // Each class's declarations are recorded here, before any target lowers them.
    crate::metadata::class_declarations::record_all(&mut ir);
    // Common code typed an `expect` classifier as its own; every target realizes the classifier
    // the `actual typealias` actualizing it names, so the file names that one from here on.
    let (expects, nested_expects) = module_facts.actualized_expects();
    ir.actualize_expects(expects, nested_expects);
    Some(CheckedIrFile {
        ir,
        kind: CheckedIrUnitKind::Source,
        source,
        classifiers,
        callables,
        dependency_exports: Default::default(),
        native_plugins: symbols.native_plugins(),
        module_name: module.module_name,
        stems: module.stems,
        origins: module.origins,
    })
}
