//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use krusty::backend::{
    Artifact, Backend, BackendClassifierSource, BackendModuleFacts, CheckedBackendClassifiers,
};
use krusty::compilation_target::CompilationTarget;
use krusty::diag::DiagSink;
use krusty::jvm::names::type_descriptor;
use krusty::metadata::local_properties::LocalPropertyMeta;
use krusty::types::{type_name, Ty};

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
    /// A runtime declaration a plugin's generated code calls has no frozen JVM realization.
    PluginRuntimeCalls(String),
    /// A checked declaration argument no longer matched its selected JVM parameter boundary.
    CallArguments(String),
    /// A selected overridden call could not be joined to its checked dispatch classifier facts.
    MemberDispatch(krusty::types::TypeName),
    /// A lambda argument of an inline call carried no published parameter modifier and declared
    /// type, so whether the call inlines it is unknown.
    InlineParameters,
    /// A read-only collection superclass lacks the exact semantic declaration its stubs need.
    ReadOnlyCollectionStubs(String),
    /// A collection class's supertype lacks the declaration facts its `toArray` members need.
    CollectionToArray(String),
    /// An abstract builtin member lacks the source-superclass facts its materialization needs.
    AbstractBuiltinOverrides(String),
    /// A `suspend inline` callee's compiled body does not say where it places a suspend lambda
    /// passed to it: in the caller's frame, or in an object it constructs.
    SuspendLambdaPlacement(String),
}

/// What the plugin pass of [`run_backend_passes`] runs: the native plugins the frontend ran for this
/// compilation, and the module name their output is mangled with.
pub(crate) struct BackendPassPlugins<'a> {
    pub(crate) native_plugins: &'a krusty::plugins::registry::NativePlugins,
    pub(crate) module_name: &'a str,
}

/// JVM-only products accumulated by the post-lowering representation pipeline and consumed by
/// emission.
#[derive(Default)]
pub(crate) struct BackendPassFacts {
    continuation_metadata: crate::suspend::ContinuationMetadataMap,
    /// Suspend functions whose state machine is built during emission because their only suspension
    /// lives inside a body the emitter splices.
    emit_time_machines: crate::suspend::EmitTimeMachines,
    /// Physical returns that preserve `COROUTINE_SUSPENDED` and otherwise answer `Unit`.
    suspended_result_returns: crate::suspend::SuspendedResultReturns,
    intrinsic_probe_continuations: crate::suspend::IntrinsicProbeContinuations,
    /// Suspend-lambda classes whose physical names depend on final caller placement.
    specialized_suspend_lambda_classes: crate::suspend::SpecializedLambdaClasses,
    default_call_operands: crate::default_call_operands::DefaultCallOperands,
    bridge_adaptations: crate::bridge_adaptations::BridgeAdaptations,
    /// The bridges that take `FunctionN.invoke`'s packed argument array.
    function_argument_arrays: crate::function_argument_arrays::FunctionArgumentArrays,
    /// The overrides whose scalar JVM result is realized as its wrapper.
    override_results: crate::override_results::OverrideResults,
    /// Neutral-result guards on collection overrides whose descriptor needs no bridge.
    collection_method_entry_barriers: crate::collection_barriers::MethodEntryBarriers,
    /// JVM-only collection members: bridges for mapped collection declarations inherited from a
    /// Java superclass, and throwing stubs for a read-only collection face's mutable members.
    collection_members: crate::read_only_collection_stubs::CollectionMemberPlans,
    /// What the property-reference pass selected for each synthesized reference class. The
    /// value-class pass consumes and extends it; nothing recovers these answers from a spelling.
    property_reference_realizations: crate::property_references::PropertyReferenceRealizations,
    /// Physical constructions selected for Kotlin function-value SAM wrappers.
    sam_wrapper_realizations: crate::sam_wrappers::SamWrapperRealizations,
    local_delegate_access: crate::local_delegate_accessors::HelperAccess,
    /// Final implementation ordinals of top-level lambdas whose companion initializer was moved
    /// into its enclosing class. Lifted naming consumes this explicit placement fact so a nested
    /// callable keeps its moved parent's physical path.
    physical_top_lambda_ordinals: crate::lifted_names::PhysicalTopLambdaOrdinals,
    /// The lifted names of callables declared within realized lambda classes.
    lambda_class_scoped_names: crate::lifted_names::LambdaClassScopedNames,
    lambda_methods: crate::lambda_classes::LambdaMethods,
    /// Checked property operations realized before representation passes. Companion hoisting
    /// reads it to attach a private accessor to the operation that moves into `<clinit>`.
    property_realizations: crate::property_realizations::PropertyRealizations,
    super_accessors: crate::module_calls::SuperAccessors,
    /// The `-jvm-default` mode, which decides the forwarders (and their bridges) a class writes for
    /// the interface defaults it inherits.
    jvm_default: crate::ir_emit::JvmDefaultMode,
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
/// 3. `lower_companion_properties`, then `place_delegated_arrays` — realize supported companion
///    backing fields as JVM outer statics, then allocate each generated delegated-property array
///    against that complete owner-static inventory.
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
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected), then
///    allocate hoisted companion-field names from their final physical descriptors.
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
    ir: &mut krusty::ir::IrFile,
    target: BackendPassTarget<'_>,
    plugins: BackendPassPlugins<'_>,
    callables: &mut krusty::backend::CheckedBackendCallables,
    file_classes: &crate::file_facades::FileClasses,
    facts: &mut BackendPassFacts,
) -> Result<(), SkipReason> {
    let BackendPassTarget {
        classifiers,
        classpath,
        ..
    } = target;
    let facades = &file_classes.containers[..];
    let lowered_expressions = ir.exprs.len();
    krusty::plugins::complete_enabled(
        ir,
        plugins.native_plugins,
        plugins.module_name,
        jvm_plugin_type_descriptor,
        classifiers,
    );
    crate::local_class_names::realize_copied_lambda_class_names(ir, |source| {
        crate::module_calls::facade_for(source, facades)
            .expect("a local classifier's declaring source has a file stem")
    });
    // The JVM publishes members kotlinc's plugins generate only in IR (serialization's
    // `write$Self`); they join each class's record before any lowering rewrites one.
    krusty::metadata::class_declarations::add_target_generated_functions(ir);
    // Plugins run after the frontend/backend handoff and may append checked calls selected by an
    // exact dependency identity. Freeze those provider-normalized records now, once plugin output
    // is final and before any realization consumes them. Existing facts remain the original copy.
    callables
        .freeze_plugin_super_callables(ir, |identity| classpath.external_callable(identity))
        .map_err(|_| SkipReason::SuperCalls)?;
    // A plugin's generated code calls the runtime declarations it selected at the handoff by their
    // identities; they become JVM calls here, exactly like the file's own dependency calls did.
    crate::external_calls::realize_plugin_runtime_calls(
        ir,
        classifiers,
        classpath,
        callables,
        &mut facts.default_call_operands,
        lowered_expressions,
    )
    .map_err(|error| SkipReason::PluginRuntimeCalls(error.to_string()))?;
    run_backend_passes_after_plugins(
        ir,
        target,
        plugins.module_name,
        callables,
        Some(file_classes),
        facts,
    )
}

/// What every JVM backend pass over one file reads: the file's facade class, the checked
/// classifiers and classpath it was analyzed against, and the selected lambda realizations.
#[derive(Clone, Copy)]
pub(crate) struct BackendPassTarget<'a> {
    pub(crate) facade: &'a str,
    pub(crate) classifiers: &'a CheckedBackendClassifiers<'a>,
    pub(crate) classpath: &'a krusty::jvm::classpath::Classpath,
    pub(crate) lambda_modes: crate::ir_emit::LambdaModes,
}

