//! The frontend/backend boundary of one checked file: freeze what its backend may read.
//!
//! A backend receives the file's common IR, never the frontend symbol table. Dependency callables
//! the IR selected arrive as a frozen copy of their provider's normalized facts, so a backend has
//! no reason to ask the provider about one. Classifiers are not frozen yet: the classifier view
//! still answers a dependency classifier through the provider.

use crate::backend::{
    BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers, CheckedIrFile,
};
use crate::diag::{DiagSink, Span};
use crate::fir::SourceFileId;
use crate::ir::IrFile;
use crate::resolve::PassTwoSymbols;

#[cfg(test)]
mod tests;

/// The module-wide facts every file of one emission hands its backend alongside its own IR.
pub(super) struct ModuleEmission<'a> {
    pub module_name: &'a str,
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
    // Plugin-generated declarations are part of the file every target receives.
    crate::plugins::declare_enabled(&mut ir, symbols.native_plugins(), module.module_name);
    let callables = match CheckedBackendCallables::freeze(&ir, symbols.semantic_platform()) {
        Ok(callables) => callables,
        Err(error) => {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot freeze dependency declaration facts: {error:?}"),
            );
            return None;
        }
    };
    // Each class's declarations are recorded here, before any target lowers them.
    crate::metadata::class_declarations::record_all(&mut ir);
    Some(CheckedIrFile {
        ir,
        source,
        classifiers: CheckedBackendClassifiers::new(module_facts, symbols.semantic_platform()),
        callables,
        native_plugins: symbols.native_plugins(),
        module_name: module.module_name,
        stems: module.stems,
        origins: module.origins,
    })
}
