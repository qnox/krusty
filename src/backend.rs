//! Backend contracts.
//!
//! A backend consumes checked frontend output and emits target artifacts.

use crate::ast::File;
use crate::diag::DiagSink;
use crate::resolve::{SymbolTable, TypeInfo};

/// One emitted artifact: a target-relative path and its bytes (e.g. `Foo.class`, a `.wasm` module).
pub type Artifact = (String, Vec<u8>);

pub trait Backend {
    /// Cross-file state accumulated while lowering.
    type State: Default;

    /// Lower one checked file to artifacts.
    fn lower_file(
        &self,
        file: &File,
        info: &TypeInfo,
        syms: &SymbolTable,
        stem: &str,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact>;

    /// Emit any whole-module artifacts from the accumulated `state` (e.g. `META-INF/<m>.kotlin_module`).
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}
