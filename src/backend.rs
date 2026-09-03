//! Backend contracts.
//!
//! A backend consumes checked frontend output and emits target artifacts.

mod module_facts;

pub use module_facts::{
    BackendClassifierFact, BackendClassifierSource, BackendFactError, BackendMemberFact,
    BackendMemberName, BackendModuleFacts, CheckedBackendClassifiers, SymbolSourceClassifiers,
    UndeterminedType,
};

use crate::diag::DiagSink;
use crate::frontend::CheckedFile;
use crate::ir::IrFile;

/// One streamed common-IR unit plus the compact module facts a backend may use for physical
/// realization. No syntax from another source unit is reachable through this view.
pub struct CheckedIrFile<'a> {
    pub ir: IrFile,
    pub source: crate::fir::SourceFileId,
    /// Frozen, classifier-only semantic facts. Callables and properties are already selected in
    /// checked IR; exposing the frontend symbol table here would permit lookup and provisional
    /// local signatures to leak across the backend boundary.
    pub classifiers: CheckedBackendClassifiers<'a>,
    pub module_name: &'a str,
    pub stems: &'a [String],
}

/// One emitted artifact: a target-relative path and its bytes (e.g. `Foo.class`, a `.wasm` module).
pub type Artifact = (String, Vec<u8>);

pub trait Backend {
    /// Cross-file state accumulated while lowering.
    type State: Default;

    /// Lower one checked file to artifacts.
    fn lower_file(
        &self,
        checked: CheckedFile<'_>,
        stem: &str,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact>;

    /// Consume one checked common-IR file produced by the streaming FIR path. No parsed source or
    /// AST-keyed semantic table crosses this boundary; target realization consumes only checked IR
    /// and compact stable module facts.
    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact>;

    /// Emit any whole-module artifacts from the accumulated `state` (e.g. `META-INF/<m>.kotlin_module`).
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}
