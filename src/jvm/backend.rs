//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::ast::{Decl, File};
use crate::backend::{
    Artifact, Backend, BackendClassifierSource, BackendModuleFacts, CheckedBackendClassifiers,
};
use crate::diag::DiagSink;
use crate::frontend::FrontendSymbols;
use crate::jvm::names::{file_class_name, type_descriptor};
use crate::metadata::local_properties::LocalPropertyMeta;
use crate::types::{type_name, Ty};

/// Why [`run_backend_passes`] declined a file: the named pass met a shape it can't lower yet, so the
/// caller must skip (or diagnose) the file rather than miscompile it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `lower_value_classes` — a `@JvmInline value class` shape not yet supported.
    ValueClasses,
    /// `lower_suspend` — a `suspend fun` shape not yet supported.
    Suspend,
    /// `derive_bridges` — an override whose bridge cannot be modeled (a bounded type-param erasure, or a
    /// `suspend` override the coroutine pass would rewrite out from under the bridge).
    Bridges,
    /// JVM default-argument operands could not be realized from a checked semantic call.
    DefaultCalls,
    /// A plugin-generated checked `super` call could not be given a JVM invocation shape.
    SuperCalls,
    /// A checked declaration argument no longer matched its selected JVM parameter boundary.
    CallArguments(String),
    /// A selected overridden call could not be joined to its checked dispatch classifier facts.
    MemberDispatch(crate::types::TypeName),
    /// A lambda argument of an inline call carried no published parameter modifier and declared
    /// type, so whether the call inlines it is unknown.
    InlineParameters,
}

/// What the plugin pass of [`run_backend_passes`] runs: the native plugins the frontend ran for this
/// compilation, and the module name their output is mangled with.
pub(crate) struct BackendPassPlugins<'a> {
    pub(crate) native_plugins: &'a crate::plugins::registry::NativePlugins,
    pub(crate) module_name: &'a str,
}

/// JVM-only products accumulated by the post-lowering representation pipeline and consumed by
/// emission.
#[derive(Default)]
pub(crate) struct BackendPassFacts {
    continuation_metadata: crate::jvm::suspend::ContinuationMetadataMap,
    /// Suspend functions whose state machine is built during emission because their only suspension
    /// lives inside a body the emitter splices. See `docs/JVM_INLINE_BEFORE_CPS.md`.
    emit_time_machines: crate::jvm::suspend::EmitTimeMachines,
    /// Physical returns that preserve `COROUTINE_SUSPENDED` and otherwise answer `Unit`.
    suspended_result_returns: crate::jvm::suspend::SuspendedResultReturns,
    intrinsic_probe_continuations: crate::jvm::suspend::IntrinsicProbeContinuations,
    /// Suspend-lambda classes whose physical names depend on final caller placement.
    specialized_suspend_lambda_classes: crate::jvm::suspend::SpecializedLambdaClasses,
    default_call_operands: crate::jvm::default_call_operands::DefaultCallOperands,
    bridge_adaptations: crate::jvm::bridge_adaptations::BridgeAdaptations,
    /// The bridges that take `FunctionN.invoke`'s packed argument array.
    function_argument_arrays: crate::jvm::function_argument_arrays::FunctionArgumentArrays,
    /// The overrides whose scalar JVM result is realized as its wrapper.
    override_results: crate::jvm::override_results::OverrideResults,
    /// Neutral-result guards on collection overrides whose descriptor needs no bridge.
    collection_method_entry_barriers: crate::jvm::collection_barriers::MethodEntryBarriers,
    /// JVM-only bridges for mapped collection declarations inherited from a Java superclass.
    inherited_collection_bridges:
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridges,
    /// What the property-reference pass selected for each synthesized reference class. The
    /// value-class pass consumes and extends it; nothing recovers these answers from a spelling.
    property_reference_realizations: crate::jvm::property_references::PropertyReferenceRealizations,
    /// Physical constructions selected for Kotlin function-value SAM wrappers.
    sam_wrapper_realizations: crate::jvm::sam_wrappers::SamWrapperRealizations,
    local_delegate_access: crate::jvm::local_delegate_accessors::HelperAccess,
    lambda_methods: crate::jvm::lambda_classes::LambdaMethods,
    /// Checked property operations realized before representation passes. Companion hoisting
    /// reads it to attach a private accessor to the operation that moves into `<clinit>`.
    property_realizations: crate::jvm::property_realizations::PropertyRealizations,
    /// The `-jvm-default` mode, which decides the forwarders (and their bridges) a class writes for
    /// the interface defaults it inherits.
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
}

/// THE post-lowering, pre-emit JVM pass pipeline — the single definition every consumer (the real
/// backend, `tests/common`, the conformance harness, `bytediff`, `survey`) must call, so a newly
/// added pass lands in all of them by construction. Hand-replicating this sequence has twice
/// produced false-green test runs (a pass added here but missed in a replica → IllegalAccessError
/// miscompiles the gate never saw); a unit test below bans direct calls to the individual passes.
///
/// Runs, in order:
/// 1. `plugins::complete_enabled` — compiler-extension plugins (kotlinx.serialization) realize
///    the declarations they generated at the handoff: bodies, storage and JVM helpers; no-op
///    without a trigger annotation. The members the JVM publishes beyond the handoff's join each
///    class's declaration record. Once their output is final, freeze any exact dependency identity
///    selected by a generated `super` call, then realize all checked super calls from those
///    frozen facts.
///
/// 2. `realize_top_level_jvm_fields` — select public field storage for eligible top-level
///    `@JvmField` declarations through stable property/layout identities.
///
/// 3. `lower_companion_properties` — realize supported companion backing fields as JVM outer statics.
///    Common IR keeps the ordinary declaration and semantic initializer for other targets.
///
/// 4. `check_before_result_coercion` — move a `!!` or platform assertion over the coercion that
///    narrows an erased call result beneath that coercion, so the check reads the slot the call
///    produced and the cast follows, as kotlinc emits it. Runs while that coercion is still the
///    checked one: before generic erasure and call-result boundary realization rewrite the slot
///    and fold the conversion chain it belongs to.
///
/// 5. `realize_call_result_boundaries` — retain the selected declaration's erased JVM result slot
///    and fold its marked conversion chain. A `!!` that immediately unboxes a generic scalar reads
///    that erased reference: a number through `Number`, `Boolean` and `Char` through their wrappers.
///    Later representation passes may refine the slot.
///
/// 6. `sam_wrappers::realize`, then `derive_bridges` — create the JVM class for an existing function
///    value converted to a Kotlin fun interface, then synthesize the `ACC_BRIDGE` methods an override
///    needs to be reachable through a supertype's erased descriptor. Both are JVM realizations of
///    checked declarations; common lowering records only the semantic conversion/override facts.
///
/// 7. `collection_barriers::select` — record collection bridge and method-entry plans.
///
/// 8. `lower_value_classes` — realize `@JvmInline value class`es as their unboxed underlying type
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected).
///
/// 9. `elide_default_property_stores` — omit declaration stores already supplied by JVM field
///    initialization, judged by each field's physical slot now that carriers are realized. Common
///    IR retains them for targets without zero-initialized fields.
///
/// 10. `realize_default_calls` — materialize JVM placeholders, masks, and marker operands only after
///     value-class lowering has fixed their physical carriers.
///
/// 11. `lower_class_capture_slots` — realize marked mutable class captures as JVM `Ref` holders.
///
/// 12. `lower_suspend` — realize `suspend fun`s as their continuation-passing-style ABI.
///
/// 13. `mark_must_inline_lambdas` — drop the dead standalone impl of a must-inline call's
///     (`require`/`check`) message lambda; it is spliced at the call site.
///
/// 14. `reparent_lambda_impls` — a lambda impl method must be a member of the CLASS whose code emits
///     its `invokedynamic` (the impl is PRIVATE, kotlinc's placement, so a cross-class handle would
///     be an IllegalAccessError). Lowering attaches impls per `cur_class`, which misses code that
///     ends up in a class only later: enum-entry constructor arguments and suspend-lambda state
///     machines. Runs after all IR→IR transforms, before emit.
///
/// Per-site concerns (timing counters, bail-reason strings, diagnostics) stay at the call sites.
pub(crate) fn run_backend_passes(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    plugins: BackendPassPlugins<'_>,
    classifiers: &CheckedBackendClassifiers<'_>,
    callables: &mut crate::backend::CheckedBackendCallables,
    classpath: &crate::jvm::classpath::Classpath,
    stems: &[String],
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
    facts: &mut BackendPassFacts,
) -> Result<(), SkipReason> {
    crate::plugins::complete_enabled(
        ir,
        plugins.native_plugins,
        plugins.module_name,
        jvm_plugin_type_descriptor,
        classifiers,
    );
    // The JVM publishes members kotlinc's plugins generate only in IR (serialization's
    // `write$Self`); they join each class's record before any lowering rewrites one.
    crate::metadata::class_declarations::add_target_generated_functions(ir);
    // Plugins run after the frontend/backend handoff and may append checked calls selected by an
    // exact dependency identity. Freeze those provider-normalized records now, once plugin output
    // is final and before any realization consumes them. Existing facts remain the original copy.
    callables
        .freeze_plugin_super_callables(ir, |identity| classpath.external_callable(identity))
        .map_err(|_| SkipReason::SuperCalls)?;
    run_backend_passes_after_plugins(
        ir,
        facade,
        plugins.module_name,
        classifiers,
        callables,
        classpath,
        Some(stems),
        lambda_modes,
        facts,
    )
}

