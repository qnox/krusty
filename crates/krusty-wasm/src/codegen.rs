//! krusty's WebAssembly code generator: checked common IR to one WasmGC module.
//!
//! Each file is lowered into the module as it arrives; the module is encoded when the backend is
//! finalized, because wasm is linked as a whole (see the module docs in `super`). Anything the
//! generator has not been taught declines the compilation with a diagnostic naming the construct,
//! and a declined compilation emits nothing.

mod any_member_reach;
mod callable_registry;
mod classes;
mod interface_members;
mod laid_out_classes;
mod lower;
mod module_tables;
mod property_registry;

use krusty::backend::{Artifact, Backend, CheckedIrFile, Entry, BOX_RESULT_FRAME};
use krusty::diag::DiagSink;

use super::encode::{Code, Function, Module};
use super::host::Host;
use super::runtime::Runtime;
use super::target::WasmTarget;

/// What every decline begins with; the construct follows.
pub const DECLINE_PREFIX: &str = "krusty: the wasm backend does not support ";

pub struct WasmBackend {
    target: WasmTarget,
    entry: krusty::backend::ProgramEntry,
    /// An explicit `-Xwasm-enable-array-range-checks`; otherwise each file's reference release
    /// decides.
    array_range_checks: Option<bool>,
}

impl WasmBackend {
    pub fn new(target: WasmTarget) -> Self {
        Self {
            target,
            entry: krusty::backend::ProgramEntry::new(Entry::Main),
            array_range_checks: None,
        }
    }

    /// Start the program in `entry` instead of `main`.
    pub fn with_entry(mut self, entry: Entry) -> Self {
        self.entry.kind = entry;
        self
    }

    /// Emit the synthetic program entry only for this exact declared source module. Ordinary
    /// library functions, dependency exports and initialization are still emitted.
    pub fn with_entry_module(mut self, module: impl Into<String>) -> Self {
        self.entry = self.entry.in_module(module.into());
        self
    }

    /// Explicit `-Xwasm-enable-array-range-checks` semantics. This is a code-generation option;
    /// it does not change frontend array types, overload selection or dependency identities.
    pub fn with_array_range_checks(mut self, enabled: bool) -> Self {
        self.array_range_checks = Some(enabled);
        self
    }
}

/// The module being assembled, across the files of one compilation.
#[derive(Default)]
pub struct WasmModule {
    started: Option<Started>,
    /// A file was declined, so the module is incomplete and is never encoded.
    declined: bool,
    /// The function the program starts in, and the file that declares it, once one has.
    entry: Option<(u32, String)>,
    /// The interface member numbers of the source module being lowered, by its name.
    slot_plan: Option<(
        String,
        krusty::backend::class_tables::module_slots::ModuleSlotPlan,
    )>,
    /// What the files lowered so far contribute to whether a call through `kotlin.Any`'s slots
    /// reaches another file's slot with no body.
    reach: any_member_reach::ModuleReach,
    /// Each file's top-level property initializer, in file order. They all run before the entry,
    /// as the native target runs them at program start: a program's first touch of a file is
    /// through its entry or a call that follows it, so starting with every initializer is the same
    /// order the JVM's lazy facade initialization observes for a program declared in one file.
    initializers: Vec<u32>,
    /// The module whose code first used each classifier, which a later redeclaration must not
    /// replace (see `krusty::backend::classifier_redeclarations`).
    classifier_uses: krusty::backend::classifier_redeclarations::ClassifierUses,
}

struct Started {
    module: Module,
    host: Host,
    runtime: Runtime,
    callables: callable_registry::CallableRegistry,
    properties: property_registry::PropertyRegistry,
    classes: laid_out_classes::LaidOutClasses,
    tables: module_tables::ModuleTables,
}

impl Started {
    fn new(target: WasmTarget) -> Self {
        let mut module = Module::default();
        // The object model's types come first, so their indices are the ones `lower` names.
        Runtime::declare_types(&mut module);
        let mut host = Host::import(&mut module, target);
        host.declare(&mut module);
        let runtime = Runtime::start(&mut module, &host);
        Self {
            module,
            host,
            runtime,
            callables: Default::default(),
            properties: Default::default(),
            classes: Default::default(),
            tables: Default::default(),
        }
    }
}

impl Backend for WasmBackend {
    type State = WasmModule;

    fn compilation_target(&self) -> krusty::compilation_target::CompilationTarget {
        match self.target {
            WasmTarget::Js => krusty::compilation_target::CompilationTarget::WasmJs,
            WasmTarget::Wasi => krusty::compilation_target::CompilationTarget::WasmWasi,
        }
    }

    fn emits_dependency_support(&self) -> bool {
        true
    }

    fn adds_members_to_realized_classes(&self) -> bool {
        true
    }

    fn capability_decline(&self, construct: &str) -> String {
        format!("{DECLINE_PREFIX}{construct} yet")
    }

    fn common_ir_preparation(&self) -> Option<krusty::backend::CommonIrPreparation> {
        Some(krusty::backend::CommonIrPreparation {
            counted_loops: COUNTED_LOOPS,
            enums: krusty::backend::enums::Contract::KotlinWasm,
            // Every reference object is a function value (`crate::callable_references`), a
            // `KProperty` one included: its entry reads the property, and its `get` forwards to
            // that entry.
            property_reference_values: true,
        })
    }