fn run_backend_passes_after_plugins(
    ir: &mut krusty::ir::IrFile,
    target: BackendPassTarget<'_>,
    module_name: &str,
    callables: &krusty::backend::CheckedBackendCallables,
    file_classes: Option<&crate::file_facades::FileClasses>,
    facts: &mut BackendPassFacts,
) -> Result<(), SkipReason> {
    let BackendPassTarget {
        facade,
        classifiers,
        classpath,
        lambda_modes,
    } = target;
    let facades = file_classes.map(|classes| &classes.containers[..]);
    // Every later pass reads a type parameter's JVM carrier off its occurrences. Select it once,
    // from the declarations' complete bounds, before any of them runs.
    crate::enclosing_capture_storage::realize(ir).map_err(SkipReason::CallArguments)?;
    crate::type_parameter_carriers::realize(ir);
    let module_value_classes = classifiers.module().source_value_classes();
    let module_readable_value_classes = classifiers.module().metadata_readable_value_classes();
    // Plugins produce backend-neutral checked IR. Realize any semantic super dispatch they add at
    // the same JVM boundary as source super calls, never in the plugin itself or the emitter.
    crate::module_calls::realize_super_calls(
        ir,
        classifiers,
        callables,
        &mut facts.property_realizations,
        &mut facts.super_accessors,
        facts.jvm_default,
    )
    .map_err(|_| SkipReason::SuperCalls)?;
    crate::annotation_constructions::lower_annotation_constructions(ir, facade);
    crate::platform_visible_constructors::realize(ir);
    // A property's own annotations become a synthetic marker method — a JVM realization of a Kotlin
    // declaration that has no class-file form. Before the value-class pass, which renames a marker
    // together with its mangled getter.
    crate::property_annotations::synthesize_property_annotation_markers(ir);
    // A package property remains a semantic declaration plus a checked common-IR layout. Join the
    // stable identities here, where `@JvmField` becomes a JVM storage decision.
    crate::property_storage::realize_top_level_jvm_fields(ir);
    // Some early representation decisions need to know whether an exact checked classifier is a
    // value class. Publish that declaration inventory before the first such decision; later
    // representation passes still own erasure, boxing and mangling.
    if !crate::value_classes::record_referenced_value_classes(ir, classifiers) {
        return Err(SkipReason::ValueClasses);
    }
    // A singleton or constant delegate is evaluated in place, with no `x$delegate` storage. Before
    // companion hoisting and delegate-field naming, which then see only the storage that remains.
    crate::in_place_delegates::realize(ir);
    // Companion backing-field hoisting is a JVM storage choice. Common IR retains the ordinary
    // property declaration and semantic initializer; this pass selects the outer-static realization.
    crate::companion::lower_companion_properties(
        ir,
        &facts.property_realizations,
        &mut facts.physical_top_lambda_ordinals,
    );
    // A generated delegated-property array shares its owner's static namespace with companion
    // backing fields. Allocate and rewrite its operands only after hoisting has made that namespace
    // complete, while every generated element still participates in the representation passes
    // below.
    crate::property_references::place_delegated_arrays(
        ir,
        &mut facts.property_reference_realizations,
        classpath,
    );
    // Delegated properties that share a source name (extension properties for different
    // receivers) each lower a `{name}$delegate` static. The JVM name is unique per owner:
    // the first keeps that spelling and each later one takes the next `$N` suffix.
    crate::property_storage::realize_delegate_static_field_names(ir);
    // Kotlin parameter nullability is already fixed in common IR. Select the JVM's entry-guard
    // realization before generic/value-class erasure changes the physical parameter types; those
    // later representation passes may then remove a guard whose carrier becomes primitive.
    crate::parameter_assertions::realize(ir);
    // Common IR retains source type-parameter identities and complete intersections. Select the JVM
    // class-bound erasure here, once, before any descriptor-sensitive backend pass runs.
    crate::reified_operations::realize(ir, &facts.lambda_methods);
    // A null check over an erased call result reads the call's own slot, ahead of the coercion
    // that narrows it; erasure and call-result boundaries below rewrite that coercion.
    crate::result_null_checks::check_before_result_coercion(ir);
    // A conversion copied from another source file constructs that declaration file's public
    // inline SAM wrapper. Resolve the exact source-unit identity before wrapper realization; the
    // copy's inline provenance names the declaration and never reconstructs its facade spelling.
    if let Some(facades) = facades {
        crate::module_calls::resolve_foreign_template_facades(ir, facades);
    }
    // A checked Kotlin function-value conversion is represented by one file-owned wrapper class.
    // Create it before generic erasure and bridge/value-class realization process its generated
    // method, retaining nullable-construction sites only in JVM pass facts.
    let class_java_sam = lambda_modes.sam_conversions == crate::ir_emit::LambdaMode::Class;
    facts.sam_wrapper_realizations = crate::sam_wrappers::realize(ir, facade, class_java_sam);
    // A projected SAM conversion stays logically typed as the function it captures. Only the JVM
    // implementation method receives the declaration's erased slots, after class-mode wrappers
    // have consumed conversions that realize their own forwarding methods.
    crate::sam_projected_adapters::realize(ir);
    crate::generic_erasure::lower_function_type_parameters(ir);
    crate::deferred_local_storage::realize(ir);
    crate::call_result_boundaries::realize_call_result_boundaries(ir);
    // Bridges are a JVM realization of an override, derived here from the IR's own declarations and the
    // checker's supertype view. Runs BEFORE the barrier pass (which annotates existing bridges) and
    // before the value-class pass (which retargets them once mangled names are known).
    // A scalar result over a reference-returning overridden slot is the wrapper; its bridges follow.
    // A bridge to a value-class override asks which dependency classes are value classes.
    // Refresh the inventory for declarations synthesized since the first publication above.
    if !crate::value_classes::record_referenced_value_classes(ir, classifiers) {
        return Err(SkipReason::ValueClasses);
    }
    facts.override_results = crate::override_results::box_scalar_override_results(
        ir,
        callables,
        &facts.property_realizations,
    )?;
    facts.override_results.collection_signatures =
        crate::collection_override_signatures::select(ir, classpath);
    // An abstract class's materialized builtin members precede bridge derivation: their final
    // bridges are ones a subclass inherits rather than declares.
    let abstract_overrides = crate::abstract_builtin_overrides::select(ir, classpath, classifiers)?;
    crate::bridges::derive_bridges(
        ir,
        callables,
        &facts.override_results,
        &mut facts.function_argument_arrays,
        facts.jvm_default,
        &crate::bridges::SuperclassSources {
            classpath,
            module: classifiers.module(),
            abstract_overrides: &abstract_overrides,
        },
    )?;
    crate::collection_barriers::select(
        ir,
        &facts.override_results,
        &mut facts.collection_method_entry_barriers,
    );
    facts.collection_members = crate::read_only_collection_stubs::CollectionMemberPlans {
        inherited_bridges: crate::inherited_collection_bridges::select(
            ir,
            classpath,
            &facts.override_results,
        ),
        read_only_stubs: crate::read_only_collection_stubs::select(ir, classpath, classifiers)?,
        abstract_overrides,
        to_array: crate::collection_to_array::select(ir, classpath, classifiers)?,
    };
    // The value-class pass reads same-module SOURCE value classes (internal name → sole-field
    // underlying) for its erasure/mangle map from the frontend symbols directly, NOT through the
    // resolver's library view (which would change the checker's construction/member resolution for
    // source value classes).
    let inherited_default_statics = crate::value_classes::InheritedDefaultStatics::plan(
        ir,
        classifiers,
        callables,
        &facts.property_realizations,
        facts.jvm_default,
    );
    if !crate::value_classes::lower_value_classes(
        ir,
        classifiers,
        crate::value_classes::ModuleValueClasses {
            underlying: module_value_classes,
            readable: module_readable_value_classes,
        },
        &inherited_default_statics,
        &mut facts.bridge_adaptations,
        &mut facts.override_results,
        &mut facts.property_reference_realizations,
    ) {
        return Err(SkipReason::ValueClasses);
    }
    crate::companion::finalize_hoisted_static_field_names(
        ir,
        &facts.property_reference_realizations.delegated_arrays,
    );
    // Companion fields move before value-class erasure. A private property reference was realized
    // against the companion instance accessor; point it at the outer class's `access$…$cp` now
    // that the field's physical type is known.
    crate::property_references::retarget_hoisted_companion_references(
        ir,
        &mut facts.property_reference_realizations,
    );
    crate::default_parameter_representation::record_defaulted_primitive_bounds(ir);
    // A concatenation appends a `toString()` call's receiver itself, as kotlinc does once its
    // value-class lowering has turned a value class's `toString()` into a static call.
    crate::concatenated_to_string::flatten_to_string_operands(ir);
    // Every body of the file is lowered, so each lifting sequence is whole, and the value-class
    // pass has named the functions kotlinc names their lifted callables after: name those callables
    // before any pass renders a debug name from them.
    facts.lambda_class_scoped_names = crate::lifted_names::number(
        ir,
        &facts.local_delegate_access,
        &facts.physical_top_lambda_ordinals,
    );
    // Generic erasure and value-class projection have now fixed every declaration parameter's JVM
    // carrier. Consume and retarget the exact call-owned adapters before default/suspend/inline
    // transforms clone or wrap those calls; the provenance is a one-shot representation contract.
    crate::physical_call_arguments::retarget_declaration_arguments(ir)
        .map_err(SkipReason::CallArguments)?;
    if !facts.default_call_operands.synchronize(ir) {
        return Err(SkipReason::CallArguments(
            "default call operand plan no longer matches its call".to_string(),
        ));
    }
    // The JVM supplies default field values before any constructor runs. Elide only source
    // declaration stores recorded by exact ExprId; common IR and other targets keep them. Runs
    // after value-class lowering, so each field's type is the physical slot the JVM zero-fills.
    crate::property_storage::elide_default_property_stores(ir);
    if let Some(file_classes) = file_classes {
        crate::module_calls::realize_default_calls(
            ir,
            file_classes,
            &mut facts.default_call_operands,
        )
        .map_err(|_| SkipReason::DefaultCalls)?;
    }
    // kotlinc's `MappedEnumWhenLowering` runs before its coroutine lowering, which then sees only
    // the integer `when` it leaves.
    crate::enum_when_mappings::lower(ir);
    crate::shared_captures::lower_class_capture_slots(ir);
    // An inline expansion rewrites a type-parameter cell to the call-site element. A lambda that
    // still uses the shared erased implementation takes the erased holder, and suspension spilling
    // reads that holder from the cell, so the two have to agree before either pass.
    crate::shared_captures::restore_erased_escaping_capture_holders(ir);
    let null_out_dead_spills = crate::runtime_capabilities::null_out_spilled_variable(classpath);
    crate::suspend::lambda_placement::record(ir, classpath, &facts.default_call_operands)
        .map_err(SkipReason::SuspendLambdaPlacement)?;
    if !crate::suspend::lower_suspend(
        ir,
        facade,
        crate::suspend::SuspendLoweringFacts {
            continuation_metadata: &mut facts.continuation_metadata,
            default_call_operands: &mut facts.default_call_operands,
            emit_time_machines: &mut facts.emit_time_machines,
            suspended_result_returns: &mut facts.suspended_result_returns,
            intrinsic_probe_continuations: &mut facts.intrinsic_probe_continuations,
            specialized_lambda_classes: &mut facts.specialized_suspend_lambda_classes,
        },
        null_out_dead_spills,
    ) {
        return Err(SkipReason::Suspend);
    }
    crate::suspend::finalize_suspend_bridges(ir, &mut facts.bridge_adaptations);
    crate::reified_operations::record_lowered_classes(ir);
    // After the suspend transform: the body moved onto the static is the finished state machine.
    crate::suspend_impls::lower_suspend_impls(ir);
    // Source lambda names are inputs to lifting. Fix them before reparenting so the lifted-name
    // pass can retain the final caller-qualified path and ordinal.
    crate::debug_local_names::realize_source_lambda_implementation_names(ir);
    crate::ir_emit::mark_must_inline_lambdas(ir).map_err(|missing| {
        krusty::trace_compiler!(
            "splice",
            "inline call {} published no parameter facts for lambda operand {}",
            missing.call,
            missing.parameter
        );
        SkipReason::InlineParameters
    })?;
    crate::ir_emit::reparent_lambda_impls(ir);
    // After reparenting: a lifted name is distinct only within the class the method lands in.
    crate::lifted_names::realize(
        ir,
        &facts.override_results,
        &facts.lambda_class_scoped_names,
        &facts.emit_time_machines,
    );
    // After lifted names are fixed. The module suffix must not leak into `$lambda$N`: kotlinc
    // names those from the value-class spelling and only then appends `$<module>` to the member.
    crate::internal_names::mangle_internal_members(
        ir,
        module_name,
        facade,
        &facts.property_reference_realizations,
    );
    facts
        .super_accessors
        .finalize(ir)
        .map_err(|_| SkipReason::SuperCalls)?;
    // A delegated property's reflected signature names its declaration's physical getter. Its
    // exact FunId was retained when the reference was built; consume that identity only after
    // value-class and internal-member naming have fixed the accessor's final JVM ABI.
    facts
        .property_reference_realizations
        .finalize_delegated_getter_signatures(ir);
    // A specialized suspend lambda is already a real class when suspend lowering completes, but
    // its JVM name depends on the caller's final placement and lifted spelling. Realize that name
    // only now and keep the coroutine-emission facts keyed by the same physical identity.
    facts.specialized_suspend_lambda_classes.realize(
        ir,
        facade,
        lambda_modes,
        &mut facts.emit_time_machines,
    );
    // A call-site-specialized lambda is already a function class. Its `$$inlined$` name depends
    // on the caller's lifted spelling, which exists only now.
    let closure_names =
        crate::lambda_classes::rename_specialized_lambda_classes(ir, facade, lambda_modes);
    crate::reified_anonymous::rename(
        ir,
        facade,
        lambda_modes,
        crate::reified_anonymous::Continuations {
            machines: &mut facts.emit_time_machines,
            metadata: &mut facts.continuation_metadata,
        },
    );
    facts
        .property_reference_realizations
        .remap_delegated_owners(&closure_names);
    // With placement and lifted caller names final, realize JVM-only implementation spellings.
    crate::debug_local_names::realize_lambda_implementation_names(
        ir,
        facade,
        lambda_modes,
        &facts.specialized_suspend_lambda_classes,
    );
    crate::overridden_calls::realize(ir, classifiers, callables, &facts.property_realizations)
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
    cp: std::rc::Rc<krusty::jvm::classpath::Classpath>,
    /// Class-file major version to emit (`-jvm-target`), or `None` for krusty's default (v52).
    class_major: Option<u16>,
    jvm_default: crate::ir_emit::JvmDefaultMode,
    java_parameters: bool,
    lambda_modes: crate::ir_emit::LambdaModes,
    /// `-Xstring-concat=inline`: build string concatenations with `StringBuilder` at every target.
    inline_string_concat: bool,
    /// `-Xuse-inline-scopes-numbers`: name inlined locals by their inline scope's number.
    inline_scopes_numbers: bool,
    /// Whether to emit the `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions`
    /// clears this).
    param_assertions: bool,
    /// Whether to emit the `Intrinsics.checkNotNullExpressionValue` guard on a narrowed platform
    /// value (`-Xno-call-assertions` clears this).
    call_assertions: bool,
    /// Whether to emit that guard on a platform value that reaches an extension receiver
    /// (`-Xno-receiver-assertions` clears this).
    receiver_assertions: bool,
    /// `-language-version X.Y`: the `@kotlin.Metadata` `mv` and `.kotlin_module` header version to
    /// stamp; `None` keeps the default ([`crate::ir_emit::DEFAULT_METADATA_VERSION`]).
    metadata_version: Option<[i32; 3]>,
    /// Whether the source-language configuration enables kotlinc's
    /// `LanguageFeature.AnnotationsInMetadata`. This is deliberately independent of the physical
    /// metadata stamp: an internal stamp override must not change source-language semantics.
    annotations_in_metadata: bool,
    /// Whether the language settings are pre-release, so every `@kotlin.Metadata` carries
    /// kotlinc's pre-release flag.
    pre_release_metadata: bool,
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<krusty::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend {
            cp,
            class_major: None,
            jvm_default: crate::ir_emit::JvmDefaultMode::default(),
            java_parameters: false,
            lambda_modes: crate::ir_emit::LambdaModes::default(),
            inline_string_concat: false,
            inline_scopes_numbers: false,
            param_assertions: true,
            call_assertions: true,
            receiver_assertions: true,
            metadata_version: None,
            annotations_in_metadata: true,
            pre_release_metadata: false,
        }
    }

    /// `-language-version X.Y` stamps every `@kotlin.Metadata` `mv` and the `.kotlin_module`
    /// header with `[X, Y, 0]`; `None` keeps kotlinc's no-flag stamp.
    pub fn with_metadata_version(mut self, version: Option<[i32; 3]>) -> JvmBackend {
        self.metadata_version = version;
        self
    }

    /// Mark every `@kotlin.Metadata` as pre-release, as kotlinc does when
    /// `LanguageVersionSettings.isPreRelease` holds for the compilation.
    pub fn with_pre_release_metadata(mut self, pre_release: bool) -> JvmBackend {
        self.pre_release_metadata = pre_release;
        self
    }

    /// The version and pre-release flag of every `@kotlin.Metadata` this backend writes.
    fn metadata_stamp(&self) -> krusty::jvm::classfile::MetadataStamp {
        krusty::jvm::classfile::MetadataStamp {
            version: self
                .metadata_version
                .unwrap_or(crate::ir_emit::DEFAULT_METADATA_VERSION),
            pre_release: self.pre_release_metadata,
        }
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

    /// `-Xno-receiver-assertions` passes `false`: emit no such guard where the narrowed value is an
    /// extension call's receiver.
    pub fn with_receiver_assertions(mut self, enabled: bool) -> JvmBackend {
        self.receiver_assertions = enabled;
        self
    }

    /// `-jvm-default`: which JVM shape an interface's members with bodies are compiled into.
    pub fn with_jvm_default(mut self, mode: crate::ir_emit::JvmDefaultMode) -> JvmBackend {
        self.jvm_default = mode;
        self
    }

    /// `-java-parameters`: write a `MethodParameters` attribute naming each declared parameter.
    pub fn with_java_parameters(mut self, enabled: bool) -> JvmBackend {
        self.java_parameters = enabled;
        self
    }

    /// Independently select `-Xlambdas` and `-Xsam-conversions` realization strategies.
    pub fn with_lambda_modes(mut self, modes: crate::ir_emit::LambdaModes) -> JvmBackend {
        self.lambda_modes = modes;
        self
    }

    /// `-Xstring-concat=inline`: never build a string concatenation with `invokedynamic`.
    pub fn with_inline_string_concat(mut self, inline: bool) -> JvmBackend {
        self.inline_string_concat = inline;
        self
    }

    /// `-Xuse-inline-scopes-numbers`: name inlined locals by their inline scope's number.
    pub fn with_inline_scopes_numbers(mut self, enabled: bool) -> JvmBackend {
        self.inline_scopes_numbers = enabled;
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
pub use krusty::jvm::compilation_inputs::NO_CLASS_METADATA_VARIABLE;

pub fn shipping_emit_options(
    stem: &str,
    source_kind: krusty::frontend::SourceKind,
    module_name: &str,
    class_major: Option<u16>,
    cp: std::rc::Rc<krusty::jvm::classpath::Classpath>,
) -> crate::ir_emit::EmitOptions {
    // Module/conformance inputs may retain a source-relative prefix (`helpers/Foo`) even though the
    // CLI has already reduced its input to `Foo`. `SourceFile` is a simple filename on every JVM
    // class, so normalize at this shared boundary instead of making each non-CLI caller grow its own
    // path branch. Accept both separators because Kotlin testdata names are logical source paths and
    // are not guaranteed to use the host platform's separator.
    let source_stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    crate::ir_emit::EmitOptions {
        class_major,
        source_file: Some(format!(
            "{source_stem}.{}",
            match source_kind {
                krusty::frontend::SourceKind::KotlinScript => "kts",
                _ => "kt",
            }
        )),
        // kotlinc records `classModuleName` in @Metadata unless the module is the default `main`.
        module_name: (module_name != "main").then(|| module_name.to_string()),
        // Per-invocation strategies; the CLI overrides them on the backend.
        lambda_modes: crate::ir_emit::LambdaModes::default(),
        java_parameters: false,
        inline_string_concat: false,
        inline_scopes_numbers: false,
        // Compute + emit each class's own `@Metadata`. Without it a krusty-compiled CLASS is
        // unreadable BY KRUSTY: the facade metadata describes top-level declarations only, so a
        // second compilation sees no constructor/member parameter names (named arguments) and no
        // `operator` marks (destructuring). A shape `build_class_metadata` has not verified against
        // kotlinc declines individually and emits nothing, so this cannot write an unverified
        // payload. `KRUSTY_NO_CLASS_METADATA` restores the facade-only output for bisecting.
        emit_class_metadata: std::env::var_os(NO_CLASS_METADATA_VARIABLE).is_none(),
        jvm_default: crate::ir_emit::JvmDefaultMode::default(),
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
        pre_release_metadata: false,
        // The backend that ran the compilation's plugins installs them for the metadata writer.
        plugins: std::rc::Rc::default(),
        // The backend sets the facade of a multifile part.
        multifile_facade: None,
    }
}

fn checked_module_inner_class_resolver(
    module: &BackendModuleFacts,
    cp: std::rc::Rc<krusty::jvm::classpath::Classpath>,
) -> krusty::jvm::classfile::InnerClassResolver {
    module_inner_class_resolver_from_shapes(
        module
            .classifiers()
            .map(|(classifier, shape)| InnerModuleClassifier {
                classifier,
                declaration_name: shape.declaration_name.clone(),
                visibility: shape.access.visibility(),
                inner: shape.outer_instance.is_some(),
                annotation: shape.is_annotation(),
                interface: shape.is_interface(),
                enum_class: shape.is_enum(),
                abstract_class: shape.is_abstract,
                final_class: !shape.is_abstract && !shape.is_extensible,
            }),
        module.generated_classifiers().iter(),
        classpath_inner_class_resolver(cp),
    )
}

struct InnerModuleClassifier {
    classifier: krusty::types::TypeName,
    declaration_name: Option<krusty::types::ClassifierDeclarationName>,
    visibility: krusty::types::Visibility,
    inner: bool,
    annotation: bool,
    interface: bool,
    enum_class: bool,
    abstract_class: bool,
    final_class: bool,
}

fn module_inner_class_resolver_from_shapes<'a>(
    classes: impl IntoIterator<Item = InnerModuleClassifier>,
    generated: impl IntoIterator<Item = &'a krusty::types::GeneratedClassifierFact>,
    classpath: krusty::jvm::classfile::InnerClassResolver,
) -> krusty::jvm::classfile::InnerClassResolver {
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

    let mut source = krusty::fx_hash::HashMap::default();
    for class in classes {
        let internal = class.classifier.render();
        let (outer, simple) = match class.declaration_name {
            Some(krusty::types::ClassifierDeclarationName::Nested { owner, name }) => (owner, name),
            Some(krusty::types::ClassifierDeclarationName::TopLevel { .. }) | None => {
                // This compilation owns the declaration even when ownership is unavailable.
                // A classpath record with the same physical name cannot fill that gap.
                source.insert(internal, None);
                continue;
            }
        };
        let visibility = match class.visibility {
            krusty::types::Visibility::Protected => PROTECTED,
            krusty::types::Visibility::Private => PRIVATE,
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
            internal,
            Some(krusty::jvm::classfile::InnerClassDetails {
                outer: Some(outer.render()),
                name: Some(simple.to_string()),
                access,
            }),
        );
    }
    for class in generated {
        let visibility = match class.visibility {
            krusty::types::Visibility::Protected => PROTECTED,
            krusty::types::Visibility::Private => PRIVATE,
            _ => PUBLIC,
        };
        let mut access = visibility | if class.captures_outer { 0 } else { STATIC };
        match class.kind {
            krusty::types::GeneratedClassifierKind::Annotation => {
                access |= INTERFACE | ABSTRACT | ANNOTATION;
            }
            krusty::types::GeneratedClassifierKind::Interface => {
                access |= INTERFACE | ABSTRACT;
            }
            krusty::types::GeneratedClassifierKind::Enum => access |= ENUM,
            krusty::types::GeneratedClassifierKind::Class => {}
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
            Some(krusty::jvm::classfile::InnerClassDetails {
                outer: Some(class.lexical_owner.render()),
                name: Some(class.source_name.to_string()),
                access,
            }),
        );
    }
    std::rc::Rc::new(move |internal: &str| match source.get(internal) {
        Some(details) => details.clone(),
        None => classpath(internal),
    })
}

pub fn classpath_inner_class_resolver(
    cp: std::rc::Rc<krusty::jvm::classpath::Classpath>,
) -> krusty::jvm::classfile::InnerClassResolver {
    std::rc::Rc::new(move |internal: &str| {
        // The class file first — it is authoritative whenever the nested class has one. A mapped
        // builtin whose JVM class is absent (no JDK on the classpath) still declares the same nesting
        // in its `.kotlin_builtins` entry; without this fallback the reference emits no `InnerClasses`
        // attribute at all, so the JDK-less class file diverges from the JDK-present one.
        cp.find(internal)
            .and_then(|class| {
                let entry = class.inner_class_self()?;
                Some(krusty::jvm::classfile::InnerClassDetails {
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
                Some(krusty::jvm::classfile::InnerClassDetails {
                    outer: Some(outer),
                    name: Some(name),
                    access,
                })
            })
    })
}

/// package → file-facade class names, accumulated across files for the `.kotlin_module` mapping.
#[derive(Default)]
pub struct JvmState {
    module_packages: std::collections::BTreeMap<String, Vec<krusty::metadata::module::PackagePart>>,
    /// One compilation emits many files against one frozen module. The inner-class attribute map
    /// depends only on that module, so building it again on every file is quadratic.
    inner_class_resolver: Option<krusty::jvm::classfile::InnerClassResolver>,
    /// Each source's top-level class. Every file of the module reads every source's, so they are
    /// derived once.
    file_classes: Option<crate::file_facades::FileClasses>,
    /// The emitted multifile parts, by the package-qualified public facade they forward through.
    multifile_parts: crate::multifile_facades::MultifileParts,
}

impl JvmState {
    fn file_classes(
        &mut self,
        stems: &[String],
        module: &krusty::backend::BackendModuleFacts,
    ) -> crate::file_facades::FileClasses {
        self.file_classes
            .get_or_insert_with(|| crate::file_facades::FileClasses::of(stems, module))
            .clone()
    }
}

/// A facade's internal name: its simple name under the file's package (`p/q/FooKt`).
fn facade_class_name(segment: &str, package: &Option<String>) -> String {
    match package.as_deref() {
        Some(package) if !package.is_empty() => format!("{}/{segment}", package.replace('.', "/")),
        _ => segment.to_string(),
    }
}

/// A checked file after every JVM representation pass has selected its physical facts. Keeping the
/// handoff together makes the boundary explicit: emission consumes this closed product and must not
/// recover any of its decisions.
struct BackendReadyIr<'a> {
    ir: krusty::ir::IrFile,
    stem: &'a str,
    module_name: &'a str,
    facade_name: String,
    facade_class: krusty::types::TypeName,
    /// The public facade a multifile part is invoked through.
    multifile_facade: Option<String>,
    package: String,
    signature_symbols: &'a dyn BackendClassifierSource,
    dependency_callables: krusty::backend::CheckedBackendCallables,
    inner_class_resolver: krusty::jvm::classfile::InnerClassResolver,
    pass_facts: BackendPassFacts,
    metadata: Option<crate::ir_emit::KotlinMetadata>,
    has_facade_members: bool,
    native_plugins: &'a krusty::plugins::registry::NativePlugins,
}

impl JvmBackend {
    fn emit_streamed_ir(
        &self,
        file: krusty::backend::CheckedIrFile<'_>,
        file_classes: &crate::file_facades::FileClasses,
        property_realizations: crate::property_realizations::PropertyRealizations,
        mut pass_facts: BackendPassFacts,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let krusty::backend::CheckedIrFile {
            mut ir,
            kind: _,
            source,
            classifiers,
            mut callables,
            dependency_exports: _,
            native_plugins,
            module_name,
            stems,
            origins: _,
        } = file;
        let stem = &stems[source.raw() as usize];
        let package = ir.package.clone().unwrap_or_default();
        let facade_name =
            facade_class_name(&file_classes.containers[source.raw() as usize], &ir.package);
        let facade_class = krusty::types::type_name(&facade_name);
        let multifile_facade = file_classes.multifile_facades[source.raw() as usize]
            .as_deref()
            .map(|facade| facade_class_name(facade, &ir.package));
        pass_facts.property_realizations = property_realizations;
        if let Err(reason) = run_backend_passes(
            &mut ir,
            BackendPassTarget {
                facade: &facade_name,
                classifiers: &classifiers,
                classpath: &self.cp,
                lambda_modes: self.lambda_modes,
            },
            BackendPassPlugins {
                native_plugins,
                module_name,
            },
            &mut callables,
            file_classes,
            &mut pass_facts,
        ) {
            report_backend_pass_failure(reason, diags);
            return Vec::new();
        }
        if multifile_facade.is_some() {
            crate::multifile_facades::suffix_private_part_members(
                &mut ir,
                &facade_name,
                &mut pass_facts.continuation_metadata,
            );
        }
        if !pass_facts.default_call_operands.synchronize(&ir) {
            diags.error(
                krusty::diag::Span::new(0, 0),
                "default call operand plan no longer matches its call".to_string(),
            );
            return Vec::new();
        }
        if crate::declaration_collisions::validate(
            &ir,
            &pass_facts.override_results,
            &pass_facts.property_reference_realizations.delegated_arrays,
            diags,
        )
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
            self.metadata_stamp(),
            self.annotations_in_metadata,
        );
        // A multifile part's record is kotlinc's `MULTIFILE_CLASS_PART`.
        let metadata = metadata.map(|mut metadata| {
            if multifile_facade.is_some() {
                metadata.k = 5;
            }
            metadata
        });
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
                multifile_facade,
                package,
                signature_symbols: &classifiers,
                dependency_callables: callables,
                inner_class_resolver,
                pass_facts,
                metadata,
                has_facade_members,
                native_plugins,
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
            multifile_facade,
            package,
            signature_symbols,
            dependency_callables,
            inner_class_resolver,
            pass_facts,
            metadata,
            has_facade_members,
            native_plugins,
        } = ready;
        let mut outputs = Vec::new();
        if !self.param_assertions {
            crate::parameter_assertions::strip(&mut ir);
        }
        let omitted_null_assertions = if !self.call_assertions {
            Some(crate::implicit_null_assertions::OmittedNullAssertions::Implicit)
        } else if !self.receiver_assertions {
            Some(crate::implicit_null_assertions::OmittedNullAssertions::ExtensionReceivers)
        } else {
            None
        };
        if let Some(omitted) = omitted_null_assertions {
            crate::implicit_null_assertions::omit(&mut ir, omitted);
        }
        // Suspend lowering can move a catch onto a fresh `try`. Rebuild the marker plan from the
        // declaration parameters so emission names the handler that is actually written.
        crate::reified_operations::record_catch_markers(&mut ir, &pass_facts.lambda_methods);
        let source_kind = if ir.is_script() {
            krusty::frontend::SourceKind::KotlinScript
        } else {
            krusty::frontend::SourceKind::Kotlin
        };
        let mut emit_opts = shipping_emit_options(
            stem,
            source_kind,
            module_name,
            self.class_major,
            self.cp.clone(),
        )
        .with_jvm_default(self.jvm_default)
        .with_lambda_modes(self.lambda_modes)
        .with_inline_string_concat(self.inline_string_concat)
        .with_inline_scopes_numbers(self.inline_scopes_numbers)
        .with_param_assertions(self.param_assertions)
        .with_metadata_version(self.metadata_version)
        .with_annotations_in_metadata(self.annotations_in_metadata)
        .with_pre_release_metadata(self.pre_release_metadata)
        .with_java_parameters(self.java_parameters);
        emit_opts.inner_class_resolver = Some(inner_class_resolver);
        emit_opts.multifile_facade = multifile_facade.clone();
        emit_opts.plugins = std::rc::Rc::new(native_plugins.host(module_name));
        let run = crate::ir_emit::EmitRun::default();
        let emit_metadata = crate::ir_emit::EmitMetadata {
            facade: metadata.as_ref(),
            continuations: &pass_facts.continuation_metadata,
            emit_time_machines: &pass_facts.emit_time_machines,
            suspended_result_returns: &pass_facts.suspended_result_returns,
            intrinsic_probe_continuations: &pass_facts.intrinsic_probe_continuations,
            bridge_adaptations: &pass_facts.bridge_adaptations,
            function_argument_arrays: &pass_facts.function_argument_arrays,
            override_results: &pass_facts.override_results,
            collection_method_entry_barriers: &pass_facts.collection_method_entry_barriers,
            collection_members: &pass_facts.collection_members,
        };
        let classes = crate::ir_emit::emit_all_with_checked_classifiers(
            &ir,
            (facade_class, &facade_name),
            &*self.cp,
            crate::ir_emit::CheckedEmitFacts {
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
                    krusty::diag::Span::new(0, 0),
                    format!("krusty: JVM backend inline error: {reason}"),
                );
                return outputs;
            }
            if let Some(reason) = run.emit_error() {
                diags.error(krusty::diag::Span::new(0, 0), reason);
                return outputs;
            }
            diags.error(
                krusty::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        if let Some(multifile_facade) = &multifile_facade {
            // The methods the facade forwards, checked against the other parts' once every part
            // is emitted.
            let Ok(facade_slots) = crate::declaration_collisions::facade_slots(
                &ir,
                &pass_facts.override_results,
                diags,
            ) else {
                return outputs;
            };
            if let Some((_, part)) = classes
                .iter()
                .find(|(internal, _)| *internal == facade_name)
            {
                state.multifile_parts.record(
                    multifile_facade,
                    &facade_name,
                    part,
                    run.multifile_part.take(),
                    facade_slots,
                );
            }
        }
        for (internal, bytes) in classes {
            outputs.push((format!("{internal}.class"), bytes));
        }

        if has_facade_members {
            let simple_name =
                |internal: &str| internal.rsplit('/').next().unwrap_or(internal).to_string();
            state.module_packages.entry(package).or_default().push(
                krusty::metadata::module::PackagePart {
                    name: simple_name(&facade_name),
                    multifile_facade: multifile_facade.as_deref().map(simple_name),
                },
            );
        }
        outputs
    }
}

fn report_backend_pass_failure(reason: SkipReason, diags: &mut DiagSink) {
    match reason {
        SkipReason::DefaultCalls => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                "internal error: invalid checked default-argument realization".to_string(),
            );
            return;
        }
        SkipReason::SuperCalls => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                "internal error: invalid checked super-call realization".to_string(),
            );
            return;
        }
        SkipReason::PluginRuntimeCalls(detail) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!(
                    "internal error: missing JVM realization for a plugin runtime call {detail}"
                ),
            );
            return;
        }
        SkipReason::CallArguments(detail) => {
            diags.error(krusty::diag::Span::new(0, 0), detail);
            return;
        }
        SkipReason::InlineParameters => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                "internal error: an inline call's lambda argument has no published parameter modifier and declared type".to_string(),
            );
            return;
        }
        SkipReason::ReadOnlyCollectionStubs(detail) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("krusty: cannot realize read-only collection stubs: {detail}"),
            );
            return;
        }
        SkipReason::CollectionToArray(detail) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("krusty: cannot realize collection toArray members: {detail}"),
            );
            return;
        }
        SkipReason::AbstractBuiltinOverrides(detail) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("krusty: cannot realize abstract builtin overrides: {detail}"),
            );
            return;
        }
        SkipReason::SuspendLambdaPlacement(detail) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("krusty: cannot tell {detail}"),
            );
            return;
        }
        SkipReason::MemberDispatch(classifier) => {
            diags.error(
                krusty::diag::Span::new(0, 0),
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
        | SkipReason::PluginRuntimeCalls(_)
        | SkipReason::CallArguments(_)
        | SkipReason::InlineParameters
        | SkipReason::ReadOnlyCollectionStubs(_)
        | SkipReason::CollectionToArray(_)
        | SkipReason::AbstractBuiltinOverrides(_)
        | SkipReason::SuspendLambdaPlacement(_)
        | SkipReason::MemberDispatch(_) => {
            unreachable!()
        }
    };
    diags.error(
        krusty::diag::Span::new(0, 0),
        format!("krusty: this {what} shape is not yet supported by the IR backend"),
    );
}

