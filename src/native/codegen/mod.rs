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

use crate::compilation_target::CompilationTarget;
mod dependency_intrinsics;
mod lower;

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;

use super::target::NativeTarget;

/// Target-owned realization of one exact dependency declaration.
///
/// The symbol and its exact unread extension receiver are published together so a later module
/// can supply an inert ABI value only for a matching checked singleton receiver.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DependencyExport {
    symbol: String,
    signature: crate::backend::DependencyFunctionSignature,
    member_receiver: Option<crate::backend::DependencyMemberReceiver>,
    singleton_member_receiver: Option<crate::backend::SingletonMemberReceiver>,
    singleton_extension_receiver: Option<crate::backend::SingletonExtensionUnreadReceiver>,
}

/// One source function this target must publish for dependent source modules.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DependencyExportDefinition {
    symbol: String,
    signature: crate::backend::DependencyFunctionSignature,
    member_receiver: Option<crate::backend::DependencyMemberReceiver>,
}

/// The symbol every program object must define for the runtime's `_start` to call.
pub const PROGRAM_ENTRY: &str = "kt_program_entry";

pub use crate::backend::{Entry, BOX_RESULT_FRAME};

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
    runtime_symbols: Option<crate::fx_hash::HashSet<String>>,
    /// Public ABI lines, in file order and source order within a file.
    abi: Vec<super::c_abi::Record>,
    /// Export spellings already claimed by earlier files. The C spelling is intentionally
    /// human-readable and therefore lossy; a collision is refused rather than emitted twice.
    abi_symbols: crate::fx_hash::HashSet<String>,
    /// The ABI each external symbol was first imported with; the linker joins every object's
    /// import of it into one definition.
    imported_abis: lower::ImportedAbis,
    /// Exact declaring KLIB module plus declaration identity to its collision-free target export.
    /// Entries survive source-module boundaries for one emission session; the module component
    /// distinguishes legal same-signature internals in unrelated dependencies.
    dependency_symbols:
        crate::fx_hash::HashMap<crate::backend::KlibDependencyCallable, DependencyExport>,
    next_dependency_symbol: usize,
    next_dependency_support_object: usize,
    next_disambiguated_source_object: usize,
    used_object_stems: crate::fx_hash::HashSet<String>,
    /// The dispatch numbering of the named classes of the module being emitted, planned from its
    /// headers by the first of its files; see `backend::class_tables::module_slots`.
    slot_plan: Option<(
        String,
        crate::backend::class_tables::module_slots::ModuleSlotPlan,
    )>,
}

impl CodegenModule {
    fn source_object_stem(&mut self, source_stem: String, source_stems: &[String]) -> String {
        if self.used_object_stems.insert(source_stem.clone()) {
            return source_stem;
        }
        loop {
            let stem = format!(
                "__krusty_source_object_{}",
                self.next_disambiguated_source_object
            );
            self.next_disambiguated_source_object += 1;
            if source_stems.iter().all(|source| source != &stem)
                && self.used_object_stems.insert(stem.clone())
            {
                return stem;
            }
        }
    }

    /// Allocate a compiler-private object stem without colliding with this module's source names
    /// or any object already emitted in the session.
    fn dependency_support_stem(&mut self, source_stems: &[String]) -> String {
        loop {
            let stem = format!(
                "__krusty_dependency_support_{}",
                self.next_dependency_support_object
            );
            self.next_dependency_support_object += 1;
            if source_stems.iter().all(|source| source != &stem)
                && self.used_object_stems.insert(stem.clone())
            {
                return stem;
            }
        }
    }
}