fn run_backend_passes_after_plugins(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    module_name: &str,
    classifiers: &CheckedBackendClassifiers<'_>,
    callables: &crate::backend::CheckedBackendCallables,
    classpath: &crate::jvm::classpath::Classpath,
    stems: Option<&[String]>,
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
    facts: &mut BackendPassFacts,
) -> Result<(), SkipReason> {
    let module_value_classes = classifiers.module().source_value_classes();
    let module_readable_value_classes = classifiers.module().metadata_readable_value_classes();
    // Plugins produce backend-neutral checked IR. Realize any semantic super dispatch they add at
    // the same JVM boundary as source super calls, never in the plugin itself or the emitter.
    crate::jvm::module_calls::realize_super_calls(
        ir,
        callables,
        &mut facts.property_realizations,
        facts.jvm_default,
    )
    .map_err(|_| SkipReason::SuperCalls)?;
    crate::jvm::annotation_constructions::lower_annotation_constructions(ir, facade);
    // A property's own annotations become a synthetic marker method — a JVM realization of a Kotlin
    // declaration that has no class-file form. Before the value-class pass, which renames a marker
    // together with its mangled getter.
    crate::jvm::property_annotations::synthesize_property_annotation_markers(ir);
    // A package property remains a semantic declaration plus a checked common-IR layout. Join the
    // stable identities here, where `@JvmField` becomes a JVM storage decision.
    crate::jvm::property_storage::realize_top_level_jvm_fields(ir);
    // Companion backing-field hoisting is a JVM storage choice. Common IR retains the ordinary
    // property declaration and semantic initializer; this pass selects the outer-static realization.
    crate::jvm::companion::lower_companion_properties(ir, &facts.property_realizations);
    // Delegated properties that share a source name (extension properties for different
    // receivers) each lower a `{name}$delegate` static. The JVM name is unique per owner:
    // the first keeps that spelling and each later one takes the next `$N` suffix.
    crate::jvm::property_storage::realize_delegate_static_field_names(ir);
    // Kotlin parameter nullability is already fixed in common IR. Select the JVM's entry-guard
    // realization before generic/value-class erasure changes the physical parameter types; those
    // later representation passes may then remove a guard whose carrier becomes primitive.
    crate::jvm::parameter_assertions::realize(ir);
    // Common IR retains source type-parameter identities and complete intersections. Select the JVM
    // class-bound erasure here, once, before any descriptor-sensitive backend pass runs.
    crate::jvm::reified_operations::realize(ir, &facts.lambda_methods);
    // A null check over an erased call result reads the call's own slot, ahead of the coercion
    // that narrows it; erasure and call-result boundaries below rewrite that coercion.
    crate::jvm::result_null_checks::check_before_result_coercion(ir);
    // A checked Kotlin function-value conversion is represented by one file-owned wrapper class.
    // Create it before generic erasure and bridge/value-class realization process its generated
    // method, retaining nullable-construction sites only in JVM pass facts.
    let class_java_sam = lambda_modes.sam_conversions == crate::jvm::ir_emit::LambdaMode::Class;
    facts.sam_wrapper_realizations = crate::jvm::sam_wrappers::realize(ir, facade, class_java_sam);
    // A projected SAM conversion stays logically typed as the function it captures. Only the JVM
    // implementation method receives the declaration's erased slots, after class-mode wrappers
    // have consumed conversions that realize their own forwarding methods.
    crate::jvm::sam_projected_adapters::realize(ir);
    crate::jvm::generic_erasure::lower_function_type_parameters(ir);
    crate::jvm::deferred_local_storage::realize(ir);
    crate::jvm::call_result_boundaries::realize_call_result_boundaries(ir);
    // Bridges are a JVM realization of an override, derived here from the IR's own declarations and the
    // checker's supertype view. Runs BEFORE the barrier pass (which annotates existing bridges) and
    // before the value-class pass (which retargets them once mangled names are known).
    // A scalar result over a reference-returning overridden slot is the wrapper; its bridges follow.
    // A bridge to a value-class override asks which dependency classes are value classes.
    if !crate::jvm::value_classes::record_referenced_value_classes(ir, classifiers) {
        return Err(SkipReason::ValueClasses);
    }
    facts.override_results = crate::jvm::override_results::box_scalar_override_results(
        ir,
        callables,
        &facts.property_realizations,
    )?;
    crate::jvm::bridges::derive_bridges(
        ir,
        classpath,
        callables,
        &facts.override_results,
        &mut facts.function_argument_arrays,
        facts.jvm_default,
    )?;
    crate::jvm::collection_barriers::select(
        ir,
        &facts.override_results,
        &mut facts.collection_method_entry_barriers,
    );
    facts.inherited_collection_bridges =
        crate::jvm::inherited_collection_bridges::select(ir, classpath, &facts.override_results);
    // Same-module SOURCE value classes (internal name → sole-field underlying) for the value-class pass's
    // erasure/mangle map — a value class declared in ANOTHER file of this module. Read from the frontend
    // symbols directly, NOT surfaced through the resolver's library view (which would change the checker's
    // construction/member resolution for source value classes).
    if !crate::jvm::value_classes::lower_value_classes(
        ir,
        classifiers,
        module_value_classes,
        module_readable_value_classes,
        &mut facts.bridge_adaptations,
        &mut facts.override_results,
        &mut facts.property_reference_realizations,
    ) {
        return Err(SkipReason::ValueClasses);
    }
    // Companion fields move before value-class erasure. A private property reference was realized
    // against the companion instance accessor; point it at the outer class's `access$…$cp` now
    // that the field's physical type is known.
    crate::jvm::property_references::retarget_hoisted_companion_references(
        ir,
        &mut facts.property_reference_realizations,
    );
    crate::jvm::parameter_assertions::finalize_after_value_class_lowering(ir);
    crate::jvm::default_parameter_representation::record_defaulted_primitive_bounds(ir);
    // A concatenation appends a `toString()` call's receiver itself, as kotlinc does once its
    // value-class lowering has turned a value class's `toString()` into a static call.
    crate::jvm::concatenated_to_string::flatten_to_string_operands(ir);
    // Every body of the file is lowered, so each lifting sequence is whole, and the value-class
    // pass has named the functions kotlinc names their lifted callables after: name those callables
    // before any pass renders a debug name from them.
    crate::jvm::lifted_names::number(ir, &facts.local_delegate_access);
    // Generic erasure and value-class projection have now fixed every declaration parameter's JVM
    // carrier. Consume and retarget the exact call-owned adapters before default/suspend/inline
    // transforms clone or wrap those calls; the provenance is a one-shot representation contract.
    crate::jvm::physical_call_arguments::retarget_declaration_arguments(ir)
        .map_err(SkipReason::CallArguments)?;
    if !facts.default_call_operands.synchronize(ir) {
        return Err(SkipReason::CallArguments(
            "default call operand plan no longer matches its call".to_string(),
        ));
    }
    // The JVM supplies default field values before any constructor runs. Elide only source
    // declaration stores recorded by exact ExprId; common IR and other targets keep them. Runs
    // after value-class lowering, so each field's type is the physical slot the JVM zero-fills.
    crate::jvm::property_storage::elide_default_property_stores(ir);
    if let Some(stems) = stems {
        crate::jvm::module_calls::resolve_foreign_template_facades(ir, stems);
        crate::jvm::module_calls::realize_default_calls(
            ir,
            stems,
            &mut facts.default_call_operands,
        )
        .map_err(|_| SkipReason::DefaultCalls)?;
    }
    crate::jvm::shared_captures::lower_class_capture_slots(ir);
    // An inline expansion rewrites a type-parameter cell to the call-site element. A lambda that
    // still uses the shared erased implementation takes the erased holder, and suspension spilling
    // reads that holder from the cell, so the two have to agree before either pass.
    crate::jvm::shared_captures::restore_erased_escaping_capture_holders(ir);
    let null_out_dead_spills =
        crate::jvm::runtime_capabilities::null_out_spilled_variable(classpath);
    if !crate::jvm::suspend::lower_suspend(
        ir,
        facade,
        &mut facts.continuation_metadata,
        &mut facts.default_call_operands,
        &mut facts.emit_time_machines,
        &mut facts.suspended_result_returns,
        &mut facts.intrinsic_probe_continuations,
        &mut facts.specialized_suspend_lambda_classes,
        null_out_dead_spills,
    ) {
        return Err(SkipReason::Suspend);
    }
    crate::jvm::suspend::finalize_suspend_bridges(ir, &mut facts.bridge_adaptations);
    // After the suspend transform: the body moved onto the static is the finished state machine.
    crate::jvm::suspend_impls::lower_suspend_impls(ir);
    // Source lambda names are inputs to lifting. Fix them before reparenting so the lifted-name
    // pass can retain the final caller-qualified path and ordinal.
    crate::jvm::debug_local_names::realize_source_lambda_implementation_names(ir);
    crate::jvm::ir_emit::mark_must_inline_lambdas(ir).map_err(|missing| {
        crate::trace_compiler!(
            "splice",
            "inline call {} published no parameter facts for lambda operand {}",
            missing.call,
            missing.parameter
        );
        SkipReason::InlineParameters
    })?;
    crate::jvm::ir_emit::reparent_lambda_impls(ir);
    // After reparenting: a lifted name is distinct only within the class the method lands in.
    crate::jvm::lifted_names::realize(ir, &facts.override_results);
    // After lifted names are fixed. The module suffix must not leak into `$lambda$N`: kotlinc
    // names those from the value-class spelling and only then appends `$<module>` to the member.
    crate::jvm::internal_names::mangle_internal_members(ir, module_name, facade);
    // A specialized suspend lambda is already a real class when suspend lowering completes, but
    // its JVM name depends on the caller's final placement and lifted spelling. Realize that name
    // only now and keep the coroutine-emission facts keyed by the same physical identity.
    facts.specialized_suspend_lambda_classes.realize(
        ir,
        facade,
        lambda_modes,
        &mut facts.emit_time_machines,
    );
    // A specialized reified lambda is already a function class. Its `$$inlined$` name depends on
    // the caller's lifted spelling, which exists only now.
    let closure_names =
        crate::jvm::lambda_classes::rename_specialized_reified_classes(ir, facade, lambda_modes);
    crate::jvm::reified_anonymous::rename(ir, facade, lambda_modes);
    facts
        .property_reference_realizations
        .remap_delegated_owners(&closure_names);
    // With placement and lifted caller names final, realize JVM-only implementation spellings.
    crate::jvm::debug_local_names::realize_lambda_implementation_names(
        ir,
        facade,
        lambda_modes,
        &facts.specialized_suspend_lambda_classes,
    );
    crate::jvm::overridden_calls::realize(ir, classifiers, callables, &facts.property_realizations)
        .map_err(|missing| SkipReason::MemberDispatch(missing.0))?;
    // Every type the emitter will test or cast against is final now: carry each referenced
    // classifier's checked role into the IR, where type operations read it.
    ir.publish_classifier_roles(classifiers);
    Ok(())
}