    fn numbers_slots(&self) -> bool {
        true
    }

    /// The runtime lays out text, its builders, its exception classes and `Unit`'s one instance
    /// itself: a library body of one of their members reads a layout this target does not have,
    /// a library class extending one of the exceptions delegates to the runtime's constructor,
    /// and a library body's `Unit` value is the runtime's, as a source reference of it is.
    fn represents_dependency_classifier(
        &self,
        classifier: krusty::backend::DependencyClassifier,
    ) -> bool {
        // `Unit`'s value is the runtime's own, as a source reference of it is. Every enum
        // constant is laid out with the runtime's `kotlin.Enum` base, so its members are too.
        classifier == krusty::types::wk::string()
            || classifier == krusty::types::wk::kotlin_enum()
            || classifier == krusty::types::wk::unit()
            || lower::is_string_builder(classifier)
            || lower::runtime_lays_out(classifier)
    }

    /// The runtime's `StringBuilder` answers the `Appendable` and `CharSequence` members it
    /// overrides (`crate::builtin_members`).
    fn dispatched_dependency_classes(&self) -> Vec<krusty::backend::DependencyClassifier> {
        vec![crate::string_builders::class()]
    }

    /// Both stdlibs read an appended sequence's `length`: the WASI one copies that many indexed
    /// `Char`s, and the JS one checks `appendRange`'s bounds against it before it appends the
    /// sequence's `toString` (`crate::char_sequence_appends`).
    fn appends_char_sequences_by_index(&self) -> bool {
        true
    }

    fn realizes_dependency_intrinsic(
        &self,
        operation: krusty::backend::BackendCompilerIntrinsic,
    ) -> bool {
        lower::is_direct_operation(operation)
    }

    /// An array classifier's `storage` property is this target's own array storage.
    fn realizes_dependency_getter(
        &self,
        signature: krusty::backend::DependencyGetterSignature<'_>,
    ) -> bool {
        // An enum constant answers `kotlin.Enum`'s `name` and `ordinal` from its `$Enum` fields.
        lower::realizes_storage_getter(signature)
            || krusty::backend::enums::getter_base_member(signature).is_some()
    }

    fn emitted_constructor_initializer(
        &self,
        state: &mut Self::State,
        declaration: &krusty::backend::ConstructorInitializerDeclaration,
    ) -> bool {
        if state.declined {
            return false;
        }
        let Some(started) = state.started.as_mut() else {
            return false;
        };
        let Ok(Some((owner, _, signature))) = started
            .classes
            .published_initializer(&declaration.constructor, &started.module)
        else {
            return false;
        };
        owner == &declaration.class
            && laid_out_classes::constructor_frame_matches(
                &mut started.module,
                &declaration.parameters,
                signature,
            )
    }

