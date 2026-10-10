//! Backend contracts.
//!
//! A backend consumes checked frontend output and emits target artifacts.

pub(crate) mod counted_loops;
mod dependency_facts;
mod entry;
pub(crate) mod local_properties;
mod module_facts;

#[cfg(test)]
pub(crate) use dependency_facts::referenced_dependencies;
pub use dependency_facts::{
    BackendCallableFact, BackendCompilerIntrinsic, BackendPropertyFact, BackendSemanticCallRole,
    CheckedBackendCallables, DependencyFactError,
};

pub use entry::{Entry, BOX_RESULT_FRAME};

pub use module_facts::{
    BackendClassifierFact, BackendClassifierSource, BackendFactError, BackendModuleFacts,
    CheckedBackendClassifiers, UndeterminedType,
};

use crate::diag::DiagSink;
use crate::ir::IrFile;

/// One streamed common-IR unit plus the compact module facts a backend may use for physical
/// realization. No syntax from another source unit is reachable through this view.
pub struct CheckedIrFile<'a> {
    pub ir: IrFile,
    pub source: crate::fir::SourceFileId,
    /// Frozen classifier facts. Callables and properties are already selected in checked IR;
    /// exposing the frontend symbol table here would permit lookup and provisional local
    /// signatures to leak across the backend boundary.
    pub classifiers: CheckedBackendClassifiers<'a>,
    /// Frozen facts for exactly the dependency callables and properties this file's IR selected,
    /// copied from their provider at this boundary so emission need not ask the provider about one
    /// again.
    pub callables: CheckedBackendCallables,
    /// The native compiler plugins the frontend ran for this compilation. A backend runs exactly
    /// these, so a declaration the frontend published is realized and no unrequested one appears.
    pub native_plugins: &'a crate::plugins::registry::NativePlugins,
    pub module_name: &'a str,
    pub stems: &'a [String],
}

/// One emitted artifact: a target-relative path and its bytes (e.g. `Foo.class`, a `.wasm` module).
pub type Artifact = (String, Vec<u8>);

pub trait Backend {
    /// Cross-file state accumulated while lowering.
    type State: Default;

    /// Consume one checked common-IR file produced by the streaming FIR path. No parsed source or
    /// AST-keyed semantic table crosses this boundary; target realization consumes only checked IR
    /// and compact stable module facts.
    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact>;

    /// Report what makes the accumulated module unemittable as a whole (a program with no entry,
    /// say) before anything is finalized. An error here suppresses [`Backend::finalize`].
    fn check_module(&self, _state: &Self::State, _diags: &mut DiagSink) {}

    /// Emit any whole-module artifacts from the accumulated `state` (e.g. `META-INF/<m>.kotlin_module`).
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}