fn jvm_plugin_type_descriptor(ty: Ty) -> Option<String> {
    Some(type_descriptor(ty))
}

/// The JVM backend holds the shared classpath (`Rc`, same instance as `JvmLibraries`) so the emitter
/// can read inline-function bodies for the bytecode inliner.
pub struct JvmBackend {
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
    /// Class-file major version to emit (`-jvm-target`), or `None` for krusty's default (v52).
    class_major: Option<u16>,
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
    java_parameters: bool,
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
    /// Whether to emit the `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions`
    /// clears this).
    param_assertions: bool,
    /// Whether to emit the `Intrinsics.checkNotNullExpressionValue` guard on a narrowed platform
    /// value (`-Xno-call-assertions` clears this).
    call_assertions: bool,
    /// `-language-version X.Y`: the `@kotlin.Metadata` `mv` and `.kotlin_module` header version to
    /// stamp; `None` keeps the default ([`crate::jvm::ir_emit::DEFAULT_METADATA_VERSION`]).
    metadata_version: Option<[i32; 3]>,
    /// Whether the source-language configuration enables kotlinc's
    /// `LanguageFeature.AnnotationsInMetadata`. This is deliberately independent of the physical
    /// metadata stamp: an internal stamp override must not change source-language semantics.
    annotations_in_metadata: bool,
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<crate::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend {
            cp,
            class_major: None,
            jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
            java_parameters: false,
            lambda_modes: crate::jvm::ir_emit::LambdaModes::default(),
            param_assertions: true,
            call_assertions: true,
            metadata_version: None,
            annotations_in_metadata: true,
        }
    }

    /// `-language-version X.Y` stamps every `@kotlin.Metadata` `mv` and the `.kotlin_module`
    /// header with `[X, Y, 0]`; `None` keeps kotlinc's no-flag stamp.
    pub fn with_metadata_version(mut self, version: Option<[i32; 3]>) -> JvmBackend {
        self.metadata_version = version;
        self
    }

    /// Select declaration-annotation record emission from the finalized source-language feature
    /// set. Metadata versioning is an output representation choice and does not select this gate.
    pub fn with_annotations_in_metadata(mut self, enabled: bool) -> JvmBackend {
        self.annotations_in_metadata = enabled;
        self
    }

    /// `-Xno-param-assertions` passes `false`: emit no `Intrinsics.checkNotNullParameter` guards.
    pub fn with_param_assertions(mut self, enabled: bool) -> JvmBackend {
        self.param_assertions = enabled;
        self
    }

    /// `-Xno-call-assertions` passes `false`: emit no `Intrinsics.checkNotNullExpressionValue` guard
    /// where a platform value is narrowed to a declared non-null type.
    pub fn with_call_assertions(mut self, enabled: bool) -> JvmBackend {
        self.call_assertions = enabled;
        self
    }

    /// `-jvm-default`: which JVM shape an interface's members with bodies are compiled into.
    pub fn with_jvm_default(mut self, mode: crate::jvm::ir_emit::JvmDefaultMode) -> JvmBackend {
        self.jvm_default = mode;
        self
    }

    /// `-java-parameters`: write a `MethodParameters` attribute naming each declared parameter.
    pub fn with_java_parameters(mut self, enabled: bool) -> JvmBackend {
        self.java_parameters = enabled;
        self
    }

    /// Independently select `-Xlambdas` and `-Xsam-conversions` realization strategies.
    pub fn with_lambda_modes(mut self, modes: crate::jvm::ir_emit::LambdaModes) -> JvmBackend {
        self.lambda_modes = modes;
        self
    }

    /// Set the class-file version subsequent emits target (from the CLI's `-jvm-target`).
    pub fn with_class_major(mut self, major: Option<u16>) -> JvmBackend {
        self.class_major = major;
        self
    }
}

