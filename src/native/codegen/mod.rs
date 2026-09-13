//! krusty's own code generator: checked common IR to machine code.
//!
//! This replaces the C emitter. Nothing here prints source for another compiler; a Kotlin file is
//! lowered to Cranelift IR, compiled to a relocatable ELF object by Cranelift, and linked by
//! krusty's own linker (`super::linker`) against the runtime that was prebuilt when krusty itself
//! was built. A user's build touches no C toolchain.
//!
//! **Why Cranelift, and what it owns.** `docs/BUILD_AND_NATIVE_PLAN.md`, *Decided: Cranelift, as a
//! library*: Cranelift owns instruction selection and register allocation — and only those. The
//! lowering below, the calling convention at every runtime boundary, object layout, collector
//! integration and the linker are krusty's. The lowering sits behind this module's boundary so a
//! hand-written backend could replace Cranelift one architecture at a time without touching the
//! rest of the compiler.
//!
//! **Runnable increments.** This generator is grown one Kotlin program at a time: each addition
//! lands with a test that compiles, links, RUNS and compares output, and anything it has not been
//! taught declines with a diagnostic naming the construct — the same discipline the rest of the
//! native track keeps. Nothing is ever emitted on a guess.

mod lower;

use std::rc::Rc;

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;
use crate::frontend::CheckedFile;
use crate::jvm::classpath::Classpath;

use super::target::NativeTarget;

/// The symbol every program object must define for the runtime's `_start` to call.
pub const PROGRAM_ENTRY: &str = "kt_program_entry";

/// Which top-level function a program starts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Kotlin's `fun main()`.
    Main,
    /// A `codegen/box` conformance case: `fun box(): String`, whose result the entry prints — so
    /// the case's verdict (`OK`, or what went wrong) is the program's output, with no `main` written
    /// into the corpus.
    Box,
}

pub struct CraneliftBackend {
    /// The symbol provider. A JVM classpath because that is krusty's only provider today; only
    /// *signatures* come from it — see `super::intrinsics` for the two spellings it leaks and where
    /// they are normalized.
    classpath: Rc<Classpath>,
    target: NativeTarget,
    entry: Entry,
}

impl CraneliftBackend {
    pub fn new(classpath: Rc<Classpath>, target: NativeTarget) -> Self {
        Self {
            classpath,
            target,
            entry: Entry::Main,
        }
    }

    /// Start the program in `entry` instead of `main`.
    pub fn with_entry(mut self, entry: Entry) -> Self {
        self.entry = entry;
        self
    }
}

/// What the backend accumulates across the files of one module.
#[derive(Default)]
pub struct CodegenModule {
    /// The file that declared `main`, so a second one is an error rather than two entry points.
    entry_file: Option<String>,
}

impl Backend for CraneliftBackend {
    type State = CodegenModule;

    fn lower_file(
        &self,
        _checked: CheckedFile<'_>,
        _stem: &str,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        diags.error(
            crate::diag::Span::new(0, 0),
            "krusty: the native backend requires the checked common-IR pipeline".to_string(),
        );
        Vec::new()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = file.stems[file.source.raw() as usize].clone();
        let lowered =
            match lower::lower_file(&file.ir, &self.classpath, self.target, &stem, self.entry) {
                Ok(lowered) => lowered,
                Err(unsupported) => {
                    diags.error(
                        crate::diag::Span::new(0, 0),
                        format!("krusty: the native backend does not support {unsupported} yet"),
                    );
                    return Vec::new();
                }
            };
        if lowered.defines_entry {
            if let Some(first) = &state.entry_file {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!(
                        "krusty: this module declares the entry point in both {first} and {stem}"
                    ),
                );
                return Vec::new();
            }
            state.entry_file = Some(stem.clone());
        }
        vec![(format!("{stem}.o"), lowered.object)]
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        // The runtime is not an artifact of a user's build: it was prebuilt with krusty and the
        // linker supplies it. There is nothing module-level to emit.
        Vec::new()
    }
}
