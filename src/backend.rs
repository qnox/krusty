//! The front-end → back-end boundary.
//!
//! The **front end** (lex → parse → resolve → type-check) is run by the common [`compile`]
//! orchestrator here; it produces, per file, a checked `File` + `TypeInfo` against the global
//! `SymbolTable`. A [`Backend`] only **lowers** that checked output to target artifacts — it never
//! type-checks. The JVM backend is in `jvm`; WASM and JS are future targets (see
//! `docs/ARCHITECTURE.md`). No non-backend module may depend on a concrete backend.

use crate::ast::File;
use crate::diag::DiagSink;
use crate::resolve::{check_file, SymbolTable, TypeInfo};

/// One emitted artifact: a target-relative path and its bytes (e.g. `Foo.class`, a `.wasm` module).
pub type Artifact = (String, Vec<u8>);

pub trait Backend {
    /// Cross-file state accumulated while lowering (e.g. the package → facade map a JVM
    /// `.kotlin_module` needs). Built up by [`Backend::lower_file`], consumed by [`Backend::finalize`].
    type State: Default;

    /// Lower one **already type-checked** file to artifacts, threading `state` for any cross-file
    /// bookkeeping. Pure lowering — no type-checking.
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

/// The common pipeline: for each parsed file, run the front-end type-check and hand the checked
/// result to the backend to lower; then finalize. Streams one file's check/lower state at a time
/// (each `TypeInfo` drops after its file is lowered). Returns all artifacts; lowering/type errors
/// are recorded in `diags` (the caller checks `diags.has_errors()`).
pub fn compile<B: Backend>(
    files: &[File],
    stems: &[String],
    syms: &mut SymbolTable,
    backend: &B,
    module_name: &str,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    let mut outputs = Vec::new();
    let mut state = B::State::default();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32); // stamp this file's index onto its check/lower diagnostics
        let info = check_file(file, syms, diags); // front-end: type-check
        if diags.has_errors() {
            continue; // collect all diagnostics before bailing; emit nothing
        }
        outputs.extend(backend.lower_file(file, &info, syms, &stems[i], &mut state, diags));
        // back-end: lower
        // `info` (per-file checked state) drops here, before the next file — streaming shape.
    }
    if !diags.has_errors() {
        outputs.extend(backend.finalize(state, module_name));
    }
    outputs
}