/// The per-file emit configuration krusty SHIPS with — ONE definition, so an in-process caller (the
/// test harness, an embedder) emits exactly what `krusty -d …` does. It carries the class version
/// (`-jvm-target`), the `SourceFile` name (the origin `.kt`; kotlinc uses the simple name,
/// reconstructed from the stem — directories are already stripped), the `-module-name`, and the
/// per-class `@Metadata` switch. Threaded explicitly into emission so every class (incl. synthetics)
/// inherits it. `EmitOptions::default()` is NOT the pre-class-metadata shape — it emits per-class
/// `@Metadata` too; what it lacks is the `SourceFile`, the inner-class resolver and the `-jvm-target`
/// class version, which is why a caller claiming to emit shipping bytes must start here.
/// `KRUSTY_NO_CLASS_METADATA` (read below) is how a shipping caller gets facade-only output back; it is
/// consulted ONLY here, so a caller holding some other `EmitOptions` opts out by setting
/// `emit_class_metadata: false` itself.
pub fn shipping_emit_options(
    stem: &str,
    module_name: &str,
    class_major: Option<u16>,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::ir_emit::EmitOptions {
    // Module/conformance inputs may retain a source-relative prefix (`helpers/Foo`) even though the
    // CLI has already reduced its input to `Foo`. `SourceFile` is a simple filename on every JVM
    // class, so normalize at this shared boundary instead of making each non-CLI caller grow its own
    // path branch. Accept both separators because Kotlin testdata names are logical source paths and
    // are not guaranteed to use the host platform's separator.
    let source_stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    crate::jvm::ir_emit::EmitOptions {
        class_major,
        source_file: Some(format!("{source_stem}.kt")),
        // kotlinc records `classModuleName` in @Metadata unless the module is the default `main`.
        module_name: (module_name != "main").then(|| module_name.to_string()),
        // Per-invocation strategies; the CLI overrides them on the backend.
        lambda_modes: crate::jvm::ir_emit::LambdaModes::default(),
        java_parameters: false,
        // Compute + emit each class's own `@Metadata`. Without it a krusty-compiled CLASS is
        // unreadable BY KRUSTY: the facade metadata describes top-level declarations only, so a
        // second compilation sees no constructor/member parameter names (named arguments) and no
        // `operator` marks (destructuring). A shape `build_class_metadata` has not verified against
        // kotlinc declines individually and emits nothing, so this cannot write an unverified
        // payload. `KRUSTY_NO_CLASS_METADATA` restores the facade-only output for bisecting.
        emit_class_metadata: std::env::var_os("KRUSTY_NO_CLASS_METADATA").is_none(),
        jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
        // `-Xno-param-assertions` is applied by the caller (`with_param_assertions`); the shipping
        // default emits the guards, as kotlinc does.
        param_assertions: true,
        inner_class_resolver: Some(classpath_inner_class_resolver(cp)),
        value_classes: std::rc::Rc::default(),
        // `-language-version` is a per-invocation option; the backend applies it with
        // `EmitOptions::with_metadata_version`. The default keeps kotlinc's no-flag stamp.
        metadata_version: None,
        // The shipping default language level is 2.4, where this feature is enabled. A configured
        // compiler invocation overrides it through `EmitOptions::with_annotations_in_metadata`.
        annotations_in_metadata: true,
    }
}

fn checked_module_inner_class_resolver(
    module: &BackendModuleFacts,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    module_inner_class_resolver_from_shapes(
        module
            .classifiers()
            .map(|(classifier, shape)| InnerModuleClassifier {
                classifier,
                visibility: shape.access.visibility(),
                inner: shape.outer_instance.is_some(),
                annotation: shape.is_annotation(),
                interface: shape.is_interface(),
                enum_class: shape.is_enum(),
                abstract_class: shape.is_abstract,
                final_class: !shape.is_abstract && !shape.is_extensible,
            }),
        module.generated_classifiers().iter(),
        cp,
    )
}

#[derive(Clone, Copy)]
struct InnerModuleClassifier {
    classifier: crate::types::TypeName,
    visibility: crate::types::Visibility,
    inner: bool,
    annotation: bool,
    interface: bool,
    enum_class: bool,
    abstract_class: bool,
    final_class: bool,
}

fn module_inner_class_resolver_from_shapes<'a>(
    classes: impl IntoIterator<Item = InnerModuleClassifier>,
    generated: impl IntoIterator<Item = &'a crate::types::GeneratedClassifierFact>,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    const PUBLIC: u16 = 0x0001;
    const PROTECTED: u16 = 0x0004;
    const PRIVATE: u16 = 0x0002;
    const STATIC: u16 = 0x0008;
    const FINAL: u16 = 0x0010;
    const INTERFACE: u16 = 0x0200;
    const ABSTRACT: u16 = 0x0400;
    const ANNOTATION: u16 = 0x2000;
    const ENUM: u16 = 0x4000;
    const SYNTHETIC: u16 = 0x1000;

    let classes = classes.into_iter().collect::<Vec<_>>();
    let module_names = classes
        .iter()
        .map(|class| class.classifier)
        .collect::<std::collections::HashSet<_>>();
    let mut source = std::collections::HashMap::new();
    for class in classes {
        let internal = class.classifier.render();
        // Only MEMBER-nested classes get snapshot entries, and the outer boundary is NOT the last
        // `$` (mirroring `register_inner_classes`): the boundary is the longest proper prefix that
        // is itself a module class. A name whose remainder still carries `$` past that boundary is
        // a hoisted LOCAL class (`pkg/Outer$m$Local` — `m` is a function, not a class) or a
        // backticked simple name; kotlinc spells a local's entry with `outer_class_info_index = 0`,
        // and inventing an outer makes the loader chase a class that does not exist
        // (`NoClassDefFoundError`). The two are not distinguishable from the name alone (a
        // backticked MEMBER `` class `X$Y` `` is denotable cross-file and would deserve an entry),
        // so the snapshot omits the ambiguous shape rather than risk fabricating an outer.
        let Some(boundary) = internal
            .char_indices()
            .filter(|&(_, ch)| ch == '$')
            .map(|(at, _)| at)
            .filter(|&at| module_names.contains(&crate::types::type_name(&internal[..at])))
            .max()
        else {
            continue; // top-level (or nested under nothing this module declares)
        };
        let (outer, simple) = (&internal[..boundary], &internal[boundary + 1..]);
        if simple.contains('$') {
            continue;
        }
        let visibility = match class.visibility {
            crate::types::Visibility::Protected => PROTECTED,
            crate::types::Visibility::Private => PRIVATE,
            _ => PUBLIC,
        };
        let mut access = visibility | if class.inner { 0 } else { STATIC };
        if class.annotation {
            access |= INTERFACE | ABSTRACT | ANNOTATION;
        } else if class.interface {
            access |= INTERFACE | ABSTRACT;
        } else if class.enum_class {
            access |= FINAL | ENUM;
        } else if class.abstract_class {
            access |= ABSTRACT;
        } else if class.final_class {
            access |= FINAL;
        }
        source.insert(
            internal.clone(),
            crate::jvm::classfile::InnerClassDetails {
                outer: Some(outer.to_string()),
                name: Some(simple.to_string()),
                access,
            },
        );
    }
    for class in generated {
        let visibility = match class.visibility {
            crate::types::Visibility::Protected => PROTECTED,
            crate::types::Visibility::Private => PRIVATE,
            _ => PUBLIC,
        };
        let mut access = visibility | if class.captures_outer { 0 } else { STATIC };
        match class.kind {
            crate::types::GeneratedClassifierKind::Annotation => {
                access |= INTERFACE | ABSTRACT | ANNOTATION;
            }
            crate::types::GeneratedClassifierKind::Interface => {
                access |= INTERFACE | ABSTRACT;
            }
            crate::types::GeneratedClassifierKind::Enum => access |= ENUM,
            crate::types::GeneratedClassifierKind::Class => {}
        }
        if class.is_abstract {
            access |= ABSTRACT;
        }
        if class.is_final {
            access |= FINAL;
        }
        if class.compiler_generated {
            access |= SYNTHETIC;
        }
        source.insert(
            class.classifier.render(),
            crate::jvm::classfile::InnerClassDetails {
                outer: Some(class.lexical_owner.render()),
                name: Some(class.source_name.to_string()),
                access,
            },
        );
    }
    let classpath = classpath_inner_class_resolver(cp);
    std::rc::Rc::new(move |internal: &str| {
        source
            .get(internal)
            .cloned()
            .or_else(|| classpath(internal))
    })
}

pub fn classpath_inner_class_resolver(
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    std::rc::Rc::new(move |internal: &str| {
        // The class file first — it is authoritative whenever the nested class has one. A mapped
        // builtin whose JVM class is absent (no JDK on the classpath) still declares the same nesting
        // in its `.kotlin_builtins` entry; without this fallback the reference emits no `InnerClasses`
        // attribute at all, so the JDK-less class file diverges from the JDK-present one.
        cp.find(internal)
            .and_then(|class| {
                let entry = class.inner_class_self()?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: entry.outer.clone(),
                    name: entry.name.clone(),
                    // ACC_SYNTHETIC does NOT cross the compilation boundary. kotlinc knows a class
                    // is compiler-generated only while it is compiling it; a class read back from
                    // the classpath is just a declaration, and the row it writes for one omits the
                    // bit even though that class's OWN row carries it. Measured on a generated
                    // `$serializer`: 0x1019 in the module that declares it, 0x0019 in every module
                    // that only references it.
                    access: entry.access & !0x1000,
                })
            })
            .or_else(|| {
                let (outer, name, access) = cp.builtin_nested_class(internal)?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: Some(outer),
                    name: Some(name),
                    access,
                })
            })
    })
}