impl Backend for CraneliftBackend {
    type State = CodegenModule;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Native
    }

    fn emits_dependency_support(&self) -> bool {
        true
    }

    fn numbers_slots(&self) -> bool {
        true
    }

    fn realizes_bodyless_dependency_support(&self) -> bool {
        true
    }

    fn realizes_dependency_intrinsic(
        &self,
        operation: crate::backend::BackendCompilerIntrinsic,
    ) -> bool {
        dependency_intrinsics::operation_plan(operation).is_some()
    }

    fn lower_ir_file(
        &self,
        mut file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let source_stem = file.stems[file.source.raw() as usize].clone();
        let object_stem = match file.kind {
            crate::backend::CheckedIrUnitKind::Source => {
                state.source_object_stem(source_stem.clone(), file.stems)
            }
            crate::backend::CheckedIrUnitKind::DependencySupport => {
                state.dependency_support_stem(file.stems)
            }
        };
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
                Ok(symbols) => {
                    // A public top-level function's C spelling shares the final image's global
                    // namespace with the prebuilt runtime. Reserve those definitions before the
                    // first file records its human-readable exports, just as target symbol
                    // allocation already does below.
                    state.abi_symbols.extend(symbols.iter().cloned());
                    state.runtime_symbols = Some(symbols);
                }
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
            Ok(inventory) => inventory.declared_in_module(
                file.classifiers
                    .module()
                    .source_value_classes()
                    .keys()
                    .copied(),
            ),
            Err(unsupported) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: the native backend does not support {unsupported} yet"),
                );
                return Vec::new();
            }
        };
        if state
            .slot_plan
            .as_ref()
            .is_none_or(|(module, _)| module != file.module_name)
        {
            let classes = file.classifiers.module().classes();
            let plan = match super::value_classes::NativeValueClasses::of_module_classes(
                classes.iter().map(|(name, _)| name),
                classes.iter().flat_map(|(_, class)| class.carried_types()),
                &file.classifiers,
            ) {
                Ok(values) => super::classes::module_plan(classes, &values),
                Err(unsupported) => {
                    diags.error(
                        crate::diag::Span::new(0, 0),
                        format!("krusty: the native backend does not support {unsupported} yet"),
                    );
                    return Vec::new();
                }
            };
            state.slot_plan = Some((file.module_name.to_owned(), plan));
        }
        let slot_plan = &state.slot_plan.as_ref().expect("planned just above").1;
        let mut dependency_exports = crate::fx_hash::HashMap::default();
        let mut exports = file.dependency_exports.callables().collect::<Vec<_>>();
        // `DependencyExportSignatures` is a lookup map; target symbol allocation must not inherit
        // its randomized iteration order or identical inputs would produce different objects.
        exports.sort_unstable_by_key(|(callable, _)| *callable);
        for (callable, identity) in exports {
            let signature = match crate::backend::DependencyFunctionSignature::from_ir(
                &file.ir,
                callable,
                file.dependency_exports.member_receiver(callable),
            ) {
                Ok(signature) => signature,
                Err(error) => {
                    diags.error(
                        crate::diag::Span::new(0, 0),
                        format!("internal error: {error}"),
                    );
                    return Vec::new();
                }
            };
            let symbol = format!("kt_klib_export_{}", state.next_dependency_symbol);
            state.next_dependency_symbol += 1;
            let export = DependencyExport {
                symbol: symbol.clone(),
                signature: signature.clone(),
                member_receiver: file.dependency_exports.member_receiver(callable),
                singleton_member_receiver: file
                    .dependency_exports
                    .singleton_member_receiver(callable),
                singleton_extension_receiver: file
                    .dependency_exports
                    .singleton_extension_unread_receiver(callable),
            };
            if state
                .dependency_symbols
                .insert(identity.clone(), export)
                .is_some()
            {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    "internal error: two source declarations export one module-qualified KLIB identity",
                );
                return Vec::new();
            }
            dependency_exports.insert(
                callable,
                DependencyExportDefinition {
                    symbol,
                    signature,
                    member_receiver: file.dependency_exports.member_receiver(callable),
                },
            );
        }
        let lowered = match lower::lower_file(
            lower::FileInput {
                ir: &file.ir,
                classifiers: &file.classifiers,
                callables: &file.callables,
                runtime_symbols,
                abi_symbols: &mut state.abi_symbols,
                imported_abis: &mut state.imported_abis,
                dependency_properties: &properties,
                value_classes: &value_classes,
                slot_plan,
                source: file.source,
                module_name: file.module_name,
                unit_kind: file.kind,
                dependency_exports: &dependency_exports,
                dependency_symbols: &state.dependency_symbols,
            },
            self.target,
            &object_stem,
            self.entry,
            self.verify,
        ) {
            Ok(lowered) => lowered,
            Err(declined) => {
                report_decline(&declined, &file, diags);
                return Vec::new();
            }
        };
        if lowered.defines_entry {
            if let Some(first) = &state.entry_file {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!(
                        "krusty: this module declares the entry point in both {first} and {source_stem}"
                    ),
                );
                return Vec::new();
            }
            state.entry_file = Some(source_stem);
        }
        state.abi.extend(lowered.abi);
        vec![(format!("{object_stem}.o"), lowered.object)]
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        // The runtime is not an artifact of a user's build: it was prebuilt with krusty and the
        // linker supplies it. The header is the module's public C ABI, not a program to compile.
        let header = super::c_abi::header(module_name, &state.abi);
        vec![(format!("{module_name}.h"), header.into_bytes())]
    }
}

/// Report a decline at the source span of the node it was declined at: in the file that node was
/// written in, which an inlined body can make another file of the module. A decline no checked node
/// claimed is a fact about the whole file and is reported at its start.
fn report_decline(declined: &lower::Unsupported, file: &CheckedIrFile<'_>, diags: &mut DiagSink) {
    let message = format!(
        "krusty: the native backend does not support {} yet",
        declined.construct()
    );
    match declined
        .origin()
        .and_then(|origin| file.origin_span(origin))
    {
        Some((source, span)) => {
            let current = diags.current_file();
            diags.set_file(source);
            diags.error(span, message);
            diags.set_file(current);
        }
        None => diags.error(crate::diag::Span::new(0, 0), message),
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

#[cfg(test)]
mod tests {
    use super::CodegenModule;

    #[test]
    fn dependency_support_object_stems_skip_source_collisions() {
        let mut module = CodegenModule::default();
        let first_module_stems = vec!["__krusty_dependency_support_0".to_string()];
        assert_eq!(
            module.source_object_stem(
                "__krusty_dependency_support_0".to_string(),
                &first_module_stems
            ),
            "__krusty_dependency_support_0"
        );

        assert_eq!(
            module.dependency_support_stem(&["Main".to_string()]),
            "__krusty_dependency_support_1"
        );
        assert_eq!(
            module.dependency_support_stem(&["Other".to_string()]),
            "__krusty_dependency_support_2"
        );
        assert_eq!(
            module.source_object_stem(
                "__krusty_dependency_support_2".to_string(),
                &["__krusty_dependency_support_2".to_string()]
            ),
            "__krusty_source_object_0"
        );
    }
}