    fn lower_ir_file(
        &self,
        mut file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if state.declined {
            return Vec::new();
        }
        let mut decline = |state: &mut WasmModule, construct: String| {
            state.declined = true;
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("{DECLINE_PREFIX}{construct} yet"),
            );
        };
        let selected = match file.kind {
            krusty::backend::CheckedIrUnitKind::Source => self
                .entry
                .for_module(file.module_name)
                .and_then(|entry| entry.selected(&file.ir)),
            krusty::backend::CheckedIrUnitKind::DependencySupport => None,
        };
        if selected.is_some_and(|(_, parameters)| {
            parameters == krusty::backend::MainEntryParameters::Arguments
        }) {
            decline(state, "a `main` that takes its arguments".to_string());
            return Vec::new();
        }
        if let Err(redeclared) =
            state
                .classifier_uses
                .enter_file(file.module_name, &file.ir, &file.classifiers)
        {
            decline(state, redeclared.construct());
            return Vec::new();
        }
        krusty::backend::construction_entries::publish_constructors(
            &mut file.ir,
            (file.module_name, file.classifiers.module()),
            &mut file.dependency_exports,
        );
        krusty::backend::construction_entries::publish_type_tests(
            &mut file.ir,
            file.classifiers.module(),
            &mut file.dependency_exports,
        );
        krusty::backend::construction_entries::realize(&mut file.ir, &mut file.dependency_exports);
        if let Err(error) =
            krusty::backend::property_entries::realize(&mut file.ir, &mut file.dependency_exports)
        {
            decline(state, error.to_string());
            return Vec::new();
        }
        krusty::backend::dispatch_entries::realize(&mut file.ir, &file.dependency_exports);
        let Ok(properties) = krusty::backend::local_properties::realize(&mut file.ir) else {
            decline(state, "a local delegated property".to_string());
            return Vec::new();
        };
        let started = state
            .started
            .get_or_insert_with(|| Started::new(self.target));
        if file.kind == krusty::backend::CheckedIrUnitKind::Source {
            // A support unit has no source `CallableId`s. It is emitted immediately before the
            // source file that selected it, so rotating the source registry here would discard
            // cross-file reservations from the current source module.
            started.callables.enter_source_module(file.module_name);
            started.properties.enter_source_module(file.module_name);
            started.classes.enter_source_module(
                file.module_name,
                &file.ir,
                &file.callables,
                &file.classifiers,
                &mut started.module,
            );
        } else {
            started.classes.enter_dependency_support(
                &file.ir,
                &file.callables,
                &file.classifiers,
                &mut started.module,
            );
        }
        if state
            .slot_plan
            .as_ref()
            .is_none_or(|(module, _)| module != file.module_name)
        {
            let plan = classes::module_plan(
                file.classifiers.module().classes(),
                &file.classifiers,
                &file.callables,
                &started.classes,
            );
            state.slot_plan = Some((file.module_name.to_owned(), plan));
            // The table types are built from the plan, so they are that module's too, except a
            // dependency class's, which is the whole program's.
            started.tables.enter_plan(
                (file.kind == krusty::backend::CheckedIrUnitKind::Source)
                    .then_some(file.module_name),
                file.classifiers.module().classes(),
                &file.classifiers,
                &started.classes,
            );
        }
        let plan = &state.slot_plan.as_ref().expect("planned just above").1;
        let lowered = match lower::lower_file(
            lower::FileInput {
                module_facts: file.classifiers.module(),
                ir: &file.ir,
                properties: &properties,
                callables: &file.callables,
                classifiers: &file.classifiers,
                array_range_checks: self.array_range_checks.unwrap_or_else(|| {
                    super::array_access::default_range_checks(file.ir.reference.release())
                }),
                dependency_exports: &file.dependency_exports,
                module_name: file.module_name,
                plan,
            },
            lower::ModuleState {
                module: &mut started.module,
                runtime: &mut started.runtime,
                callables: &mut started.callables,
                properties: &mut started.properties,
                classes: &mut started.classes,
                tables: &mut started.tables,
            },
        ) {
            Ok(lowered) => lowered,
            Err(construct) => {
                decline(state, construct);
                return Vec::new();
            }
        };
        if let Some(construct) = state.reach.add(lowered.reach) {
            decline(state, construct);
            return Vec::new();
        }
        state.initializers.extend(lowered.initializer);
        if let Some((function, _)) = selected {
            let entry = match lowered.functions.get(function as usize).copied() {
                Some(Some(entry)) => entry,
                Some(None) => {
                    decline(state, "an inline-only entry function".to_string());
                    return Vec::new();
                }
                None => {
                    decline(state, "an invalid entry function identity".to_string());
                    return Vec::new();
                }
            };
            let stem = file.stems[file.source.raw() as usize].clone();
            match &state.entry {
                Some((_, first)) => diags.error(
                    krusty::diag::Span::new(0, 0),
                    format!(
                        "krusty: the program declares {} in both {first} and {stem}",
                        self.entry.kind.declaration()
                    ),
                ),
                None => state.entry = Some((entry, stem)),
            }
        }
        Vec::new()
    }

    fn check_module(&self, state: &Self::State, diags: &mut DiagSink) {
        if let Some(started) = state.started.as_ref().filter(|_| !state.declined) {
            if let Some(construct) = started.runtime.objects.interfaces.declined() {
                diags.error(
                    krusty::diag::Span::new(0, 0),
                    format!("{DECLINE_PREFIX}{construct} yet"),
                );
                return;
            }
            if let Err(error) = started.callables.check_links() {
                diags.error(
                    krusty::diag::Span::new(0, 0),
                    format!("{DECLINE_PREFIX}{error} yet"),
                );
            }
        }
        if state.started.is_some() && !state.declined && state.entry.is_none() {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!(
                    "krusty: the program declares no {} to start in",
                    self.entry.kind.declaration()
                ),
            );
        }
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        let (Some(mut started), false, Some((entry, _))) =
            (state.started, state.declined, state.entry)
        else {
            return Vec::new();
        };
        started
            .runtime
            .objects
            .finalize_interfaces(&mut started.module);
        let start = started.module.declare();
        let ty = started.module.func_type(Vec::new(), Vec::new());
        let mut code = Code::default();
        for initializer in &state.initializers {
            code.call(*initializer);
        }
        match self.entry.kind {
            Entry::Main => {
                code.call(entry);
            }
            Entry::Box => {
                let frame = BOX_RESULT_FRAME.encode_utf16().collect::<Vec<_>>();
                started
                    .runtime
                    .string_constant(&mut started.module, &mut code, &frame);
                code.call(started.runtime.write_string)
                    .call(entry)
                    .call(started.runtime.string_or_null)
                    .call(started.runtime.write_string);
            }
        }
        started.module.define(
            start,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        started.module.export_function("_start", start);
        vec![
            (format!("{module_name}.wasm"), started.module.encode()),
            (
                format!("{module_name}.mjs"),
                started.host.loader(module_name).into_bytes(),
            ),
        ]
    }
}

/// A counted loop keeps its entry test at the top, as on JavaScript and Native; wasm's `loop`
/// branches back to its start, and the header calls nothing that could be inlined. An `until … step`
/// loop builds its progression, as on JavaScript, until this generator is taught the counted form.
const COUNTED_LOOPS: krusty::backend::counted_loops::CountedLoopPolicy =
    krusty::backend::counted_loops::CountedLoopPolicy {
        style: krusty::backend::counted_loops::CounterLoopStyle::PreTested,
        inlining: krusty::backend::counted_loops::HeaderInlining::None,
        until_steps: krusty::backend::counted_loops::UntilSteps::Built,
    };