pub fn prepare_module_symbols(files: &[File], stems: &[String], syms: &mut FrontendSymbols) {
    if files.len() <= 1 {
        return;
    }

    let mut fns: Vec<(u32, u32, Option<String>, String)> = Vec::new();
    let mut unemitted_fns: Vec<(u32, crate::ast::DeclId)> = Vec::new();
    let mut props: Vec<(u32, u32, String, String)> = Vec::new();
    let mut ext_props: Vec<(u32, u32, String)> = Vec::new();
    for (i, (file, stem)) in files.iter().zip(stems).enumerate() {
        let facade = file_class_name(stem, file.package.as_deref());
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    // This is the single owner of the emitted/unemitted decision. The checker
                    // consumes the declaration-keyed outcome recorded below; it must not repeat
                    // this predicate because signature/body support evolves with JVM lowering.
                    // The semantic handoff says whether a callable body exists; this JVM boundary
                    // owns only its representation as a facade static. Common IR lowering consumes
                    // the same answer, so registration cannot promise a body that lowering omits.
                    let emitted = syms.source_fn_has_callable_body(file, f);
                    if emitted {
                        fns.push((
                            i as u32,
                            d.0,
                            f.receiver.is_none().then(|| f.name.clone()),
                            facade.clone(),
                        ));
                    } else {
                        // Negative registration is as important as the facade map: it distinguishes
                        // a deliberately splice-only function from a checker-only pipeline where no
                        // JVM registration ran at all.
                        unemitted_fns.push((i as u32, d));
                    }
                }
                Decl::Property(p) if p.receiver.is_none() => {
                    props.push((i as u32, d.0, p.name.clone(), facade.clone()))
                }
                Decl::Property(_) => ext_props.push((i as u32, d.0, facade.clone())),
                _ => {}
            }
        }
    }

    for (file_index, decl_id, name, facade) in fns {
        let facade = type_name(&facade);
        syms.record_fn_facade(file_index, crate::ast::DeclId(decl_id), Some(facade));
        if let Some(name) = name {
            syms.fn_facades.insert(name, facade);
        }
    }
    for (file_index, declaration) in unemitted_fns {
        syms.record_fn_facade(file_index, declaration, None);
    }
    for (file, declaration, name, facade) in props {
        syms.prop_facades_by_decl
            .insert((file, declaration), type_name(&facade));
        if let Some(&(ty, is_var, is_const)) = syms.props.get(&name) {
            syms.prop_facades
                .insert(name, (type_name(&facade), ty, is_var, is_const));
        }
    }
    for (file_index, decl_id, facade) in ext_props {
        syms.ext_prop_facades_by_decl
            .insert((file_index, decl_id), type_name(&facade));
    }
}

/// package → file-facade class names, accumulated across files for the `.kotlin_module` mapping.
#[derive(Default)]
pub struct JvmState {
    module_packages: std::collections::BTreeMap<String, Vec<String>>,
    /// One compilation emits many files against one frozen module. The inner-class attribute map
    /// depends only on that module, so building it again on every file is quadratic.
    inner_class_resolver: Option<crate::jvm::classfile::InnerClassResolver>,
}

/// A checked file after every JVM representation pass has selected its physical facts. Keeping the
/// handoff together makes the boundary explicit: emission consumes this closed product and must not
/// recover any of its decisions.
struct BackendReadyIr<'a> {
    ir: crate::ir::IrFile,
    stem: &'a str,
    module_name: &'a str,
    facade_name: String,
    facade_class: crate::types::TypeName,
    package: String,
    signature_symbols: &'a dyn BackendClassifierSource,
    dependency_callables: crate::backend::CheckedBackendCallables,
    inner_class_resolver: crate::jvm::classfile::InnerClassResolver,
    pass_facts: BackendPassFacts,
    metadata: Option<crate::jvm::ir_emit::KotlinMetadata>,
    has_facade_members: bool,
}

impl JvmBackend {
    fn emit_streamed_ir(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        property_realizations: crate::jvm::property_realizations::PropertyRealizations,
        mut pass_facts: BackendPassFacts,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let crate::backend::CheckedIrFile {
            mut ir,
            source,
            classifiers,
            mut callables,
            native_plugins,
            module_name,
            stems,
        } = file;
        let stem = &stems[source.raw() as usize];
        let package = ir.package.clone().unwrap_or_default();
        let facade_name = file_class_name(stem, ir.package.as_deref());
        let facade_class = crate::types::type_name(&facade_name);
        pass_facts.property_realizations = property_realizations;
        if let Err(reason) = run_backend_passes(
            &mut ir,
            &facade_name,
            BackendPassPlugins {
                native_plugins,
                module_name,
            },
            &classifiers,
            &mut callables,
            &self.cp,
            stems,
            self.lambda_modes,
            &mut pass_facts,
        ) {
            report_backend_pass_failure(reason, diags);
            return Vec::new();
        }
        if !pass_facts.default_call_operands.synchronize(&ir) {
            diags.error(
                crate::diag::Span::new(0, 0),
                "default call operand plan no longer matches its call".to_string(),
            );
            return Vec::new();
        }
        if crate::jvm::declaration_collisions::validate(&ir, &pass_facts.override_results, diags)
            .is_err()
        {
            return Vec::new();
        }
        let facade_locals = pass_facts
            .property_reference_realizations
            .local_delegated
            .of(facade_class);
        let metadata = facade_package_metadata_from_ir(
            &ir,
            (module_name, facade_locals),
            self.param_assertions,
            &classifiers,
            self.metadata_version
                .unwrap_or(crate::jvm::ir_emit::DEFAULT_METADATA_VERSION),
            self.annotations_in_metadata,
        );
        let has_facade_members = metadata.is_some();
        let inner_class_resolver = match state.inner_class_resolver.clone() {
            Some(resolver) => resolver,
            None => {
                let resolver =
                    checked_module_inner_class_resolver(classifiers.module(), self.cp.clone());
                state.inner_class_resolver = Some(resolver.clone());
                resolver
            }
        };
        self.emit_backend_ready_ir(
            BackendReadyIr {
                ir,
                stem,
                module_name,
                facade_name,
                facade_class,
                package,
                signature_symbols: &classifiers,
                dependency_callables: callables,
                inner_class_resolver,
                pass_facts,
                metadata,
                has_facade_members,
            },
            state,
            diags,
        )
    }

    fn emit_backend_ready_ir(
        &self,
        ready: BackendReadyIr<'_>,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let BackendReadyIr {
            mut ir,
            stem,
            module_name,
            facade_name,
            facade_class,
            package,
            signature_symbols,
            dependency_callables,
            inner_class_resolver,
            pass_facts,
            metadata,
            has_facade_members,
        } = ready;
        let mut outputs = Vec::new();
        if !self.param_assertions {
            crate::jvm::parameter_assertions::strip(&mut ir);
        }
        if !self.call_assertions {
            crate::jvm::ir_emit::strip_call_assertions(&mut ir);
        }
        // Suspend lowering can move a catch onto a fresh `try`. Rebuild the marker plan from the
        // declaration parameters so emission names the handler that is actually written.
        crate::jvm::reified_operations::record_catch_markers(&mut ir, &pass_facts.lambda_methods);
        let mut emit_opts =
            shipping_emit_options(stem, module_name, self.class_major, self.cp.clone())
                .with_jvm_default(self.jvm_default)
                .with_lambda_modes(self.lambda_modes)
                .with_param_assertions(self.param_assertions)
                .with_metadata_version(self.metadata_version)
                .with_annotations_in_metadata(self.annotations_in_metadata)
                .with_java_parameters(self.java_parameters);
        emit_opts.inner_class_resolver = Some(inner_class_resolver);
        let run = crate::jvm::ir_emit::EmitRun::default();
        let emit_metadata = crate::jvm::ir_emit::EmitMetadata {
            facade: metadata.as_ref(),
            continuations: &pass_facts.continuation_metadata,
            emit_time_machines: &pass_facts.emit_time_machines,
            suspended_result_returns: &pass_facts.suspended_result_returns,
            intrinsic_probe_continuations: &pass_facts.intrinsic_probe_continuations,
            bridge_adaptations: &pass_facts.bridge_adaptations,
            function_argument_arrays: &pass_facts.function_argument_arrays,
            override_results: &pass_facts.override_results,
            collection_method_entry_barriers: &pass_facts.collection_method_entry_barriers,
            inherited_collection_bridges: &pass_facts.inherited_collection_bridges,
        };
        let classes = crate::jvm::ir_emit::emit_all_with_checked_classifiers(
            &ir,
            (facade_class, &facade_name),
            &*self.cp,
            crate::jvm::ir_emit::CheckedEmitFacts {
                metadata: emit_metadata,
                signature_symbols,
                dependency_callables: &dependency_callables,
                property_realizations: &pass_facts.property_realizations,
                property_reference_realizations: &pass_facts.property_reference_realizations,
                default_call_operands: &pass_facts.default_call_operands,
                sam_wrapper_realizations: &pass_facts.sam_wrapper_realizations,
                local_delegate_access: &pass_facts.local_delegate_access,
            },
            &emit_opts,
            &run,
        );
        let Some(classes) = classes else {
            if let Some(reason) = run.inline_bail() {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: JVM backend inline error: {reason}"),
                );
                return outputs;
            }
            if let Some(reason) = run.emit_error() {
                diags.error(crate::diag::Span::new(0, 0), reason);
                return outputs;
            }
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        for (internal, bytes) in classes {
            outputs.push((format!("{internal}.class"), bytes));
        }

        if has_facade_members {
            let facade = facade_name
                .rsplit('/')
                .next()
                .unwrap_or(&facade_name)
                .to_string();
            state
                .module_packages
                .entry(package)
                .or_default()
                .push(facade);
        }
        outputs
    }
}