impl Backend for JvmBackend {
    type State = JvmState;

    fn compilation_target(&self) -> CompilationTarget {
        CompilationTarget::Jvm
    }

    /// kotlinc compiles a same-module inline function to a method node before inlining it
    /// (`IrSourceCompilerForInline`), so its calls keep the callee's erased body.
    fn same_module_inlining(&self) -> krusty::ir::SameModuleInlining {
        krusty::ir::SameModuleInlining::CompiledBodies
    }

    fn lower_ir_file(
        &self,
        mut file: krusty::backend::CheckedIrFile<'_>,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let file_classes = state.file_classes(file.stems, file.classifiers.module());
        let facades = file_classes.containers.clone();
        let facade = facade_class_name(&facades[file.source.raw() as usize], &file.ir.package);
        let facade_class = type_name(&facade);
        crate::local_class_names::realize(&mut file.ir, |source| {
            crate::module_calls::facade_for(source, &facades)
                .expect("a local classifier's declaring source has a file stem")
        });
        if let Err(error) = crate::ranges::realize(&mut file.ir, self.cp.clone()) {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("internal error: cannot realize checked range operation: {error:?}"),
            );
            return Vec::new();
        }
        let spliced_getters = match crate::function_references::realize(
            &mut file.ir,
            &file.callables,
            crate::function_references::Facades {
                current: &facade,
                facades: &facades,
            },
        ) {
            Ok(spliced_getters) => spliced_getters,
            Err(target) => {
                diags.error(
                    krusty::diag::Span::new(0, 0),
                    format!(
                        "internal error: missing JVM function-reference realization for {target:?}"
                    ),
                );
                return Vec::new();
            }
        };
        // A lambda the metafactory cannot adapt is a class of its own; decided before any lambda
        // is numbered, as kotlinc numbers only the lambdas it lifts.
        let current_source = krusty::ir::IrModuleSource {
            source: file.source,
            package: facade_class.namespace(),
        };
        let delegate_closures = crate::local_delegate_closures::Requirements::collect(&file.ir);
        let mut lambda_methods = match crate::lambda_classes::realize(
            &mut file.ir,
            &file.classifiers,
            &facade,
            &delegate_closures,
            current_source,
            self.lambda_modes,
        ) {
            Ok(methods) => methods,
            Err(()) => {
                diags.error(
                    krusty::diag::Span::new(0, 0),
                    "internal error: invalid declaration-owned delegate closure realization",
                );
                return Vec::new();
            }
        };
        let local_delegate_access = match crate::local_delegate_accessors::realize(
            &mut file.ir,
            current_source,
            &facades,
            &file.classifiers,
        ) {
            Ok(access) => access,
            Err(()) => {
                diags.error(
                    krusty::diag::Span::new(0, 0),
                    "internal error: invalid JVM local delegated-property accessor plan",
                );
                return Vec::new();
            }
        };
        let property_reference_realizations = match crate::property_references::realize(
            &mut file.ir,
            &file_classes,
            &file.callables,
            &facade,
            &local_delegate_access,
            spliced_getters,
        ) {
            Ok(realizations) => realizations,
            Err(target) => {
                diags.error(
                    krusty::diag::Span::new(0, 0),
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
        crate::annotation_constructions::lower_annotation_constructions(&mut file.ir, &facade);
        let mut default_call_operands =
            crate::default_call_operands::DefaultCallOperands::default();
        let mut property_realizations =
            crate::property_realizations::PropertyRealizations::default();
        let mut super_accessors = crate::module_calls::SuperAccessors::default();
        if let Err(target) =
            crate::local_properties::realize(&mut file.ir, &mut property_realizations)
        {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("internal error: cannot realize local JVM property access for {target:?}"),
            );
            return Vec::new();
        }
        if let Err(target) = crate::module_calls::realize(
            &mut file.ir,
            &file_classes,
            &file.classifiers,
            &file.callables,
            &mut property_realizations,
            &mut super_accessors,
            self.jvm_default,
        ) {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("internal error: missing JVM module layout for {target:?}"),
            );
            return Vec::new();
        }
        if let Err(error) = crate::external_calls::realize(
            &mut file.ir,
            &file.classifiers,
            &self.cp,
            &file.callables,
            &mut default_call_operands,
        ) {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("internal error: missing JVM dependency realization for {error}"),
            );
            return Vec::new();
        }
        self.emit_streamed_ir(
            file,
            &file_classes,
            property_realizations,
            BackendPassFacts {
                super_accessors,
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

    fn check_module(&self, state: &JvmState, diags: &mut DiagSink) {
        let clashes = state.multifile_parts.clashes();
        if !clashes.is_empty() {
            for clash in clashes {
                diags.module_error(clash);
            }
            return;
        }
        if let Err(error) = state.multifile_parts.write(self.metadata_stamp()) {
            diags.error(
                krusty::diag::Span::new(0, 0),
                format!("internal error: {error}"),
            );
        }
    }

    fn finalize(&self, state: JvmState, module_name: &str) -> Vec<Artifact> {
        // META-INF/<module>.kotlin_module — maps packages to their file-facade classes so Kotlin
        // consumers can resolve top-level declarations from the compiled module.
        // kotlinc writes the module file even for a CLASS-ONLY module (empty parts list), so emit
        // it unconditionally — omitting it byte-diverges the artifact set from the reference
        // compiler.
        let packages: Vec<(String, Vec<krusty::metadata::module::PackagePart>)> =
            state.module_packages.into_iter().collect();
        let module_bytes = krusty::metadata::module::build_kotlin_module(
            &packages,
            self.metadata_version
                .unwrap_or(crate::ir_emit::DEFAULT_METADATA_VERSION),
        );
        let mut artifacts = match state.multifile_parts.write(self.metadata_stamp()) {
            Ok(facades) => facades
                .into_iter()
                .map(|(internal, bytes)| (format!("{internal}.class"), bytes))
                .collect(),
            // `check_module` reported the facade this module cannot write.
            Err(_) => Vec::new(),
        };
        artifacts.push((
            krusty::metadata::module::kotlin_module_file_name(module_name),
            module_bytes,
        ));
        artifacts
    }
}

/// Build file-facade metadata from common IR alone. Semantic declaration records were copied from
/// finalized Pass-1 headers before bodies streamed; this step combines them only with physical
/// function names/descriptors chosen by JVM representation passes.
pub fn facade_package_metadata_from_ir(
    ir: &krusty::ir::IrFile,
    (module_name, locals): (&str, &[LocalPropertyMeta]),
    param_assertions: bool,
    symbols: &dyn krusty::backend::BackendClassifierSource,
    metadata_stamp: krusty::jvm::classfile::MetadataStamp,
    annotations_in_metadata: bool,
) -> Option<crate::ir_emit::KotlinMetadata> {
    let local_classifiers = krusty::metadata::local_classifiers::names(ir);
    let enum_entry_bodies = krusty::metadata::local_classifiers::enum_entry_bodies(ir);
    let anonymous_classifiers = krusty::metadata::local_classifiers::anonymous_names(ir);
    let functions = ir
        .package_functions
        .iter()
        .map(|declaration| {
            let physical = ir.functions.get(declaration.function as usize);
            let jvm_name = physical
                .filter(|function| function.name != declaration.name)
                .map(|function| function.name.clone());
            let physical_descriptor = if ir.vc_declared_sigs.contains_key(&declaration.function)
                || ir
                    .plugin_extended_signatures
                    .contains(&declaration.function)
            {
                physical
                    .map(|function| {
                        krusty::jvm::method_descriptors::ir_method_desc(
                            &function.params,
                            &function.ret,
                        )
                    })
                    .expect("a rewritten package function has its IR realization")
            } else {
                declared_method_descriptor(declaration)
            };
            // Recorded exactly when a reader cannot rebuild the physical descriptor from the
            // declared types (kotlinc's `requiresFunctionSignature`).
            let jvm_desc = (super::metadata_method_signatures::requires_function_signature(
                declaration.receiver,
                declaration
                    .params
                    .iter()
                    .enumerate()
                    .skip(declaration.context_count)
                    .map(
                        |(index, (_, ty))| match declaration.vararg_index == Some(index) {
                            true => krusty::metadata::vararg_recorded_type(*ty),
                            false => *ty,
                        },
                    ),
                declaration.ret,
                &physical_descriptor,
                &local_classifiers,
            ) || declaration.spellings.names_actualized_expect())
            .then_some(physical_descriptor);
            krusty::metadata::builder::FnMeta {
                jvm_desc,
                jvm_name,
                ..krusty::metadata::declaration_records::package_function(ir, declaration)
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
            let (has_getter, has_setter, getter_function, setter_function, storage) =
                match ir.local_property_layouts.get(&declaration.property) {
                    Some(krusty::ir::IrLocalPropertyLayout::TopLevelStorage {
                        storage,
                        getter,
                        setter,
                        ..
                    }) => (
                        getter.is_some() || ir.has_jvm_default_static_getter(*storage),
                        setter.is_some() || ir.has_jvm_default_static_setter(*storage),
                        *getter,
                        *setter,
                        Some(*storage),
                    ),
                    Some(krusty::ir::IrLocalPropertyLayout::TopLevelAccessor {
                        getter,
                        setter,
                        ..
                    }) => (true, setter.is_some(), Some(*getter), *setter, None),
                    Some(
                        krusty::ir::IrLocalPropertyLayout::Member { .. }
                        | krusty::ir::IrLocalPropertyLayout::MemberExtension { .. },
                    )
                    | None => (true, declaration.mutable, None, None, None),
                };
            let companion = declaration.is_companion_extension();
            let field = property_field(ir, declaration);
            // The accessors' descriptors erase the property's type parameters to their primary
            // bounds, as the accessor methods themselves do.
            let mut accessor_types = declaration
                .context_parameters
                .iter()
                .copied()
                .chain(declaration.receiver.filter(|_| !companion))
                .collect::<Vec<_>>();
            accessor_types.push(declaration.ty);
            if let Some(getter) = getter_function {
                accessor_types =
                    crate::generic_erasure::erased_for_function(ir, getter, &accessor_types);
            }
            let erased_ty = accessor_types
                .pop()
                .expect("the property type was pushed last");
            let descriptor_parameters = accessor_types
                .iter()
                .map(|parameter| {
                    krusty::jvm::names::type_descriptor(
                        krusty::jvm::method_descriptors::jvm_value_ty(parameter),
                    )
                })
                .collect::<String>();
            let ty_descriptor = krusty::jvm::names::type_descriptor(
                krusty::jvm::method_descriptors::jvm_declared_ty(&erased_ty),
            );
            // A value class is stored as its carrier. The getter kotlinc records, and the field
            // descriptor a reader cannot derive from `S`, both name that carrier.
            let value_descriptor =
                erased_value_class_descriptor(ir, declaration).unwrap_or(ty_descriptor);
            let getter = has_getter.then(|| {
                (
                    accessor_jvm_name(getter_function).unwrap_or_else(|| match storage {
                        Some(storage) => ir.jvm_static_getter_name(storage),
                        None => krusty::jvm::names::property_getter_name(&declaration.name),
                    }),
                    value_class_accessor_descriptor(ir, getter_function)
                        .unwrap_or_else(|| format!("({descriptor_parameters}){value_descriptor}")),
                )
            });
            let setter = has_setter.then(|| {
                let mut parameters = descriptor_parameters;
                parameters.push_str(&value_descriptor);
                (
                    accessor_jvm_name(setter_function).unwrap_or_else(|| match storage {
                        Some(storage) => ir.jvm_static_setter_name(storage),
                        None => krusty::jvm::names::property_setter_name(&declaration.name),
                    }),
                    value_class_accessor_descriptor(ir, setter_function)
                        .unwrap_or_else(|| format!("({parameters})V")),
                )
            });
            // A delegate evaluated in place has no storage; its `get<X>$delegate` stands in.
            let delegate_method =
                ir.jvm_package_delegate_method(declaration.property)
                    .map(|method| {
                        let function = &ir.functions[method as usize];
                        (
                            function.name.clone(),
                            krusty::jvm::method_descriptors::ir_method_desc(
                                &function.params,
                                &function.ret,
                            ),
                        )
                    });
            let package_record = krusty::metadata::declaration_records::package_property(
                ir,
                declaration,
                setter_function,
            );
            krusty::metadata::builder::PropMeta {
                getter,
                setter,
                has_backing_field: package_record.has_backing_field && delegate_method.is_none(),
                delegate_method,
                field_name: field.as_ref().and_then(|(name, _)| name.clone()),
                field_desc: field.map(|(_, descriptor)| descriptor).filter(|physical| {
                    declaration.spellings.ret.actualized_expect
                        || super::metadata_method_signatures::requires_field_signature(
                            declaration.ty,
                            physical,
                            &local_classifiers,
                        )
                }),
                annotation_method: ir
                    .jvm_package_property_annotation_marker(declaration.property)
                    .and_then(|marker| ir.functions.get(marker as usize))
                    .map(|marker| {
                        (
                            marker.name.clone(),
                            krusty::jvm::method_descriptors::ir_method_desc(
                                &marker.params,
                                &marker.ret,
                            ),
                        )
                    }),
                ..package_record
            }
        })
        .collect::<Vec<_>>();

    let aliases = ir
        .package_type_aliases
        .iter()
        .map(krusty::metadata::declaration_records::package_alias)
        .collect::<Vec<_>>();
    let approximate = |ty| krusty::metadata::declaration_approximation::approximate(symbols, ty);
    let classifier_application = |classifier| {
        krusty::metadata::classifier_applications::classifier_application(ir, symbols, classifier)
    };
    build_facade_metadata(
        FacadeMembers {
            functions,
            properties,
            aliases,
        },
        (module_name, locals),
        param_assertions,
        krusty::metadata::MetadataTypeLookups {
            intersection_approximation: Some(&approximate),
            classifier_applications: &classifier_application,
            local_classifiers,
            enum_entry_bodies,
            anonymous_classifiers,
        },
        metadata_stamp,
        annotations_in_metadata,
    )
}

/// The physical descriptor of an accessor the value-class pass rewrote: a receiver or value typed
/// by a value class is its erased carrier there (`getXx-IQRRRT4(I)I` over `val Z.xx`), as the
/// method itself declares it.
fn value_class_accessor_descriptor(
    ir: &krusty::ir::IrFile,
    accessor: Option<u32>,
) -> Option<String> {
    let accessor = accessor.filter(|accessor| ir.vc_declared_sigs.contains_key(accessor))?;
    let function = &ir.functions[accessor as usize];
    Some(krusty::jvm::method_descriptors::ir_method_desc(
        &function.params,
        &function.ret,
    ))
}

/// The physical name and descriptor of the static field holding a delegated package property's
/// delegate.
/// The carrier descriptor of a package property whose storage was erased from a value class.
fn erased_value_class_descriptor(
    ir: &krusty::ir::IrFile,
    declaration: &krusty::ir::IrPackageProperty,
) -> Option<String> {
    let krusty::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. } =
        ir.local_property_layouts.get(&declaration.property)?
    else {
        return None;
    };
    let field = ir.statics.get(*storage as usize)?;
    field
        .erased_declared_ty
        .is_some()
        .then(|| krusty::jvm::names::type_descriptor(field.ty))
}

/// The facade field realizing a property, if any: a delegate's storage by its JVM name, or a
/// backing field (named by the property), with its physical descriptor.
fn property_field(
    ir: &krusty::ir::IrFile,
    declaration: &krusty::ir::IrPackageProperty,
) -> Option<(Option<String>, String)> {
    let (name, storage) = match ir.local_property_layouts.get(&declaration.property)? {
        krusty::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. } => (None, *storage),
        krusty::ir::IrLocalPropertyLayout::TopLevelAccessor {
            delegate: Some(storage),
            ..
        } => (
            Some(ir.static_field_jvm_name(*storage).to_string()),
            *storage,
        ),
        _ => return None,
    };
    let field = ir.statics.get(storage as usize)?;
    Some((
        name,
        krusty::jvm::names::type_descriptor(krusty::jvm::method_descriptors::jvm_value_ty(
            &field.ty,
        )),
    ))
}

/// The JVM descriptor of a package function realized from its declaration: context parameters,
/// the extension receiver, the value parameters, and a suspend function's continuation.
fn declared_method_descriptor(declaration: &krusty::ir::IrPackageFunction) -> String {
    let mut physical = declaration
        .params
        .iter()
        .map(|(_, parameter)| *parameter)
        .collect::<Vec<_>>();
    // A companion extension's receiver is recorded but has no JVM parameter.
    if let Some(receiver) = declaration.receiver.filter(|_| !declaration.companion) {
        physical.insert(declaration.context_count.min(physical.len()), receiver);
    }
    let result = if declaration.suspend {
        physical.push(Ty::obj("kotlin/coroutines/Continuation"));
        Ty::obj("java/lang/Object")
    } else {
        declaration.ret
    };
    krusty::jvm::method_descriptors::ir_method_desc(&physical, &result)
}

/// The declarations a file facade's package metadata publishes.
struct FacadeMembers {
    functions: Vec<krusty::metadata::builder::FnMeta>,
    properties: Vec<krusty::metadata::builder::PropMeta>,
    aliases: Vec<krusty::metadata::builder::TypeAliasMeta>,
}

fn build_facade_metadata(
    FacadeMembers {
        functions,
        properties,
        aliases,
    }: FacadeMembers,
    (module_name, locals): (&str, &[LocalPropertyMeta]),
    param_assertions: bool,
    lookups: krusty::metadata::MetadataTypeLookups<'_>,
    metadata_stamp: krusty::jvm::classfile::MetadataStamp,
    annotations_in_metadata: bool,
) -> Option<crate::ir_emit::KotlinMetadata> {
    (!functions.is_empty() || !properties.is_empty() || !aliases.is_empty()).then(|| {
        let (d1_bytes, d2) =
            krusty::metadata::builder::build_package_with_intersection_approximation(
                &functions,
                &properties,
                &aliases,
                ((module_name != "main").then_some(module_name), locals),
                param_assertions,
                lookups,
                annotations_in_metadata,
            );
        crate::ir_emit::KotlinMetadata {
            k: 2,
            stamp: metadata_stamp,
            xi: 48,
            d1: krusty::metadata::encoding::bytes_to_strings(&d1_bytes),
            d2,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_inner_classes_use_declaration_names_and_shadow_classpath_rows() {
        use krusty::types::{type_name_child, ClassifierDeclarationName, TypeName, Visibility};
        let package = type_name("member/ownership");
        let outer = type_name_child(package, "Outer");
        let literal = type_name_child(package, "Outer$Literal");
        let member = literal.nested_child("Part$Name");
        let unknown = type_name_child(package, "Unknown$Owner");
        let shape = |classifier, declaration_name| InnerModuleClassifier {
            classifier,
            declaration_name,
            visibility: Visibility::Public,
            inner: false,
            annotation: false,
            interface: false,
            enum_class: false,
            abstract_class: false,
            final_class: true,
        };
        let classes = [
            shape(
                outer,
                Some(ClassifierDeclarationName::TopLevel {
                    package,
                    name: "Outer".into(),
                }),
            ),
            shape(
                literal,
                Some(ClassifierDeclarationName::TopLevel {
                    package,
                    name: "Outer$Literal".into(),
                }),
            ),
            shape(
                member,
                Some(ClassifierDeclarationName::Nested {
                    owner: literal,
                    name: "Part$Name".into(),
                }),
            ),
            shape(unknown, None),
        ];
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = calls.clone();
        let classpath = std::rc::Rc::new(move |_: &str| {
            observed.set(observed.get() + 1);
            Some(krusty::jvm::classfile::InnerClassDetails {
                outer: Some("stale/Owner".into()),
                name: Some("Stale".into()),
                access: 0x0001,
            })
        });
        let resolve =
            module_inner_class_resolver_from_shapes(classes, std::iter::empty(), classpath);
        assert!(resolve(&outer.render()).is_none());
        assert!(resolve(&literal.render()).is_none());
        assert!(resolve(&unknown.render()).is_none());
        let row = resolve(&member.render()).expect("declared member row");
        assert_eq!(
            (row.outer, row.name, row.access),
            (Some(literal.render()), Some("Part$Name".into()), 0x0019)
        );
        assert_eq!(calls.get(), 0);
        let row = resolve(&type_name_child(TypeName::ROOT, "External").render()).unwrap();
        assert_eq!(
            (row.outer, row.name, row.access),
            (Some("stale/Owner".into()), Some("Stale".into()), 0x0001)
        );
        assert_eq!(calls.get(), 1);
    }

    struct NoClassifierFacts;

    impl krusty::backend::BackendClassifierSource for NoClassifierFacts {
        fn classifier(
            &self,
            _classifier: krusty::types::TypeName,
        ) -> Option<std::sync::Arc<krusty::backend::BackendClassifierFact>> {
            None
        }
    }

    /// Every caller supplies a logical source stem, but module/corpus callers can retain directories
    /// that the CLI has already stripped. The shared constructor must own that normalization so all
    /// emitted `SourceFile` attributes contain the JVM-required simple filename on either path style.
    #[test]
    fn shipping_emit_options_normalize_logical_source_paths() {
        let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
        let unix = shipping_emit_options(
            "suite/nested/Foo",
            krusty::frontend::SourceKind::Kotlin,
            "main",
            None,
            cp.clone(),
        );
        let windows = shipping_emit_options(
            "suite\\nested\\Bar",
            krusty::frontend::SourceKind::Kotlin,
            "main",
            None,
            cp,
        );

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
        let root = &workspace_root();
        for relative in [
            "crates/krusty-jvm/src/bin/survey.rs",
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
        let root = &workspace_root();
        for relative in [
            "crates/krusty-jvm/src/external_calls.rs",
            "crates/krusty-jvm/src/function_references.rs",
            "crates/krusty-jvm/src/bridges.rs",
            "crates/krusty-jvm/src/module_calls.rs",
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
        let root = &workspace_root();
        for relative in [
            "crates/krusty-jvm/src/external_calls.rs",
            "crates/krusty-jvm/src/property_references.rs",
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
    fn facade_metadata_uses_the_custom_setter_parameter_identity() {
        let mut ir = krusty::ir::IrFile::default();
        let setter = ir.add_fun(krusty::ir::IrFunction {
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
            krusty::ir::FnParamInfo::identities(vec![krusty::ir::IrParameterIdentity::source(
                "replacement",
            )]),
        );
        let property_id = krusty::fir::PropertyId::from_raw(0);
        let init = ir.add_expr(krusty::ir::IrExpr::Const(krusty::ir::IrConst::Int(0)));
        ir.statics.push(krusty::ir::IrStatic {
            name: "topLevel".to_string(),
            ty: Ty::Int,
            init: Some(init),
            is_var: true,
            is_const: false,
            is_lateinit: false,
            owner: None,
            visibility: krusty::types::Visibility::Public,
            erased_declared_ty: None,
            accessors: krusty::ir::IrStaticAccessors::ABSENT,
            line: 0,
            accessor_lines: krusty::ir::IrAccessorLines::default(),
            source_order: 0,
            property_delegate: false,
        });
        ir.local_property_layouts.insert(
            property_id,
            krusty::ir::IrLocalPropertyLayout::TopLevelStorage {
                storage: 0,
                getter: None,
                setter: Some(setter),
                qualifier: None,
            },
        );
        ir.package_properties.push(krusty::ir::IrPackageProperty {
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
            visibility: krusty::types::Visibility::Public,
            annotations: Box::new([]),
            flags: krusty::fir::DeclarationFlags::default(),
            spellings: krusty::spelling::DeclaredSpellings::default(),
            has_backing_field: true,
            modifiers: krusty::ir::IrPropertyModifiers {
                declared_setter: true,
                ..Default::default()
            },
            setter_visibility: krusty::types::Visibility::Public,
            source_order: 0,
        });

        let metadata = facade_package_metadata_from_ir(
            &ir,
            ("main", &[]),
            true,
            &NoClassifierFacts,
            krusty::jvm::classfile::MetadataStamp {
                version: [2, 4, 0],
                pre_release: false,
            },
            true,
        )
        .expect("a package property requires facade metadata");
        let decoded = krusty::jvm::metadata::decode_metadata(
            &metadata.d1,
            &metadata.d2,
            Some(metadata.k),
            "a/AKt",
            None,
        )
        .expect("decode generated facade metadata");

        let members = decoded
            .members
            .decode(&[], &decoded.type_parameter_scope)
            .expect("decode generated facade members");
        let [property] = members.package_properties.as_ref() else {
            panic!(
                "expected one package property, got {:?}",
                members.package_properties
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
            Some(krusty::types::type_name("kotlin/Int"))
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
                &[
                    "crates/krusty-jvm/src/property_storage.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "lower_companion_properties(",
                &[
                    "crates/krusty-jvm/src/companion.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "finalize_hoisted_static_field_names(",
                &[
                    "crates/krusty-jvm/src/companion.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "elide_default_property_stores(",
                &[
                    "crates/krusty-jvm/src/property_storage.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "lower_value_classes(",
                &[
                    "crates/krusty-jvm/src/value_classes.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "lower_suspend(",
                &[
                    "crates/krusty-jvm/src/suspend.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "finalize_suspend_bridges(",
                &[
                    "crates/krusty-jvm/src/suspend/cps_bridges.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "mark_must_inline_lambdas(",
                &[
                    "crates/krusty-jvm/src/ir_emit/must_inline_lambdas.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "reparent_lambda_impls(",
                &[
                    "crates/krusty-jvm/src/ir_emit.rs",
                    "crates/krusty-jvm/src/ir_emit/lambda_implementation_placement.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "complete_enabled(",
                &["src/plugins/mod.rs", "crates/krusty-jvm/src/backend.rs"],
            ),
            (
                "derive_bridges(",
                &[
                    "crates/krusty-jvm/src/bridges.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "mangle_internal_members(",
                &[
                    "crates/krusty-jvm/src/internal_names.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
            (
                "collection_barriers::select(",
                &["crates/krusty-jvm/src/backend.rs"],
            ),
            (
                "check_before_result_coercion(",
                &[
                    "crates/krusty-jvm/src/result_null_checks.rs",
                    "crates/krusty-jvm/src/backend.rs",
                ],
            ),
        ];
        let root = &workspace_root();
        let mut offenders = Vec::new();
        for dir in ["src", "tests", "crates/krusty-jvm/src"] {
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
        let root = &workspace_root();
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
        let root = &workspace_root();
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

    /// The repository checkout: these audits read the frontend's sources as well as this crate's.
    fn workspace_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root")
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
