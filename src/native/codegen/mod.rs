//! krusty's own code generator: checked common IR to machine code.
//!
//! This replaces the C emitter. A Kotlin file is lowered to Cranelift IR, compiled to a relocatable
//! ELF object by Cranelift, and linked by krusty's own linker (`super::linker`) against the runtime
//! that was prebuilt when krusty itself was built. A user's build touches no C toolchain. The one
//! text artifact is the module's public C header: the names a C caller can link, not a program.
//!
//! **Why Cranelift, and what it owns.** `docs/NATIVE.md`, *Decided: Cranelift, as a
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

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;

use super::target::NativeTarget;

/// The symbol every program object must define for the runtime's `_start` to call.
pub const PROGRAM_ENTRY: &str = "kt_program_entry";

/// What a `box()` program prints before its answer. A harness requires exactly one occurrence and
/// reads every byte after it, so an answer that spans lines is kept whole and an answer/program
/// output containing the marker fails rather than spoofing a verdict. The NULs keep ordinary
/// output from spelling it accidentally.
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
    target: NativeTarget,
    entry: Entry,
    verify: bool,
}

impl CraneliftBackend {
    pub fn new(target: NativeTarget) -> Self {
        Self {
            target,
            entry: Entry::Main,
            verify: false,
        }
    }

    /// Run Cranelift's verifier over every lowered function. It costs about a sixth of code
    /// generation, so a build leaves it off; tests turn it on, where a malformed function should
    /// fail at the lowering that produced it rather than as a miscompiled program.
    pub fn verified(mut self) -> Self {
        self.verify = true;
        self
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
    /// Public ABI lines, in file order and source order within a file.
    abi: Vec<super::c_abi::Record>,
    /// Export spellings already claimed by earlier files. The C spelling is intentionally
    /// human-readable and therefore lossy; a collision is refused rather than emitted twice.
    abi_symbols: std::collections::HashSet<String>,
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
        crate::backend::counted_loops::realize(&mut file.ir, COUNTED_LOOPS);
        // A local or anonymous classifier arrives with an opaque identity; this target names it
        // from its provenance as Kotlin/Native does. There is no facade class here, so a
        // classifier local to a top-level function starts in the package: `box$MyLocalObject`,
        // `box$1`. Everything below, the name a failed cast reports included, reads that name.
        file.ir.realize_local_class_names_in_packages();
        // The accessors a reference to a DEPENDENCY property is reached through, which the
        // generator's own object is built from; see `native::dependency_references`.
        let properties = super::dependency_references::realize_properties(&mut file.ir);
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
        // Which classifiers are value classes, read from the checked declarations: a file's own
        // and its dependencies' alike. See `native::value_classes`.
        let value_classes = match super::value_classes::NativeValueClasses::inventory(
            &file.ir,
            &file.classifiers,
        ) {
            Ok(inventory) => inventory,
            Err(unsupported) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: the native backend does not support {unsupported} yet"),
                );
                return Vec::new();
            }
        };
        let lowered = match lower::lower_file(
            lower::FileInput {
                ir: &file.ir,
                classifiers: &file.classifiers,
                callables: &file.callables,
                runtime_symbols,
                abi_symbols: &mut state.abi_symbols,
                dependency_properties: &properties,
                value_classes: &value_classes,
                source: file.source,
            },
            self.target,
            &stem,
            self.entry,
            self.verify,
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
        state.abi.extend(lowered.abi);
        vec![(format!("{stem}.o"), lowered.object)]
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        // The runtime is not an artifact of a user's build: it was prebuilt with krusty and the
        // linker supplies it. The header is the module's public C ABI, not a program to compile.
        let header = super::c_abi::header(module_name, &state.abi);
        vec![(format!("{module_name}.h"), header.into_bytes())]
    }
}

/// Native keeps the entry test at the top and carries unsigned counters in their own machine
/// representation. The common backend-boundary realizer therefore needs no JVM-style inline-call
/// provenance or Java loop shape.
pub(crate) const COUNTED_LOOPS: crate::backend::counted_loops::CountedLoopPolicy =
    crate::backend::counted_loops::CountedLoopPolicy {
        style: crate::backend::counted_loops::CounterLoopStyle::PreTested,
        inlining: crate::backend::counted_loops::HeaderInlining::None,
        until_steps: crate::backend::counted_loops::UntilSteps::Counted,
    };