fn report_backend_pass_failure(reason: SkipReason, diags: &mut DiagSink) {
    match reason {
        SkipReason::DefaultCalls => {
            diags.error(
                crate::diag::Span::new(0, 0),
                "internal error: invalid checked default-argument realization".to_string(),
            );
            return;
        }
        SkipReason::SuperCalls => {
            diags.error(
                crate::diag::Span::new(0, 0),
                "internal error: invalid checked super-call realization".to_string(),
            );
            return;
        }
        SkipReason::CallArguments(detail) => {
            diags.error(crate::diag::Span::new(0, 0), detail);
            return;
        }
        SkipReason::InlineParameters => {
            diags.error(
                crate::diag::Span::new(0, 0),
                "internal error: an inline call's lambda argument has no published parameter modifier and declared type".to_string(),
            );
            return;
        }
        SkipReason::MemberDispatch(classifier) => {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!(
                    "internal error: missing JVM dispatch classifier fact for {}",
                    classifier.render()
                ),
            );
            return;
        }
        _ => {}
    }
    let what = match reason {
        SkipReason::ValueClasses => "value-class",
        SkipReason::Suspend => "suspend-function",
        SkipReason::Bridges => "bridge-method",
        SkipReason::DefaultCalls
        | SkipReason::SuperCalls
        | SkipReason::CallArguments(_)
        | SkipReason::InlineParameters
        | SkipReason::MemberDispatch(_) => {
            unreachable!()
        }
    };
    diags.error(
        crate::diag::Span::new(0, 0),
        format!("krusty: this {what} shape is not yet supported by the IR backend"),
    );
}

impl Backend for JvmBackend {
    type State = JvmState;

    fn lower_ir_file(
        &self,
        mut file: crate::backend::CheckedIrFile<'_>,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = &file.stems[file.source.raw() as usize];
        let facade = file_class_name(stem, file.ir.package.as_deref());
        let facade_class = type_name(&facade);
        let stems = &file.stems;
        crate::jvm::local_class_names::realize(&mut file.ir, |source| {
            crate::jvm::module_calls::facade_for(source, stems)
                .expect("a local classifier's declaring source has a file stem")
        });
        if let Err(error) = crate::jvm::ranges::realize(&mut file.ir, self.cp.clone()) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot realize checked range operation: {error:?}"),
            );
            return Vec::new();
        }
        if let Err(target) = crate::jvm::function_references::realize(
            &mut file.ir,
            &file.callables,
            crate::jvm::function_references::Facades {
                current: &facade,
                stems,
            },
        ) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!(
                    "internal error: missing JVM function-reference realization for {target:?}"
                ),
            );
            return Vec::new();
        }
        // A lambda the metafactory cannot adapt is a class of its own; decided before any lambda
        // is numbered, as kotlinc numbers only the lambdas it lifts.
        let current_source = crate::ir::IrModuleSource {
            source: file.source,
            package: facade_class.namespace(),
        };
        let delegate_closures =
            crate::jvm::local_delegate_closures::Requirements::collect(&file.ir);
        let mut lambda_methods = match crate::jvm::lambda_classes::realize(
            &mut file.ir,
            &file.classifiers,
            &facade,
            &delegate_closures,
            current_source,
            self.lambda_modes.lambdas == crate::jvm::ir_emit::LambdaMode::Indy,
        ) {
            Ok(methods) => methods,
            Err(()) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    "internal error: invalid declaration-owned delegate closure realization",
                );
                return Vec::new();
            }
        };
        let local_delegate_access = match crate::jvm::local_delegate_accessors::realize(
            &mut file.ir,
            current_source,
            file.stems,
            &file.classifiers,
        ) {
            Ok(access) => access,
            Err(()) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    "internal error: invalid JVM local delegated-property accessor plan",
                );
                return Vec::new();
            }
        };
        let mut property_reference_realizations = match crate::jvm::property_references::realize(
            &mut file.ir,
            file.stems,
            &file.callables,
            &facade,
            &local_delegate_access,
        ) {
            Ok(realizations) => realizations,
            Err(target) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!(
                        "internal error: missing JVM property-reference realization for {target:?}"
                    ),
                );
                return Vec::new();
            }
        };
        lambda_methods.include_owned_methods(&file.ir);
        // A checked annotation constructor names the semantic annotation declaration. The JVM
        // realizes it as a generated concrete implementation before ordinary dependency
        // constructors are assigned physical descriptors/default stubs.
        crate::jvm::annotation_constructions::lower_annotation_constructions(&mut file.ir, &facade);
        let mut default_call_operands =
            crate::jvm::default_call_operands::DefaultCallOperands::default();
        let mut property_realizations =
            crate::jvm::property_realizations::PropertyRealizations::default();
        if let Err(target) =
            crate::jvm::local_properties::realize(&mut file.ir, &mut property_realizations)
        {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot realize local JVM property access for {target:?}"),
            );
            return Vec::new();
        }
        if let Err(target) = crate::jvm::module_calls::realize(
            &mut file.ir,
            file.stems,
            &file.classifiers,
            &file.callables,
            &mut property_realizations,
            self.jvm_default,
        ) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: missing JVM module layout for {target:?}"),
            );
            return Vec::new();
        }
        if let Err(error) = crate::jvm::external_calls::realize(
            &mut file.ir,
            &file.classifiers,
            &self.cp,
            &file.callables,
            &mut default_call_operands,
        ) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: missing JVM dependency realization for {error}"),
            );
            return Vec::new();
        }
        crate::jvm::property_references::place_delegated_arrays(
            &mut file.ir,
            &mut property_reference_realizations,
            &*self.cp,
        );
        self.emit_streamed_ir(
            file,
            property_realizations,
            BackendPassFacts {
                property_reference_realizations,
                default_call_operands,
                local_delegate_access,
                lambda_methods,
                jvm_default: self.jvm_default,
                ..BackendPassFacts::default()
            },
            state,
            diags,
        )
    }

    fn finalize(&self, state: JvmState, module_name: &str) -> Vec<Artifact> {
        // META-INF/<module>.kotlin_module — maps packages to their file-facade classes so Kotlin
        // consumers can resolve top-level declarations from the compiled module.
        // kotlinc writes the module file even for a CLASS-ONLY module (empty parts list), so emit
        // it unconditionally — omitting it byte-diverges the artifact set from the reference
        // compiler.
        let packages: Vec<(String, Vec<String>)> = state.module_packages.into_iter().collect();
        let module_bytes = crate::metadata::module::build_kotlin_module(
            &packages,
            self.metadata_version
                .unwrap_or(crate::jvm::ir_emit::DEFAULT_METADATA_VERSION),
        );
        vec![(
            crate::metadata::module::kotlin_module_file_name(module_name),
            module_bytes,
        )]
    }
}

