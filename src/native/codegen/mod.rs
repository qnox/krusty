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
use crate::libraries::SemanticPlatform;

use super::target::NativeTarget;

/// The symbol every program object must define for the runtime's `_start` to call.
pub const PROGRAM_ENTRY: &str = "kt_program_entry";

/// What a `box()` program prints before its answer. The answer is every byte after the LAST
/// occurrence, so output the program printed first cannot be mistaken for it, and an answer that
/// spans lines is read whole. The NULs keep ordinary program output from spelling it.
pub const BOX_RESULT_FRAME: &str = "\u{0}krusty box result\u{0}";

/// Which top-level function a program starts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Kotlin's `fun main()`.
    Main,
    /// A `codegen/box` conformance case: `fun box(): String`, whose result the entry prints after
    /// [`BOX_RESULT_FRAME`] — so the case's verdict (`OK`, or what went wrong) is the program's
    /// output, with no `main` written into the corpus.
    Box,
}

pub struct CraneliftBackend {
    /// The symbol provider, as a provider and not as one target's. Only *signatures* come from
    /// it, plus the realization of an identity it assigned — see `super::intrinsics` for the
    /// spellings a JVM-backed one leaks and where they are normalized.
    provider: Rc<dyn SemanticPlatform>,
    target: NativeTarget,
    entry: Entry,
}

impl CraneliftBackend {
    pub fn new(provider: Rc<dyn SemanticPlatform>, target: NativeTarget) -> Self {
        Self {
            provider,
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
    /// The prebuilt runtime's defined symbols, read by the first file that needs them.
    runtime_symbols: Option<std::collections::HashSet<String>>,
}

impl Backend for CraneliftBackend {
    type State = CodegenModule;

    fn lower_ir_file(
        &self,
        mut file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = file.stems[file.source.raw() as usize].clone();
        // A local or anonymous classifier arrives with an opaque identity; this target names it
        // from its provenance as Kotlin/Native does. There is no facade class here, so a
        // classifier local to a top-level function starts in the package: `box$MyLocalObject`,
        // `box$1`. Everything below, the name a failed cast reports included, reads that name.
        file.ir.realize_local_class_names_in_packages();
        if state.runtime_symbols.is_none() {
            match super::linker::runtime_symbols(self.target) {
                Ok(symbols) => state.runtime_symbols = Some(symbols),
                Err(error) => {
                    diags.error(
                        crate::diag::Span::new(0, 0),
                        format!(
                            "krusty: the native backend does not support a prebuilt runtime it \
                             cannot read ({error}) yet"
                        ),
                    );
                    return Vec::new();
                }
            }
        }
        let runtime_symbols = state.runtime_symbols.as_ref().expect("read just above");
        let lowered = match lower::lower_file(
            lower::FileInput {
                ir: &file.ir,
                runtime_symbols,
            },
            &self.provider,
            self.target,
            &stem,
            self.entry,
        ) {
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