#[cfg(test)]
mod tests {
    mod companion_declarations;
    mod laid_out_superclasses;
    mod reference_array_size;

    use super::{WasmBackend, WasmTarget};
    use krusty::backend::{Artifact, Backend, CommonIrPreparationError, Entry};
    use krusty::diag::DiagSink;
    use krusty::libraries::EmptySymbolSource;
    use krusty::source::SourceInput;

    /// A suspend shape common preparation cannot lower is this backend's capability decline,
    /// reported with the prefix the box lanes classify declines by, never as an internal error.
    #[test]
    fn a_common_preparation_suspend_decline_is_a_wasm_capability_decline() {
        let backend = WasmBackend::new(WasmTarget::Js);
        let mut diags = DiagSink::new();
        backend.report_common_ir_preparation_error(
            &CommonIrPreparationError::Suspend(
                krusty::backend::coroutines::ContinuationDecline::Suspends,
            ),
            &mut diags,
        );
        let messages = diags
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            ["krusty: the wasm backend does not support a suspend function that suspends yet"]
        );
    }

    /// Compile `sources`, the file stemmed `Main` first and then `Other1`, `Other2`, …, into a
    /// program starting in `entry`.
    fn compile_program(
        target: WasmTarget,
        entry: Entry,
        sources: &[&str],
    ) -> (Vec<Artifact>, Vec<String>) {
        compile_program_with_provider(target, entry, sources, Box::new(EmptySymbolSource))
    }

    /// [`compile_program`] against the exact target stdlib, which declares the runtime
    /// exceptions a checked cast throws.
    fn compile_program_with_stdlib(
        target: WasmTarget,
        entry: Entry,
        sources: &[&str],
    ) -> (Vec<Artifact>, Vec<String>) {
        use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
        let path = krusty::toolchain::kotlin_stdlib_klib_or_reason(
            krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
        )
        .unwrap_or_else(|reason| panic!("the exact target stdlib must be provisioned: {reason}"));
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            path,
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        compile_program_with_provider(target, entry, sources, Box::new(libraries))
    }

    fn compile_program_with_provider(
        target: WasmTarget,
        entry: Entry,
        sources: &[&str],
        provider: Box<dyn krusty::libraries::SemanticPlatform>,
    ) -> (Vec<Artifact>, Vec<String>) {
        let stems = (0..sources.len())
            .map(|ordinal| match ordinal {
                0 => "Main".to_string(),
                _ => format!("Other{ordinal}"),
            })
            .collect::<Vec<_>>();
        let inputs = sources
            .iter()
            .zip(&stems)
            .map(|(source, stem)| SourceInput::kotlin(source).with_file_stem(stem))
            .collect::<Vec<_>>();
        let mut diags = DiagSink::new();
        let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
            &inputs,
            krusty::frontend::PlatformProvider::new(
                krusty::compiler::Backend::compilation_target(&WasmBackend::new(target)),
                provider,
            ),
            &krusty::features::LangFeatures::new(),
            &mut diags,
        );
        let outputs = krusty::compiler::emit_analyzed(
            analysis,
            &stems,
            &WasmBackend::new(target).with_entry(entry),
            "main",
            &mut diags,
        );
        let messages = diags.diags.iter().map(|diag| diag.msg.clone()).collect();
        (outputs, messages)
    }

    fn compile(target: WasmTarget, source: &str) -> (Vec<Artifact>, Vec<String>) {
        compile_program(target, Entry::Box, &[source])
    }

    /// A class delegated property whose getter is reached from a file that also captures an
    /// anonymous object. The property reference passed to `getValue` keeps the plan's checked
    /// type, so wasm can lower the accessor instead of declining the expression.
    #[test]
    fn delegate_capture_discovery_box_compiles() {
        use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
        let source = include_str!("../../../tests/box_regressions/delegateCaptureDiscovery.kt");
        for target in WasmTarget::ALL {
            let path = krusty::toolchain::kotlin_stdlib_klib_or_reason(
                krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
            )
            .unwrap_or_else(|reason| {
                panic!("the exact target stdlib must be provisioned: {reason}")
            });
            let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
                path,
                KlibStdlibSemantics::Wasm,
            )])
            .unwrap();
            let (artifacts, diagnostics) =
                compile_program_with_provider(target, Entry::Box, &[source], Box::new(libraries));
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            let directory = std::env::temp_dir().join(format!(
                "delegate-capture-{}-{}",
                target.name(),
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            for (name, bytes) in &artifacts {
                std::fs::write(directory.join(name), bytes).unwrap();
            }
            let loader = artifacts
                .iter()
                .find(|(name, _)| name.ends_with(".mjs"))
                .expect("wasm loader")
                .0
                .as_str();
            let output = std::process::Command::new("node")
                .arg("--no-warnings")
                .arg(directory.join(loader))
                .output()
                .expect("node runs the wasm box");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{target:?} exit {:?}\n{stdout}{stderr}",
                output.status.code()
            );
            assert_eq!(
                stdout,
                format!("{}OK", krusty::backend::BOX_RESULT_FRAME),
                "{target:?} complete framed box result"
            );
            assert!(stderr.is_empty(), "{target:?} box stderr: {stderr:?}");
            let _ = std::fs::remove_dir_all(&directory);
        }
    }

    /// A file dispatching `toString` on an erased receiver, and another file creating a value
    /// whose `toString` this target has no body for, decline together, whichever file comes first.
    /// A lambda converted to a `fun interface` inside a local class is such a value: kotlinc-wasm
    /// names its class after the local class, a name this target does not spell yet.
    #[test]
    fn an_erased_to_string_declines_on_another_files_unprinted_value() {
        let show = "fun show(value: Any?): String = value.toString()\n\
                    fun box(): String = \"OK\"";
        let expected = vec![format!(
            "{}a `toString` of `kotlin.Any?` reaching an object another file creates yet",
            super::DECLINE_PREFIX
        )];
        let other = "fun interface Supplier { fun get(): Int }\n\
                     fun converted(): String {\n\
                     class Local { fun make(): Any = Supplier { 1 } }\n\
                     return show(Local().make()) }";
        for target in WasmTarget::ALL {
            for sources in [[show, other], [other, show]] {
                let (outputs, diagnostics) = compile_program(target, Entry::Box, &sources);
                assert_eq!(diagnostics, expected, "{sources:?}");
                assert!(outputs.is_empty());
            }
        }
    }

    /// A lambda another file converts to a `fun interface` at its top level prints the class name
    /// kotlinc-wasm gives it, so an erased `toString` reaching it is lowered, whichever file comes
    /// first. `wasm_sam_conversions_reached_from_another_file_print_their_class_names` in
    /// `tests/kotlin_box_wasm_conformance.rs` runs the printed names against kotlinc-wasm's.
    #[test]
    fn an_erased_to_string_reaches_another_files_sam_value() {
        let show = "fun show(value: Any?): String = value.toString()\n\
                    fun box(): String = \"OK\"";
        let other = "fun interface Supplier { fun get(): Int }\n\
                     fun converted(): String = show(Supplier { 1 })";
        for target in WasmTarget::ALL {
            for sources in [[show, other], [other, show]] {
                let (outputs, diagnostics) = compile_program(target, Entry::Box, &sources);
                assert_eq!(diagnostics, Vec::<String>::new(), "{sources:?}");
                assert!(!outputs.is_empty(), "{sources:?}");
            }
        }
    }

    /// A floating box prints its value, so another file boxing one leaves an erased `toString`
    /// compilable.
    #[test]
    fn an_erased_to_string_prints_another_files_floating_box() {
        let show = "fun show(value: Any?): String = value.toString()\n\
                    fun box(): String = \"OK\"";
        for other in [
            "fun double(): String = show(1.5)",
            "fun float(): String = show(1.5f)",
        ] {
            for target in WasmTarget::ALL {
                for sources in [[show, other], [other, show]] {
                    let (outputs, diagnostics) = compile_program(target, Entry::Box, &sources);
                    assert_eq!(diagnostics, Vec::<String>::new(), "{sources:?}");
                    assert!(!outputs.is_empty(), "{sources:?}");
                }
            }
        }
    }

    /// A plain interface is not a function value, so a lambda elsewhere in the file does not make
    /// its `toString` unprintable. The value class here delegates `toString` to that interface.
    #[test]
    fn a_plain_interface_to_string_survives_a_function_value() {
        let source = krusty::conformance::prepare_test_source(
            include_str!("../../../tests/box_regressions/discardedIfOverSealedValueClasses.kt"),
            krusty::conformance::TestTarget::WasmJs,
        );
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) =
                compile_program_with_stdlib(target, Entry::Box, &[&source]);
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            assert!(!outputs.is_empty(), "{target:?}");
        }
    }

    /// A function value converted to a `fun interface` prints the class name kotlinc-wasm gives
    /// it, so rendering a value of that interface is lowered.
    #[test]
    fn a_fun_interface_to_string_prints_a_converted_function_value() {
        let source = "fun interface Supplier { fun get(): Int }\n\
                      fun box(): String { val supplier = Supplier { 1 }\n\
                      return if (\"$supplier\" == \"\") \"empty\" else \"OK\" }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            assert!(!outputs.is_empty(), "{target:?}");
        }
    }

    /// A function value converted to a `fun interface` in a local class is an instance of it with
    /// no `toString` body, so rendering a value of that interface declines.
    #[test]
    fn a_fun_interface_to_string_declines_on_a_converted_function_value() {
        let source = "fun interface Supplier { fun get(): Int }\n\
                      fun box(): String {\n\
                      class Local { fun make(): Supplier = Supplier { 1 } }\n\
                      val supplier = Local().make()\n\
                      return if (\"$supplier\" == \"\") \"empty\" else \"OK\" }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(
                diagnostics,
                vec![format!(
                    "{}a `toString` of `Supplier` reaching a runtime object this target cannot \
                     print; first witness: a lambda converted to `Supplier` yet",
                    super::DECLINE_PREFIX
                )]
            );
            assert!(outputs.is_empty());
        }
    }

    /// A `val` of type `Any` holds the string its initializer computed, so rendering it cannot
    /// reach the function value the file also creates.
    #[test]
    fn an_erased_to_string_of_a_val_renders_its_initializer() {
        let source = "fun box(): String { val supplier = { 1 }\n\
                      val message: Any = \"O\"\n\
                      return if (supplier() == 1) \"$message\" + \"K\" else \"fail\" }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            assert!(!outputs.is_empty(), "{target:?}");
        }
    }

    /// A `var` can be assigned anything after its initializer, so rendering it still declines
    /// while the file creates a function value with no `toString` body, one converted to a
    /// `fun interface` in a local class.
    #[test]
    fn an_erased_to_string_of_a_reassigned_var_declines() {
        let source = "fun interface Supplier { fun get(): Int }\n\
                      fun box(): String {\n\
                      class Local { fun make(): Supplier = Supplier { 1 } }\n\
                      val supplier = Local().make()\n\
                      var message: Any = \"O\"\n\
                      if (supplier.get() == 2) message = supplier\n\
                      return \"$message\" + \"K\" }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(
                diagnostics,
                vec![format!(
                    "{}a `toString` of `kotlin.Any` reaching a runtime object this target cannot \
                     print; first witness: a lambda converted to `Supplier` yet",
                    super::DECLINE_PREFIX
                )]
            );
            assert!(outputs.is_empty());
        }
    }

    /// A `var` that may hold a function value converted to a `fun interface` at the file's level
    /// renders it by the class name kotlinc-wasm gives it, so the erased `toString` is lowered.
    #[test]
    fn an_erased_to_string_of_a_reassigned_var_prints_a_sam_value() {
        let source = "fun interface Supplier { fun get(): Int }\n\
                      fun box(): String { val supplier = Supplier { 1 }\n\
                      var message: Any = \"O\"\n\
                      if (supplier.get() == 2) message = supplier\n\
                      return \"$message\" + \"K\" }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            assert!(!outputs.is_empty(), "{target:?}");
        }
    }

    /// A lambda and a reference to a function print their class names, as kotlinc-wasm's do, so an
    /// erased `toString` reaching them is lowered.
    #[test]
    fn an_erased_to_string_reaches_named_function_values() {
        let source = "fun show(value: Any?): String = value.toString()\n\
                      fun box(): String = if (show { 1 } == \"\" || show(::show) == \"\") \
                      \"fail\" else \"OK\"";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new(), "{target:?}");
            assert!(!outputs.is_empty(), "{target:?}");
        }
    }

    /// Another file's local classes and anonymous objects print, so an erased `toString` reaching
    /// them is lowered.
    #[test]
    fn an_erased_to_string_reaches_another_files_local_classes() {
        let show = "fun show(value: Any?): String = value.toString()\n\
                    fun box(): String = \"OK\"";
        let other = "fun local(): String { class Local; return show(Local()) + show(object {}) }";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program(target, Entry::Box, &[show, other]);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_published_inline_reader_compiles_with_its_shared_capture() {
        let source =
            include_str!("../../../tests/box_regressions/PublishedInlineCaptureBindings.kt");
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn shared_mutable_cells_reach_nested_escaping_function_bodies() {
        let source = include_str!(
            "../../../tests/box_regressions/regressions/sharedMutableCaptureTransport.kt"
        );
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn finally_regions_with_nonlocal_transfers_compile_on_both_targets() {
        // The whole runtime contract is tests/box_regressions/finallyOnEveryExit.kt.
        for target in WasmTarget::ALL {
            for source in [
                "fun box(): String { try { return \"OK\" } finally { val unused = 0 } }",
                "fun box(): String { while (true) { try { break } finally { val unused = 0 } }; return \"OK\" }",
                "var keepGoing = true\nfun box(): String { while (keepGoing) { try { keepGoing = false; continue } finally { val unused = 0 } }; return \"OK\" }",
            ] {
                let (outputs, diagnostics) = compile(target, source);
                assert_eq!(diagnostics, Vec::<String>::new());
                assert_eq!(outputs.len(), 2);
            }
        }
    }

    #[test]
    fn checked_reference_identity_operators_accept_reference_carriers() {
        // Backend IR contract: source identity reaches PrimitiveBinOp, not structural Equality.
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "class Box(val value: Int)\n\
                 fun box(): String {\n\
                 \x20   val first = Box(7)\n\
                 \x20   val alias = first\n\
                 \x20   val different = Box(7)\n\
                 \x20   if (first !== alias) return \"alias\"\n\
                 \x20   if (first === different) return \"different\"\n\
                 \x20   return \"OK\"\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_program_is_one_module_and_the_loader_that_runs_it() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, "fun box(): String = \"OK\"");
            assert_eq!(diagnostics, Vec::<String>::new());
            let names = outputs
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            assert_eq!(names, ["main.wasm", "main.mjs"]);
            assert_eq!(&outputs[0].1[..8], b"\0asm\x01\0\0\0");
            let loader = String::from_utf8(outputs[1].1.clone()).expect("utf-8 loader");
            assert!(loader.contains("./main.wasm"), "{loader}");
        }
    }

    #[test]
    fn a_cross_file_inline_template_is_not_emitted_as_a_second_declaration() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program(
                target,
                Entry::Box,
                &[
                    "inline fun answer(): String = \"OK\"",
                    "fun box(): String = answer()",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_class_implementing_an_interface_of_another_file_is_dispatched_and_tested() {
        // Backend contract: interface members are numbered per module, not per file.
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program_with_stdlib(
                target,
                Entry::Box,
                &[
                    "class Named(private val text: String) : Greeting { override fun greet() = text }\n\
                     fun box(): String {\n\
                     \x20   val value: Any = Named(\"OK\")\n\
                     \x20   if (value !is Greeting) return \"not a greeting\"\n\
                     \x20   return value.greet()\n\
                     }",
                    "interface Greeting { fun greet(): String }",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn an_interface_member_with_two_implementations_compiles_to_one_module() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program_with_stdlib(
                target,
                Entry::Box,
                &[
                    "class First : Part { override fun part() = \"O\" }\n\
                     class Second : Part { override fun part() = \"K\" }\n\
                     fun box(): String = (First() as Part).part() + (Second() as Part).part()",
                    "interface Part { fun part(): String }",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_local_class_cloned_from_another_files_inline_template_compiles_in_each_file() {
        // Backend contract: the clone realizes the template's callables a second time, and each
        // file binds them to its own clone's functions.
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program(
                target,
                Entry::Box,
                &[
                    "fun box(): String = make() + \"K\"",
                    "interface I { fun run(): String }\n\
                     inline fun make(): String = object : I { override fun run() = \"O\" }.run()",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn an_unsupported_construct_declines_the_whole_compilation_by_name() {
        let (outputs, diagnostics) = compile(
            WasmTarget::Js,
            "lateinit var pending: String\nfun box(): String = \"OK\"",
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the wasm backend does not support a `lateinit` top-level property yet"]
        );
    }

    #[test]
    fn an_enum_class_compiles_to_one_module_on_both_targets() {
        use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

        for target in WasmTarget::ALL {
            let path = krusty::toolchain::kotlin_stdlib_klib_or_reason(
                krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
            )
            .unwrap_or_else(|reason| {
                panic!("the exact target stdlib must be provisioned: {reason}")
            });
            let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
                path,
                KlibStdlibSemantics::Wasm,
            )])
            .unwrap();
            let (outputs, diagnostics) = compile_program_with_provider(
                target,
                Entry::Box,
                &["enum class Color { RED }\nfun box(): String = \"OK\""],
                Box::new(libraries),
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_class_compiles_to_one_module_on_both_targets() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program_with_stdlib(
                target,
                Entry::Box,
                &[
                    "open class Box(val value: String) { open fun get() = value }\n\
                 class Wrapped(value: String) : Box(value) { override fun get() = value }\n\
                 fun box(): String = (Wrapped(\"OK\") as Box).get()",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn local_class_member_properties_do_not_reserve_module_entries() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "fun box(): String {\n\
                 \x20   class Local(val value: String)\n\
                 \x20   return Local(\"OK\").value\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn anonymous_object_member_properties_do_not_reserve_module_entries() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "fun box(): String {\n\
                 \x20   val local = object { val value = \"OK\" }\n\
                 \x20   return local.value\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn nothing_member_property_getter_has_no_wasm_result() {
        use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

        for target in WasmTarget::ALL {
            let path = krusty::toolchain::kotlin_stdlib_klib_or_reason(
                krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
            )
            .unwrap_or_else(|reason| {
                panic!("the exact target stdlib must be provisioned: {reason}")
            });
            let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
                path,
                KlibStdlibSemantics::Wasm,
            )])
            .unwrap();
            let (outputs, diagnostics) = compile_program_with_provider(
                target,
                Entry::Box,
                &["var flag = true\n\
                 object Test { val magic: Nothing get() = null!! }\n\
                 fun box(): String {\n\
                 \x20   val answer: String\n\
                 \x20   if (flag) answer = \"OK\" else Test.magic\n\
                 \x20   return answer\n\
                 }"],
                Box::new(libraries),
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn an_explicitly_returning_getter_does_not_require_a_unit_value() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "var reads = 0\n\
                 val answer: Int get() { reads++; return 42 }\n\
                 fun box(): String = if (answer == 42 && reads == 1) \"OK\" else \"fail\"",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_program_without_its_entry_emits_nothing() {
        for source in [
            "fun notBox(): String = \"OK\"",
            "fun box(x: Int): String = \"OK\"",
            "fun box(): Int = 0",
            "fun outer(): String { fun box(): String = \"OK\"; return box() }",
        ] {
            let (outputs, diagnostics) = compile(WasmTarget::Wasi, source);
            assert_eq!(outputs, Vec::<Artifact>::new(), "{source}");
            assert_eq!(
                diagnostics,
                ["krusty: the program declares no `fun box(): String` to start in"],
                "{source}"
            );
        }
        let (outputs, diagnostics) =
            compile_program(WasmTarget::Js, Entry::Main, &["fun box(): String = \"OK\""]);
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the program declares no `fun main()` to start in"]
        );
    }

    #[test]
    fn a_program_with_two_entries_emits_nothing() {
        let (outputs, diagnostics) = compile_program(
            WasmTarget::Js,
            Entry::Box,
            &[
                "package a\nfun box(): String = \"OK\"",
                "package b\nfun box(): String = \"OK\"",
            ],
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the program declares `fun box(): String` in both Main and Other1"]
        );
    }

    #[test]
    fn a_main_taking_its_arguments_declines_by_name() {
        let (outputs, diagnostics) = compile_program(
            WasmTarget::Wasi,
            Entry::Main,
            &["fun main(args: Array<String>) {}"],
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the wasm backend does not support a `main` that takes its arguments yet"]
        );
    }

    /// Each operator's width comes from the result common lowering recorded for the selected
    /// operator, so every mixed form lowers without the generator deriving a promotion itself.
    #[test]
    fn mixed_numeric_operators_lower_at_their_selected_result() {
        let source = "fun box(): String {\n\
            \x20   val i = 2147483647\n\
            \x20   if (i + 1L != 2147483648L) return \"fail int long\"\n\
            \x20   if ('a' + 2 != 'c') return \"fail char plus\"\n\
            \x20   if ('z' - 'a' != 25) return \"fail char minus\"\n\
            \x20   val b: Byte = 127\n\
            \x20   val s: Short = 2\n\
            \x20   if (b * s != 254) return \"fail narrow\"\n\
            \x20   var c = 'x'\n\
            \x20   c += 2\n\
            \x20   c++\n\
            \x20   if (c != '{') return \"fail char compound\"\n\
            \x20   var l = 1L\n\
            \x20   l += 2\n\
            \x20   l++\n\
            \x20   return if (l == 4L) \"OK\" else \"fail long compound\"\n\
            }\n";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn unsigned_carriers_require_an_explicit_unsigned_operation() {
        assert!(super::lower::carrier(krusty::types::Ty::UInt)
            .expect("UInt has a physical carrier")
            .is_some());
        assert_eq!(
            super::lower::ordinary_numeric_carrier(krusty::types::Ty::UInt).unwrap_err(),
            "an ordinary operator on `kotlin.UInt`"
        );
        assert_eq!(
            super::lower::ordinary_numeric_carrier(krusty::types::Ty::ULong).unwrap_err(),
            "an ordinary operator on `kotlin.ULong`"
        );
    }

    #[test]
    fn a_main_program_starts_in_main() {
        let (outputs, diagnostics) =
            compile_program(WasmTarget::Wasi, Entry::Main, &["fun main() {}"]);
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
    // PLATFORM-SPECIFIC: These controls inspect Wasm session artifacts and entry diagnostics.
    /// Real separately analyzed modules retain one target state; only the selected source module
    /// contributes a synthetic entry. Metadata still describes every ordinary source module.
    fn compile_scoped_modules(
        target: WasmTarget,
        modules: &[(&str, &[&str])],
    ) -> (Vec<Artifact>, Vec<String>) {
        let backend = WasmBackend::new(target)
            .with_entry(Entry::Box)
            .with_entry_module("application");
        let platform = match target {
            WasmTarget::Js => krusty::klib::write::KlibPlatform::WasmJs,
            WasmTarget::Wasi => krusty::klib::write::KlibPlatform::WasmWasi,
        };
        let metadata = krusty::klib::backend::KlibBackend::new(
            platform,
            krusty::klib::write::KlibStamp::new(
                krusty::features::LangFeatures::new().release(),
                krusty::features::LangFeatures::new().language_version(),
            ),
        );
        let mut session = krusty::compiler::EmissionSession::new(&backend);
        let mut diags = DiagSink::new();
        for (name, sources) in modules {
            let stems = (0..sources.len())
                .map(|i| format!("File{i}"))
                .collect::<Vec<_>>();
            let inputs = sources
                .iter()
                .zip(&stems)
                .map(|(source, stem)| SourceInput::kotlin(source).with_file_stem(stem))
                .collect::<Vec<_>>();
            let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
                &inputs,
                krusty::frontend::PlatformProvider::new(
                    krusty::compiler::Backend::compilation_target(&backend),
                    Box::new(EmptySymbolSource),
                ),
                &krusty::features::LangFeatures::new(),
                &mut diags,
            );
            let Some(module) = session.emit_module(analysis, &stems, &metadata, name, &mut diags)
            else {
                break;
            };
            assert!(
                !module.artifacts().is_empty(),
                "ordinary source metadata is retained"
            );
        }
        let artifacts = session.finish("program", &mut diags);
        (
            artifacts,
            diags.diags.iter().map(|diag| diag.msg.clone()).collect(),
        )
    }

    #[test]
    fn a_library_box_does_not_claim_the_selected_source_modules_program_entry() {
        for target in WasmTarget::ALL {
            let (artifacts, diagnostics) = compile_scoped_modules(
                target,
                &[
                    ("library", &["fun box(): String  = \"library\""]),
                    ("application", &["fun box(): String  = \"OK\""]),
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(artifacts.len(), 2);
        }
    }

    #[test]
    fn scoped_source_modules_still_reject_duplicate_and_missing_program_entries() {
        for target in WasmTarget::ALL {
            for sources in [
                &[
                    "package one\nfun box(): String  = \"OK\"",
                    "package two\nfun box(): String  = \"OK\"",
                ][..],
                &["fun answer(): String  = \"OK\""][..],
            ] {
                let (artifacts, diagnostics) = compile_scoped_modules(
                    target,
                    &[
                        ("library", &["fun box(): String  = \"library\""]),
                        ("application", sources),
                    ],
                );
                assert_eq!(artifacts, Vec::<Artifact>::new());
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(
                    diagnostics[0],
                    if sources.len() == 2 {
                        "krusty: the program declares `fun box(): String` in both File0 and File1"
                    } else {
                        "krusty: the program declares no `fun box(): String` to start in"
                    }
                );
            }
        }
    }
}