/// Build file-facade metadata from common IR alone. Semantic declaration records were copied from
/// finalized Pass-1 headers before bodies streamed; this step combines them only with physical
/// function names/descriptors chosen by JVM representation passes.
pub fn facade_package_metadata_from_ir(
    ir: &crate::ir::IrFile,
    (module_name, locals): (&str, &[LocalPropertyMeta]),
    param_assertions: bool,
    symbols: &dyn crate::backend::BackendClassifierSource,
    metadata_version: [i32; 3],
    annotations_in_metadata: bool,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    let functions = ir
        .package_functions
        .iter()
        .map(|declaration| {
            let physical = ir.functions.get(declaration.function as usize);
            let jvm_name = physical
                .filter(|function| function.name != declaration.name)
                .map(|function| function.name.clone());
            let physical_descriptor = if ir.vc_declared_sigs.contains_key(&declaration.function) {
                physical
                    .map(|function| {
                        crate::jvm::names::method_descriptor(&function.params, function.ret)
                    })
                    .expect("a value-class-rewritten package function has its IR realization")
            } else {
                declared_method_descriptor(declaration)
            };
            // Recorded exactly when a reader cannot rebuild the physical descriptor from the
            // declared types (kotlinc's `requiresFunctionSignature`).
            let jvm_desc = super::metadata_method_signatures::requires_function_signature(
                declaration.receiver,
                declaration
                    .params
                    .iter()
                    .enumerate()
                    .skip(declaration.context_count)
                    .map(
                        |(index, (_, ty))| match declaration.vararg_index == Some(index) {
                            true => crate::metadata::vararg_recorded_type(*ty),
                            false => *ty,
                        },
                    ),
                declaration.ret,
                &physical_descriptor,
                &Default::default(),
            )
            .then_some(physical_descriptor);
            crate::metadata::builder::FnMeta {
                jvm_desc,
                jvm_name,
                ..crate::metadata::declaration_records::package_function(ir, declaration)
            }
        })
        .collect::<Vec<_>>();

    // A declared accessor's realized name, which `@JvmName` may have changed.
    let accessor_jvm_name = |function: Option<u32>| {
        function.map(|function| ir.functions[function as usize].name.clone())
    };
    let properties = ir
        .package_properties
        .iter()
        .map(|declaration| {
            // Which accessors the facade really declares, declared or compiler-default: a private
            // property with default accessors has none, and kotlinc's `JvmPropertySignature` then
            // names neither.
            let (has_getter, has_setter, getter_function, setter_function) = match ir
                .local_property_layouts
                .get(&declaration.property)
            {
                Some(crate::ir::IrLocalPropertyLayout::TopLevelStorage {
                    storage,
                    getter,
                    setter,
                    ..
                }) => (
                    getter.is_some() || ir.has_jvm_default_static_getter(*storage),
                    setter.is_some() || ir.has_jvm_default_static_setter(*storage),
                    *getter,
                    *setter,
                ),
                Some(crate::ir::IrLocalPropertyLayout::TopLevelAccessor {
                    getter, setter, ..
                }) => (true, setter.is_some(), Some(*getter), *setter),
                Some(
                    crate::ir::IrLocalPropertyLayout::Member { .. }
                    | crate::ir::IrLocalPropertyLayout::MemberExtension { .. },
                )
                | None => (true, declaration.mutable, None, None),
            };
            let companion = declaration.is_companion_extension();
            let field = property_field(ir, declaration);
            let accessor_parameters = declaration
                .context_parameters
                .iter()
                .copied()
                .chain(declaration.receiver.filter(|_| !companion))
                .collect::<Vec<_>>();
            let descriptor_parameters = accessor_parameters
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>();
            let ty_descriptor = crate::jvm::names::type_descriptor(declaration.ty);
            // A value class is stored as its carrier. The getter kotlinc records, and the field
            // descriptor a reader cannot derive from `S`, both name that carrier.
            let value_descriptor =
                erased_value_class_descriptor(ir, declaration).unwrap_or(ty_descriptor);
            let getter = has_getter.then(|| {
                (
                    accessor_jvm_name(getter_function).unwrap_or_else(|| {
                        crate::jvm::names::property_getter_name(&declaration.name)
                    }),
                    format!("({descriptor_parameters}){value_descriptor}"),
                )
            });
            let setter = has_setter.then(|| {
                let mut parameters = descriptor_parameters;
                parameters.push_str(&value_descriptor);
                (
                    accessor_jvm_name(setter_function).unwrap_or_else(|| {
                        crate::jvm::names::property_setter_name(&declaration.name)
                    }),
                    format!("({parameters})V"),
                )
            });
            crate::metadata::builder::PropMeta {
                getter,
                setter,
                field_name: field.as_ref().and_then(|(name, _)| name.clone()),
                field_desc: field.map(|(_, descriptor)| descriptor).filter(|physical| {
                    super::metadata_method_signatures::requires_field_signature(
                        declaration.ty,
                        physical,
                        &Default::default(),
                    )
                }),
                ..crate::metadata::declaration_records::package_property(
                    ir,
                    declaration,
                    setter_function,
                )
            }
        })
        .collect::<Vec<_>>();

    let aliases = ir
        .package_type_aliases
        .iter()
        .map(crate::metadata::declaration_records::package_alias)
        .collect::<Vec<_>>();
    let approximate = |ty| {
        crate::types::declaration_approximation(ty, &mut |classifier, index| {
            symbols
                .classifier(classifier)
                .and_then(|declaration| declaration.type_param_variances.get(index).copied())
        })
    };
    build_facade_metadata(
        functions,
        properties,
        aliases,
        (module_name, locals),
        param_assertions,
        Some(&approximate),
        metadata_version,
        annotations_in_metadata,
    )
}

/// The physical name and descriptor of the static field holding a delegated package property's
/// delegate.
/// The carrier descriptor of a package property whose storage was erased from a value class.
fn erased_value_class_descriptor(
    ir: &crate::ir::IrFile,
    declaration: &crate::ir::IrPackageProperty,
) -> Option<String> {
    let crate::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. } =
        ir.local_property_layouts.get(&declaration.property)?
    else {
        return None;
    };
    let field = ir.statics.get(*storage as usize)?;
    field
        .erased_declared_ty
        .is_some()
        .then(|| crate::jvm::names::type_descriptor(field.ty))
}

/// The facade field realizing a property, if any: a delegate's storage by its JVM name, or a
/// backing field (named by the property), with its physical descriptor.
fn property_field(
    ir: &crate::ir::IrFile,
    declaration: &crate::ir::IrPackageProperty,
) -> Option<(Option<String>, String)> {
    let (name, storage) = match ir.local_property_layouts.get(&declaration.property)? {
        crate::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. } => (None, *storage),
        crate::ir::IrLocalPropertyLayout::TopLevelAccessor {
            delegate: Some(storage),
            ..
        } => (
            Some(ir.static_field_jvm_name(*storage).to_string()),
            *storage,
        ),
        _ => return None,
    };
    let field = ir.statics.get(storage as usize)?;
    Some((name, crate::jvm::names::type_descriptor(field.ty)))
}

/// The JVM descriptor of a package function realized from its declaration: context parameters,
/// the extension receiver, the value parameters, and a suspend function's continuation.
fn declared_method_descriptor(declaration: &crate::ir::IrPackageFunction) -> String {
    let mut physical = declaration
        .params
        .iter()
        .map(|(_, parameter)| *parameter)
        .collect::<Vec<_>>();
    // A companion extension's receiver is recorded but has no JVM parameter.
    if let Some(receiver) = declaration.receiver.filter(|_| !declaration.companion) {
        physical.insert(declaration.context_count.min(physical.len()), receiver);
    }
    let mut descriptor = physical
        .iter()
        .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
        .collect::<String>();
    if declaration.suspend {
        descriptor.push_str("Lkotlin/coroutines/Continuation;");
    }
    format!(
        "({descriptor}){}",
        if declaration.suspend {
            "Ljava/lang/Object;".to_owned()
        } else {
            crate::jvm::names::type_descriptor(declaration.ret)
        }
    )
}

fn build_facade_metadata(
    functions: Vec<crate::metadata::builder::FnMeta>,
    properties: Vec<crate::metadata::builder::PropMeta>,
    aliases: Vec<crate::metadata::builder::TypeAliasMeta>,
    (module_name, locals): (&str, &[LocalPropertyMeta]),
    param_assertions: bool,
    intersection_approximation: Option<&dyn Fn(crate::types::Ty) -> Option<crate::types::Ty>>,
    metadata_version: [i32; 3],
    annotations_in_metadata: bool,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    (!functions.is_empty() || !properties.is_empty() || !aliases.is_empty()).then(|| {
        let (d1_bytes, d2) =
            crate::metadata::builder::build_package_with_intersection_approximation(
                &functions,
                &properties,
                &aliases,
                ((module_name != "main").then_some(module_name), locals),
                param_assertions,
                intersection_approximation,
                annotations_in_metadata,
            );
        crate::jvm::ir_emit::KotlinMetadata {
            k: 2,
            mv: metadata_version.to_vec(),
            xi: 48,
            d1: crate::metadata::encoding::bytes_to_strings(&d1_bytes),
            d2,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::frontend::{collect_signatures, parse_source_with_detected_features};

    struct NoClassifierFacts;

    impl crate::backend::BackendClassifierSource for NoClassifierFacts {
        fn classifier(
            &self,
            _classifier: crate::types::TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            None
        }
    }

    /// Every caller supplies a logical source stem, but module/corpus callers can retain directories
    /// that the CLI has already stripped. The shared constructor must own that normalization so all
    /// emitted `SourceFile` attributes contain the JVM-required simple filename on either path style.
    #[test]
    fn shipping_emit_options_normalize_logical_source_paths() {
        let cp = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let unix = shipping_emit_options("suite/nested/Foo", "main", None, cp.clone());
        let windows = shipping_emit_options("suite\\nested\\Bar", "main", None, cp);

        assert_eq!(unix.source_file.as_deref(), Some("Foo.kt"));
        assert_eq!(windows.source_file.as_deref(), Some("Bar.kt"));
    }

    /// These are not arbitrary emitter unit tests: each claims to compile or survey the bytes that
    /// krusty ships. Pin that architectural boundary so adding a new `EmitOptions` field cannot leave
    /// one of those pipelines on a subtly different artifact shape. A focused differential helper may
    /// mutate the returned options afterward (for example, force metadata despite a bisect env var),
    /// but it must still begin with the complete shared configuration.
    #[test]
    fn shipping_pipelines_do_not_reimplement_emit_options() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "src/bin/survey.rs",
            "tests/common/mod.rs",
            "tests/kotlin_box_ir_jvm_conformance.rs",
        ] {
            let text =
                std::fs::read_to_string(root.join(relative)).expect("read shipping pipeline");
            assert!(
                !text.contains("EmitOptions {"),
                "{relative} must start from jvm::backend::shipping_emit_options instead of duplicating the shipping configuration",
            );
        }
    }

    #[test]
    fn selected_dependency_callables_are_not_requeried_during_realization() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "src/jvm/external_calls.rs",
            "src/jvm/function_references.rs",
            "src/jvm/bridges.rs",
            "src/jvm/module_calls.rs",
        ] {
            let text = std::fs::read_to_string(root.join(relative))
                .expect("read dependency-callable realization pass");
            assert!(
                !text.contains(".external_callable("),
                "{relative} must consume CheckedBackendCallables by ExternalCallableId"
            );
        }
    }

    #[test]
    fn selected_dependency_properties_are_not_requeried_during_realization() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "src/jvm/external_calls.rs",
            "src/jvm/property_references.rs",
        ] {
            let text = std::fs::read_to_string(root.join(relative))
                .expect("read dependency-property realization pass");
            assert!(
                !text.contains(".external_property("),
                "{relative} must consume CheckedBackendCallables by ExternalPropertyId"
            );
        }
    }

    #[test]
    fn prepare_module_symbols_records_cross_file_facades() {
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(
                "package p\nfun helper(): String = \"OK\"\n\
                 inline operator fun String.unaryMinus(): String = this\n\
                 inline fun <reified T> spliceOnly(value: Any): T? = value as? T\n\
                 val answer: Int = 42",
                &mut diags,
            ),
            parse_source_with_detected_features(
                "package p\nfun box(): String = helper()",
                &mut diags,
            ),
        ];
        let stems = vec!["A".to_string(), "B".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);

        prepare_module_symbols(&files, &stems, &mut syms);

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(
            syms.fn_facades.get("helper").map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        assert_eq!(
            syms.prop_facades
                .get("answer")
                .map(|(facade, _, _, _)| facade.render()),
            Some("p/AKt".to_string())
        );
        let extension = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function)
                        if function.name == "unaryMinus" && function.receiver.is_some()
                )
            })
            .expect("source extension declaration");
        assert_eq!(
            syms.fn_facades_by_decl
                .get(&(0, extension.0))
                .map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        let splice_only = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function) if function.name == "spliceOnly"
                )
            })
            .expect("splice-only source declaration");
        assert!(
            syms.fn_facade_is_explicitly_unemitted(0, splice_only.0),
            "registration must preserve the negative outcome so checker-only absence is distinct"
        );
        assert!(!syms.fn_facades_by_decl.contains_key(&(0, splice_only.0)));
    }

    #[test]
    fn facade_metadata_uses_the_custom_setter_parameter_identity() {
        let mut ir = crate::ir::IrFile::default();
        let setter = ir.add_fun(crate::ir::IrFunction {
            name: "setTopLevel".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            setter,
            crate::ir::FnParamInfo::identities(vec![crate::ir::IrParameterIdentity::source(
                "replacement",
            )]),
        );
        let property_id = crate::fir::PropertyId::from_raw(0);
        let init = ir.add_expr(crate::ir::IrExpr::Const(crate::ir::IrConst::Int(0)));
        ir.statics.push(crate::ir::IrStatic {
            name: "topLevel".to_string(),
            ty: Ty::Int,
            init: Some(init),
            is_var: true,
            is_const: false,
            is_lateinit: false,
            owner: None,
            visibility: crate::types::Visibility::Public,
            setter_jvm_name: None,
            erased_declared_ty: None,
            accessors: crate::ir::IrStaticAccessors::ABSENT,
            line: 0,
            source_order: 0,
        });
        ir.local_property_layouts.insert(
            property_id,
            crate::ir::IrLocalPropertyLayout::TopLevelStorage {
                storage: 0,
                getter: None,
                setter: Some(setter),
                qualifier: None,
            },
        );
        ir.package_properties.push(crate::ir::IrPackageProperty {
            property: property_id,
            name: "topLevel".to_string(),
            ty: Ty::Int,
            mutable: true,
            type_params: Vec::new(),
            receiver: None,
            context_parameters: Vec::new(),
            context_parameter_names: Vec::new(),
            context_parameter_kinds: Vec::new(),
            is_const: false,
            has_constant: true,
            visibility: crate::types::Visibility::Public,
            annotations: Box::new([]),
            flags: crate::fir::DeclarationFlags::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            has_backing_field: true,
            modifiers: crate::ir::IrPropertyModifiers {
                declared_setter: true,
                ..Default::default()
            },
            setter_visibility: crate::types::Visibility::Public,
            source_order: 0,
        });

        let metadata = facade_package_metadata_from_ir(
            &ir,
            ("main", &[]),
            true,
            &NoClassifierFacts,
            [2, 4, 0],
            true,
        )
        .expect("a package property requires facade metadata");
        let decoded = crate::jvm::metadata::decode_metadata(
            &metadata.d1,
            &metadata.d2,
            Some(metadata.k),
            "a/AKt",
            None,
            &[],
        )
        .expect("decode generated facade metadata");

        let [property] = decoded.package_properties.as_ref() else {
            panic!(
                "expected one package property, got {:?}",
                decoded.package_properties
            );
        };
        assert_eq!(property.name, "topLevel");
        assert!(property.is_var);
        assert_eq!(
            property.setter_parameter_name.as_deref(),
            Some("replacement")
        );
        assert_eq!(
            property.ret_class,
            Some(crate::types::type_name("kotlin/Int"))
        );
    }

    /// JVM post-lowering passes must run through `run_backend_passes`.
    #[test]
    fn backend_passes_are_only_called_via_run_backend_passes() {
        // token that marks a CALL of the pass → files allowed to contain it (the defining module's
        // internal/recursive uses, and the shared pipeline in this file).
        let rules: &[(&str, &[&str])] = &[
            (
                "realize_top_level_jvm_fields(",
                &["src/jvm/property_storage.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_companion_properties(",
                &["src/jvm/companion.rs", "src/jvm/backend.rs"],
            ),
            (
                "elide_default_property_stores(",
                &["src/jvm/property_storage.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_value_classes(",
                &["src/jvm/value_classes.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_suspend(",
                &["src/jvm/suspend.rs", "src/jvm/backend.rs"],
            ),
            (
                "finalize_suspend_bridges(",
                &["src/jvm/suspend/cps_bridges.rs", "src/jvm/backend.rs"],
            ),
            (
                "mark_must_inline_lambdas(",
                &[
                    "src/jvm/ir_emit/must_inline_lambdas.rs",
                    "src/jvm/backend.rs",
                ],
            ),
            (
                "reparent_lambda_impls(",
                &[
                    "src/jvm/ir_emit.rs",
                    "src/jvm/ir_emit/lambda_implementation_placement.rs",
                    "src/jvm/backend.rs",
                ],
            ),
            (
                "complete_enabled(",
                &["src/plugins/mod.rs", "src/jvm/backend.rs"],
            ),
            (
                "derive_bridges(",
                &["src/jvm/bridges.rs", "src/jvm/backend.rs"],
            ),
            (
                "mangle_internal_members(",
                &["src/jvm/internal_names.rs", "src/jvm/backend.rs"],
            ),
            ("collection_barriers::select(", &["src/jvm/backend.rs"]),
            (
                "check_before_result_coercion(",
                &["src/jvm/result_null_checks.rs", "src/jvm/backend.rs"],
            ),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for dir in ["src", "tests"] {
            visit(&root.join(dir), &mut |path, text| {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                for (token, allowed) in rules {
                    if text.contains(token) && !allowed.contains(&rel.as_str()) {
                        offenders.push(format!("{rel}: calls `{token}…)` directly"));
                    }
                }
            });
        }
        assert!(
            offenders.is_empty(),
            "backend passes must go through jvm::backend::run_backend_passes (so a new pass lands \
             in every pipeline by construction), but:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn common_lowering_leaves_jvm_storage_choices_to_backend() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        visit(&root.join("src/fir_lower"), &mut |path, text| {
            for forbidden in [
                "lower_companion_properties",
                "mark_jvm_companion_hoisted_static",
                "jvm_default",
                "fieldInitializerOptimization",
            ] {
                if text.contains(forbidden) {
                    offenders.push(format!("{}: {forbidden}", path.display()));
                }
            }
        });
        assert!(
            offenders.is_empty(),
            "common lowering must leave JVM storage choices to the backend:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn common_lowering_leaves_bridge_barriers_to_backends() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        visit(&root.join("src/fir_lower"), &mut |path, text| {
            for (line, source) in text.lines().enumerate() {
                if source.contains("barrier_plan") && !source.contains("barrier_plan: None") {
                    offenders.push(format!("{}:{}: {source}", path.display(), line + 1));
                }
            }
        });
        assert!(
            offenders.is_empty(),
            "checked FIR lowering must not assign backend bridge barriers:\n{}",
            offenders.join("\n")
        );
    }

    fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                visit(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    f(&p, &text);
                }
            }
        }
    }
}
