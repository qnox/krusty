//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of backend-agnostic IR maps Kotlin
//! identities to JVM descriptors here; common IR never carries descriptors.

use crate::fx_hash::{HashMap, HashSet};

use crate::backend::BackendClassifierSource;
use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrDataClassMemberRole, IrExpr, IrField,
    IrFile, IrTypeOp,
};
use crate::jvm::array_representation::prim_newarray_atype;
use crate::jvm::classfile::{
    ClassWriter, CodeBuilder, InnerClassResolver, Label, VerifType, MAJOR_JAVA8,
};
use crate::jvm::classreader::{MethodCode, C};
use crate::jvm::constructor_debug::{primary_constructor_line, property_line};
use crate::jvm::inline::MethodBodies;
use crate::jvm::multifile_facades;
use crate::jvm::names::{
    mapped_builtin_virtual_name, method_descriptor, property_getter_name, property_setter_name,
    type_descriptor,
};
use crate::jvm::property_references::local_delegated_properties::LocalDelegatedProperties;
use crate::jvm::value_classes::instance_representation;
use crate::kt_string::KtStringBuf;
use crate::types::{stored_value_ty, Ty, TyKind, TypeName, TypeVariance};
use companion_field::{add_companion_field, companion_field_access, emit_companion_init};
use field_visibility::{declared_field_access, default_accessor_access, is_jvm_field};
use inherited_default_forwarders::emit_default_impls_forwarders;

mod access_bridges;
mod accessor_emission_order;
mod annotation_impl;
mod annotation_interface;
mod array_elements;
mod backend_temporaries;
mod binary_operation;
mod block_scope;
mod bottom_values;
mod bridge_emission;
mod builtin_member_emission;
mod bytecode_inline_call;
mod call_operands;
pub(in crate::jvm) mod capture_order;
mod captured_storage;
mod checked_facts;
mod class_lambda_reflection;
mod class_literals;
mod class_mode_lambda_members;
mod class_pool_seed;
mod companion_blocks;
mod comparison_branches;
mod condition_emission;
mod constant_emission;
mod constructor_accessors;
mod constructor_defaults;
mod constructor_initialization;
mod constructor_signatures;
use constructor_defaults::{constructor_default_masks, emit_constructor_default_arguments};
mod companion_field;
mod copied_code;
mod coroutine_machine;
mod debug_lines;
mod declaration_annotations;
mod declaration_types;
mod declared_nullability;
mod declared_property_access;
mod declared_property_accessor;
mod default_impls;
mod delegated_property_array;
mod field_nullability;
use field_nullability::{
    declared_field_annotation, declared_property_annotation, declared_static_annotation,
    field_nullability_kind, is_nonnull_reference_field, nullability_annotation,
};
mod discarding;
use discarding::discard;
mod diverging_value_type;
mod emission_readiness;
mod emit_options;
pub(crate) use emission_readiness::jvm_can_emit;
pub(super) mod enclosure;
mod enum_entry_subclass;
mod enum_reflection_call;
mod enum_synthetic_fields;
pub use emit_options::{EmitOptions, DEFAULT_METADATA_VERSION};
mod explicit_backing_fields;
mod field_read;
mod field_visibility;
mod field_write;
mod frame_map;
mod function_annotations;
mod function_debug;
mod function_invocation;
mod static_function_calls;
use function_invocation::{
    is_high_arity_function, jvm_function_interface, jvm_function_invoke_descriptor,
};
mod function_reference_class;
mod function_reference_invoke;
mod generated_property_operations;
mod implicit_reference_coercion;
mod in_place_arguments;
mod initializer_lines;
mod inline_body_emission;
mod inline_call;
mod inline_call_outcome;
use inline_call_outcome::InlineCallOutcome;
mod inline_frame_marker;
mod inline_lambda_aliases;
mod inline_return_frames;
mod inline_scope_frames;
mod instance_field_names;
use instance_field_names::instance_field_jvm_name;
mod inherited_default_forwarders;
mod interface_compatibility;
mod intrinsic_probes;
mod lambda_class;
pub(super) mod lambda_class_names;
mod lambda_implementation_placement;
pub(crate) use lambda_implementation_placement::reparent_lambda_impls;
mod local_updates;
mod local_variable_representation;
mod loop_emission;
mod member_dispatch;
mod member_schedule;
mod metadata_policy;
mod method_access;
pub(crate) use method_access::is_reifiable;
mod method_defaults;
pub(in crate::jvm) use method_defaults::{
    body_has_reified_markers, default_stub_descriptor, default_stub_mask_count,
};
mod method_entry;
mod method_signatures;
mod must_inline_lambdas;
mod non_local_jumps;
mod non_null_operands;
mod nullable_sam;
mod numeric_comparison;
mod object_static_initialization;
mod operand_representation;
mod operand_stack;
mod override_result_emission;
use override_result_emission::{declared_method_desc, declared_method_signature};
mod primary_constructor_parameters;
mod property_access;
use property_access::{accessor_receiver_ty, accessor_takes_receiver};
mod property_reference_class;
mod property_reference_values;
mod return_emission;
mod safe_calls;
mod sam_wrapper_class;
mod scalar_coercion;
mod script_main;
mod sealed_subtypes;
mod shared_cell_declaration;
mod signature_formatter;
mod singleton_instance;
mod singleton_instance_load;
mod spliced_locals;
mod suspend_lambda_class;
mod value_class_adapters;
use value_class_adapters::{emit_value_class_box_adapter, emit_value_class_unbox_adapter};
mod transformed_suspensions;
use lambda_implementation_placement::standalone_method_is_elided;
mod try_emission;
use annotation_impl::emit_annotation_impl_class;
use array_elements::reference_array_scalar_adapter;
mod value_class_descriptors;
mod value_class_override_metadata;
mod value_class_signatures;
use crate::jvm::private_static_access::StaticOwner;
use class_pool_seed::{
    primary_constructor_method_parameters, seed_accessor_locals, seed_ctor_before_members,
    seed_enum_constructor_locals, seed_plain_constructor_locals, seed_plain_constructor_tail,
    seed_value_class_method_locals, seed_value_ctor_after_members, PlainClassPoolSeed,
};
pub(super) use enclosure::{class_enclosure, property_accessor_function};
use primary_constructor_parameters::{
    primary_ctor_parameter_fields, primary_ctor_source_parameters,
};
use try_emission::ProtectedRegion;
mod class_metadata;
#[cfg(test)]
mod metadata_name_tests;
use class_metadata::build_class_metadata;
#[cfg(test)]
use class_metadata::build_class_metadata_with_facts;
mod constructor_delegation_arguments;
pub(super) mod jvm_static_members;
mod method_placement;
use method_placement::MethodPlacement;
mod entries_mappings;
mod primary_super_call;
mod secondary_constructor;
mod static_accessors;
mod static_fields;
mod string_concatenation;
mod supertype_markers;
mod synth_debug_tables;
mod type_operation_emission;
mod type_parameter_signatures;
mod unified_lambda_splice;
mod value_emission;
mod vararg;
mod when;
mod when_mappings;
use declared_property_accessor::{
    declared_property_accessor_jvm, emit_backing_field_read_adaptation,
    emit_backing_field_write_adaptation,
};
use signature_formatter::{JvmPropertySignatures, JvmSignatureFormatter, Wildcards};
use singleton_instance::{add_singleton_instance_field, emit_singleton_instance_clinit};
use synth_debug_tables::attach_synth_debug_tables;
use type_parameter_signatures::{jvm_class_signature, jvm_type_params};

use super::method_parameters::OwnerConstructorPrefix;
pub(crate) use checked_facts::{CheckedEmitFacts, EmitMetadata};
pub(super) use declaration_types::function_descriptor;
use declaration_types::ir_method_desc;
pub(crate) use declaration_types::jvm_tys;
pub(super) use declaration_types::{class_ctor_jvm_tys, secondary_prefix_jvm_tys};
use declaration_types::{field_jvm_tys, jvm_declared_ty, jvm_value_ty, signature_function_params};
use declaration_types::{
    ir_type_desc, jvm_function_params, jvm_is_erased_top, local_variable_desc,
};
use frame_map::{FrameKey, TempRole};
use inline_body_emission::collect_body_var_types;
use inline_call::{bind_inline_handlers, parse_descriptor_params, InlineStaticTarget};
use member_schedule::{
    source_ordered_members, split_around_primary_constructor, SourceOrderedMember,
};
pub use metadata_policy::KotlinMetadata;
use metadata_policy::{
    finish_local_synthetic_class, is_continuation_class, is_coroutine_state_machine,
    synthetic_class_xi, SYNTHETIC_LOCAL, SYNTHETIC_PROTECTED, SYNTHETIC_PUBLIC,
};
use method_defaults::{
    emit_default_stub, emit_facade_default_stub, static_default_stub_marker, DefaultStubSource,
};
pub use must_inline_lambdas::mark_must_inline_lambdas;
use must_inline_lambdas::splice_called_impls;
use property_reference_values::{box_property_reference_value, value_class_boundary_conversion};
use scalar_coercion::{
    box_prim_free, emit_num_conv, native_unsigned_impl_target, semantic_scalar_adapter, unbox_prim,
    unbox_prim_from, unbox_prim_from_descriptor, wrapper_owner_primitive,
};
use secondary_constructor::SecondaryConstructorEmitter;

/// kotlinc realizes a NAMED `object` declaration's property backing fields as STATIC fields on the
/// object class: accessors read/write `getstatic`/`putstatic`, initializers run in `<clinit>` after
/// the `INSTANCE` store, and `<init>` is a bare `super()` call. Companions hoist their fields to the
/// OUTER class instead (not modeled yet), and local/anonymous objects keep instance fields.
fn object_static_storage(c: &IrClass) -> bool {
    c.is_object && !c.is_companion && !c.is_local_class && !c.is_enum_entry
}

/// Kotlin interfaces and annotation classes are both JVM interfaces. Common IR keeps their source
/// kinds distinct because annotation construction and validation differ; JVM storage decisions use
/// this combined target classification.
fn is_jvm_interface(c: &IrClass) -> bool {
    c.is_interface || c.is_annotation
}

fn declared_jvm_interface(ir: &IrFile, owner: TypeName) -> bool {
    ir.classes
        .iter()
        .any(|class| class.fq_name_id() == owner && is_jvm_interface(class))
}

/// A companion of an INTERFACE uses OBJECT-style static storage (kotlinc's interface-companion
/// layout): its instance lives in a `static final $$INSTANCE` on the companion itself, and its
/// properties back `static` fields there. A public or internal `const val` is also copied onto the
/// interface as `public static final` (the only field shape an interface admits). A private const
/// cannot be an interface field, so it stays on the companion only. The interface's `Companion`
/// field aliases `$$INSTANCE`.
fn companion_of_interface(ir: &IrFile, c: &IrClass) -> bool {
    if !c.is_companion {
        return false;
    }
    ir.classes.iter().any(|candidate| {
        is_jvm_interface(candidate) && candidate.companion_class == Some(c.fq_name)
    })
}

/// [`object_static_storage`] plus the interface-companion case — the storage rule emission keys on.
fn static_storage(ir: &IrFile, c: &IrClass) -> bool {
    object_static_storage(c) || companion_of_interface(ir, c)
}

fn has_ctor_marker_accessor(ir: &IrFile, class: &IrClass) -> bool {
    // An INTERFACE's companion self-constructs in its own `<clinit>` (no cross-class construction),
    // so kotlinc emits no marker ctor for it.
    class.has_primary_ctor
        && (class.is_sealed
            || (class.is_companion && !companion_of_interface(ir, class))
            || ir.has_value_param_ctor(&class.fq_name()))
}

/// Whether the primary's marker accessor follows the members as a companion's or a hidden
/// value-class primary's does. A sealed class's accessors are `constructor_accessors`' to emit.
fn marker_accessor_emitted_last(ir: &IrFile, class: &IrClass) -> bool {
    class.is_companion || (!class.is_sealed && ir.has_value_param_ctor(&class.fq_name()))
}

/// Mutable per-emit-run accumulators, owned by the caller and shared (by `&`, via interior mutability)
/// down the emit callgraph — formerly three thread-locals. The checked backend reads
/// `inline_bail`/`emit_bail` after emission returns `None` to distinguish an inline-splice failure
/// (a backend bug to fix) from an unsupported construct (skip the file).
#[derive(Default)]
pub(crate) struct EmitRun {
    /// What a multifile part leaves its facade.
    pub(crate) multifile_part: multifile_facades::EmittedPart,
    /// Complete physical instance-field names, built at most once for each class touched by this
    /// emission run and reused by every field declaration, accessor, metadata, and bytecode use.
    instance_field_names: std::cell::RefCell<instance_field_names::InstanceFieldNamePlans>,
    /// The reason an inline splice failed during emission (a required stdlib-inline call the backend
    /// could not splice), else `None`.
    inline_bail: std::cell::RefCell<Option<String>>,
    /// Set when a supported emission path lacks a checked backend fact and must reject the file with
    /// a diagnostic. Broken common-IR ownership contracts are invariants and do not use this path.
    emit_bail: std::cell::Cell<bool>,
    emit_error: std::cell::RefCell<Option<String>>,
    /// This pass's same-module inline functions, compiled before their callers.
    compiled_inline_callees: bytecode_inline_call::CompiledInlineCallees,
    /// Lambda impl `FunId`s that got a REAL `invokedynamic` this pass. A lambda spliced by the inliner
    /// (a `require { … }` message, an inlined `flatMap { … }` body) never emits one, so its standalone
    /// `$lambda$N` method is dead — dropped on the re-emit (kotlinc emits neither it nor its facade).
    used_lambdas: std::cell::RefCell<crate::fx_hash::HashSet<u32>>,
    /// Lambda impls proven dead by the discovery emit. Unlike `inline_only_fns`, this is an
    /// emit-run decision: it includes class-owned helpers whose every use was spliced, and is cleared
    /// before the next independent emission of the same IR.
    dead_lambdas: std::cell::RefCell<crate::fx_hash::HashSet<u32>>,
    /// Subset of live closures actually realized with `invokedynamic`. A class-strategy closure still
    /// keeps its implementation method but does not impose the JVM-7 classfile requirement.
    used_indy_lambdas: std::cell::RefCell<crate::fx_hash::HashSet<u32>>,
    /// Lambda classes to synthesize for `-Xlambdas=class`, recorded as their `invokedynamic`
    /// replacement is emitted and drained by the class driver. They cannot be written inline: the
    /// emitter is mid-way through the ENCLOSING class's writer when it reaches a lambda.
    lambda_classes: std::cell::RefCell<Vec<LambdaClassPlan>>,
    /// Source/synthetic identity of every lambda class already written in the active emit pass,
    /// paired with its JVM owner. A source lambda lowered into multiple constructors still
    /// contributes one class. The discovery pass is discarded when dead implementations require a
    /// second emit, so this set is cleared with the pending plans at each pass boundary.
    lambda_classes_written: std::cell::RefCell<
        crate::fx_hash::HashSet<(String, lambda_class_names::LambdaClassIdentity)>,
    >,
    /// Private instance members reached from another emitted JVM class. Kotlin permits this across
    /// lexical nesting, while Java 8 bytecode does not; the declaring class owns one synthetic
    /// static access bridge and every cross-owner call targets it.
    private_member_access_bridges: std::cell::RefCell<crate::fx_hash::HashSet<u32>>,
    /// Protected member calls emitted outside the checked receiver subclass that grants access.
    /// The selected declaration and semantic receiver determine the bridge; emission performs no
    /// name lookup or subtype reconstruction.
    protected_member_access_bridges: std::cell::RefCell<
        crate::fx_hash::HashMap<crate::ir::ExprId, access_bridges::ProtectedMemberAccessBridge>,
    >,
    /// The synthetic accessors each static owner declares for its private static declarations
    /// used from other classes; see [`static_accessors`].
    static_accessor_plan: std::cell::RefCell<static_accessors::StaticAccessorPlan>,
    /// Spill plans discovered for the suspend functions whose coroutine machine emission owns.
    /// Absent on the discovery pass and present on the one that builds the machine.
    machine_plans: std::cell::RefCell<coroutine_machine::MachinePlans>,
    /// Continuation classes synthesized for the machines this emission builds, drained with the
    /// facade they belong to.
    machine_classes: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    /// What kotlinc's coroutine transformer found, by continuation class.
    transformed_coroutines: transformed_suspensions::TransformedCoroutines,
    /// The names of the anonymous objects regenerated for inline calls, by calling class.
    regenerated_object_names: bytecode_inline_call::RegeneratedObjectNames,
    /// The `$WhenMappings` tables of the classes this pass emits enum switches in.
    when_mappings: std::cell::RefCell<when_mappings::WhenMappingTables>,
    /// The `$EntriesIntrinsicMappings` fields of the classes this pass inlines reified
    /// `enumEntries` into.
    entries_mappings: std::cell::RefCell<entries_mappings::EntriesMappingTables>,
}

impl EmitRun {
    fn has_machine_plan(&self, function: u32) -> bool {
        self.machine_plans.borrow().contains_key(&function)
    }

    fn record_machine_plan(&self, function: u32, plan: coroutine_machine::MachinePlan) {
        self.machine_plans.borrow_mut().insert(function, plan);
    }

    fn machine_plan(&self, function: u32) -> Option<coroutine_machine::MachinePlan> {
        self.machine_plans.borrow().get(&function).cloned()
    }
}

/// One emitter-owned SAM class to write under [`LambdaMode::Class`].
///
/// Kotlin function lambdas are realized in IR, with their body owned by the realized class. This
/// plan remains only for SAM conversions whose implementation stays a static method on its owner.
#[derive(Clone, Debug)]
struct LambdaClassPlan {
    /// Internal name of the class to write (`LKt$box$plain$1`).
    internal: String,
    /// The interface the lambda implements (`kotlin/jvm/functions/Function1`, or a user SAM).
    iface: String,
    /// The interface method to implement, with its ERASED descriptor — the slot the JVM dispatches.
    sam_method: String,
    sam_desc: String,
    /// The existing implementation method: `[captures…, lambda params…]`.
    impl_owner: String,
    impl_name: String,
    impl_desc: String,
    /// Captured values, in implementation-parameter order; empty ⇒ a singleton `INSTANCE`.
    captures: Vec<Ty>,
    /// A fun-interface conversion of a callable reference implements Kotlin's FunctionAdapter
    /// equality contract by delegating to its single captured function value.
    function_adapter: bool,
    /// Whether the body lives on an INTERFACE: a static call to one needs an `InterfaceMethodref`
    /// constant, not a `Methodref` (`IncompatibleClassChangeError` otherwise).
    owner_is_interface: bool,
    identity: lambda_class_names::LambdaClassIdentity,
}

impl EmitRun {
    /// The inline-splice failure reason recorded this run, if any (read by the caller after `None`).
    pub(crate) fn inline_bail(&self) -> Option<String> {
        self.inline_bail.borrow().clone()
    }
    /// Record a stable public failure category. Concrete owners, callable names, and descriptors are
    /// deliberately excluded here because this value reaches CLI diagnostics and survey buckets;
    /// those identities are emitted through opt-in compiler traces at the failure site instead.
    fn set_inline_bail(&self, reason: &'static str) {
        *self.inline_bail.borrow_mut() = Some(reason.to_string());
    }

    /// The malformed-IR / missing-emission-context reason recorded this run, if any.
    pub(crate) fn emit_error(&self) -> Option<String> {
        self.emit_error.borrow().clone()
    }

    fn set_emit_error(&self, reason: String) {
        crate::trace_compiler!("splice", "JVM emit error: {reason}");
        self.emit_bail.set(true);
        let mut current = self.emit_error.borrow_mut();
        if current.is_none() {
            *current = Some(reason);
        }
    }

    /// Whether this pass has already declined to emit the file. Class finalization must observe
    /// this before frame computation: a method that reported a missing backend fact can have an
    /// intentionally incomplete operand stack at the point where it stopped.
    fn emission_failed(&self) -> bool {
        self.inline_bail.borrow().is_some() || self.emit_bail.get()
    }
}

/// The emit environment threaded (by `&`) through the whole emit callgraph in place of the bare
/// `bodies` provider: the bytecode provider plus the mutable run accumulators, so the deep `Emitter`
/// records a used lambda / an emit-or-inline bail without an ambient thread-local. Replacing `bodies`
/// keeps every function's argument count unchanged.
pub(super) struct EmitEnv<'a> {
    /// The file facade class's identity; the `facade` spelling threaded beside it is its class-file
    /// name.
    facade_class: TypeName,
    /// The public facade a multifile part's non-private members are invoked through.
    multifile_facade: Option<&'a str>,
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
    continuation_metadata: &'a crate::jvm::suspend::ContinuationMetadataMap,
    emit_time_machines: &'a crate::jvm::suspend::EmitTimeMachines,
    suspended_result_returns: &'a crate::jvm::suspend::SuspendedResultReturns,
    intrinsic_probe_continuations: &'a crate::jvm::suspend::IntrinsicProbeContinuations,
    bridge_adaptations: &'a crate::jvm::bridge_adaptations::BridgeAdaptations,
    /// The bridges that take `FunctionN.invoke`'s packed argument array.
    function_argument_arrays: &'a crate::jvm::function_argument_arrays::FunctionArgumentArrays,
    override_results: &'a crate::jvm::override_results::OverrideResults,
    collection_method_entry_barriers: &'a crate::jvm::collection_barriers::MethodEntryBarriers,
    collection_members: &'a crate::jvm::read_only_collection_stubs::CollectionMemberPlans,
    /// Semantic classifier declarations used only while translating Kotlin generic types into JVM
    /// `Signature` attributes. Declaration-site variance is a Kotlin fact; spelling it as JVM
    /// use-site wildcards is owned entirely by this emitter.
    signature_symbols: &'a dyn BackendClassifierSource,
    /// Provider-normalized JVM facts for dependency identities already selected into common IR.
    dependency_callables: &'a crate::backend::CheckedBackendCallables,
    /// Exact invocation-owner facts for both the stable module/dependency model and classifiers
    /// whose completed declarations exist only in this common-IR file.
    dispatch_classifiers: std::rc::Rc<crate::jvm::member_dispatch::CheckedDispatchClassifiers<'a>>,
    /// `-jvm-default`. Read at CALL SITES: under `Disable` an interface's `$default` synthetic lives
    /// on the `$DefaultImpls` holder, not on the interface, so a call that omits a defaulted argument
    /// must target the holder or it links to a method that does not exist.
    jvm_default: JvmDefaultMode,
    /// The independently selected plain-lambda and SAM-conversion strategies.
    lambda_modes: LambdaModes,
    /// JVM-only realizations for already-resolved current-module property operations. Kept beside
    /// the emitter rather than on common IR so a backend field/accessor choice cannot leak into FIR.
    property_realizations: &'a crate::jvm::property_realizations::PropertyRealizations,
    /// JVM carrier and accessor realization selected for each synthesized property-reference
    /// class. Common IR deliberately carries none of these representation facts.
    property_reference_realizations:
        &'a crate::jvm::property_references::PropertyReferenceRealizations,
    /// JVM-only construction plans for Kotlin function-value SAM wrappers.
    sam_wrapper_realizations: &'a crate::jvm::sam_wrappers::SamWrapperRealizations,
    local_delegate_access: &'a crate::jvm::local_delegate_accessors::HelperAccess,
    /// Per-call JVM placeholder/mask/marker plans produced during default-call realization.
    default_call_operands: &'a crate::jvm::default_call_operands::DefaultCallOperands,
    /// File-wide `InnerClasses` candidates prepared once from stable classifier identities.
    inner_classes: crate::jvm::inner_classes::InnerClasses,
    /// `-java-parameters`: name each declared parameter in a `MethodParameters` attribute.
    java_parameters: bool,
    /// `-Xstring-concat=inline`: build string concatenations with `StringBuilder` at every target.
    inline_string_concat: bool,
    /// `-Xuse-inline-scopes-numbers`: name inlined locals by their inline scope's number.
    inline_scopes_numbers: bool,
    /// The `@kotlin.Metadata` `mv` (`-language-version`, or [`DEFAULT_METADATA_VERSION`]) and
    /// pre-release flag this emission writes.
    metadata_stamp: crate::jvm::classfile::MetadataStamp,
}

/// `-Xlambdas` / `-Xsam-conversions`: how a lambda and a SAM conversion are realized on the JVM.
///
/// The two strategies produce different CLASS SETS, so this is an emitter selection rather than an
/// optimization hint: under `Class` every lambda contributes its own `.class`, and a build that asked
/// for it and got `Indy` would ship a different artifact list than it declared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LambdaMode {
    /// kotlinc's default since 2.0: an `invokedynamic` call site bootstrapped through
    /// `LambdaMetafactory`, with the body in a private static `$lambda$N` on the enclosing class.
    #[default]
    Indy,
    /// The pre-2.0 strategy, still selected explicitly by 40 of intellij-community's `BUILD.bazel`
    /// files: one synthetic class per lambda, extending `kotlin.jvm.internal.Lambda` and
    /// implementing the target interface. A lambda that captures nothing is a singleton held in a
    /// static `INSTANCE` field; a capturing one is constructed per evaluation with its captures as
    /// constructor arguments.
    Class,
}

/// JVM realization strategies for the two distinct Kotlin compiler options. Keeping the pair as one
/// value lets every CLI/backend/emitter boundary forward the same normalized configuration without
/// parallel booleans or last-option-wins behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LambdaModes {
    pub lambdas: LambdaMode,
    pub sam_conversions: LambdaMode,
}

use crate::jvm::override_results::boxes_sam_result;

impl LambdaModes {
    /// How the closure of a lambda (or a conversion to `sam`) of `arity` parameters is realized. A
    /// callable-reference adapter, a high-arity function, and a SAM method with a boxed result are
    /// classes under any mode, as kotlinc writes them.
    fn for_lambda(self, sam: Option<&crate::ir::IrSamTarget>, arity: u8) -> LambdaMode {
        match sam {
            Some(target) if target.function_adapter || boxes_sam_result(target) => {
                LambdaMode::Class
            }
            Some(_) => self.sam_conversions,
            None if is_high_arity_function(arity) => LambdaMode::Class,
            None => self.lambdas,
        }
    }
}

/// kotlinc's `-jvm-default` strategy for interface members with bodies — which of the three JVM
/// shapes an interface is compiled into.
///
/// Measured against kotlinc 2.4.10 on an interface with a default method, a default property getter,
/// and a method with a default parameter value:
///
/// | | interface members | `$DefaultImpls` | implementing class | `jvmClassFlags` |
/// |---|---|---|---|---|
/// | `Enable` | default methods + `access$…$jd` bridges | forwarders to those bridges | forwarder overrides (`invokespecial`) | 3 |
/// | `NoCompatibility` | default methods | absent | nothing | 1 |
/// | `Disable` | all abstract | the real bodies, receiver as parameter 0 | forwarders (`invokestatic`) | absent |
///
/// krusty emits that full table:
///   * `NoCompatibility` — default methods on the interface, no holder, no class forwarders.
///   * `Enable` — default methods plus the compatibility surface: the `access$…$jd` bridges, holder
///     statics forwarding to them (`@Deprecated`, holder bytes differential-tested), a thin
///     `$default` holder forward, `invokespecial` forwarder overrides on implementing classes, and
///     the republished surface on sub-interfaces for inherited defaults.
///   * `Disable` emits holder bodies and implementing-class forwarders. A dependency compiled in
///     that mode publishes those holder methods as its declarations' exact physical realizations,
///     so a consumer does not reinterpret the dependency from its own mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JvmDefaultMode {
    /// kotlinc's own default since 2.2 (legacy spelling `-Xjvm-default=all-compatibility`): default
    /// methods on the interface AND a `$DefaultImpls` compatibility copy.
    #[default]
    Enable,
    /// Legacy spelling `-Xjvm-default=all`. Default methods only — no `$DefaultImpls` anywhere, and
    /// no forwarders on implementing classes. What intellij-community builds with.
    NoCompatibility,
    /// Legacy spelling `-Xjvm-default=disable`. No default methods at all: the interface is fully
    /// abstract and every body lives on `$DefaultImpls` as a static taking the receiver.
    Disable,
}

impl JvmDefaultMode {
    /// Parse a `-jvm-default` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enable" => Some(Self::Enable),
            "no-compatibility" => Some(Self::NoCompatibility),
            "disable" => Some(Self::Disable),
            _ => None,
        }
    }

    /// Parse a legacy `-Xjvm-default` value, whose names denote the SAME three shapes under
    /// different spellings. Getting this mapping backwards is silent: `all` reads as a plausible
    /// "enable" and then emits a `$DefaultImpls` class the build deliberately does not have.
    pub fn parse_legacy(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::NoCompatibility),
            "all-compatibility" => Some(Self::Enable),
            "disable" => Some(Self::Disable),
            _ => None,
        }
    }

    /// The `jvmClassFlags` (`Class` JvmProtoBuf extension field 104) an interface carries under this
    /// mode: bit 0 `hasMethodBodiesInInterface`, bit 1 `isCompiledInCompatibilityMode`. `Disable`
    /// sets neither, and kotlinc then omits the field entirely.
    pub fn interface_jvm_class_flags(self) -> Option<u64> {
        match self {
            Self::Enable => Some(3),
            Self::NoCompatibility => Some(1),
            Self::Disable => None,
        }
    }
}

/// Drop every implicit not-null guard on a narrowed platform value.
///
/// `-Xno-call-assertions` removes the null checks kotlinc emits where a Java call's `T!` result is
/// committed to a declared non-null type. The guard is an expression wrapper, so it is removed by
/// rewriting the node into its operand's value: `x!!` is a SOURCE assertion the flag leaves alone.
pub(crate) fn strip_call_assertions(ir: &mut IrFile) {
    for expr in &mut ir.exprs {
        let IrExpr::NotNullAssert { operand, check } = expr else {
            continue;
        };
        if check.is_implicit() {
            *expr = IrExpr::Block {
                stmts: Vec::new(),
                value: Some(*operand),
            };
        }
    }
}

/// The primary constructor's JVM descriptor: every constructor argument, property-backed or plain.
fn primary_ctor_descriptor(ir: &IrFile, c: &IrClass) -> String {
    format!("({})V", primary_ctor_parameter_descs(ir, c))
}

fn primary_ctor_parameter_descs(ir: &IrFile, c: &IrClass) -> String {
    class_ctor_jvm_tys(ir, c)
        .into_iter()
        .map(crate::jvm::names::type_descriptor)
        .collect()
}

fn ctor_field_descs(ir: &IrFile, c: &IrClass) -> String {
    let fields = c.fields.iter().take(c.ctor_param_count as usize).collect();
    capture_order::physical_fields(ir, c, fields)
        .into_iter()
        .map(|f| crate::jvm::names::type_descriptor(f.ty))
        .collect()
}

/// Physical JVM realization of one exact common data-class declaration. The role was bound to its
/// [`crate::ir::FunId`] before backend renaming, so this reads the final function directly rather
/// than recovering it from a source/physical name pair.
fn data_class_member_realization(
    ir: &IrFile,
    owner: TypeName,
    role: IrDataClassMemberRole,
    record: &crate::metadata::class_builder::FnMeta,
    local_classifiers: &crate::fx_hash::HashSet<TypeName>,
) -> Option<crate::metadata::class_builder::JvmFunctionSignature> {
    let function = &ir.functions[ir.data_class_member(owner, role)? as usize];
    let physical = ir_method_desc(&function.params, &function.ret);
    // Recorded exactly when a reader cannot rebuild the physical descriptor from the declared
    // types (kotlinc's `requiresFunctionSignature`): a boxed primitive, or an unsigned type that
    // the JVM realizes as its carrier.
    let requires_signature = super::metadata_method_signatures::requires_function_signature(
        None,
        record.params.iter().map(|(_, ty)| *ty),
        record.ret,
        &physical,
        local_classifiers,
    );
    let has_definitely_non_null_type =
        crate::metadata::version_requirements::needs_definitely_non_null_type(&record.spellings);
    Some(crate::metadata::class_builder::JvmFunctionSignature {
        name: (function.name != record.name).then(|| function.name.clone()),
        desc: (requires_signature || has_definitely_non_null_type).then_some(physical),
    })
}

/// The synthesized `copy` declaration's exact common-IR identity, if this class owns one.
fn data_copy_fid(ir: &IrFile, c: &crate::ir::IrClass) -> Option<u32> {
    ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::Copy)
}

/// Attach kotlinc-style `LineNumberTable` + `LocalVariableTable` debug tables to a plain property
/// class's synthesized members (primary ctor + property accessors). Every such member maps to the
/// class declaration line, and its locals (`this` + params) live for the whole method — so the tables
/// are computable from `c.fields` alone. Call BEFORE `@Metadata` is attached so the debug strings
/// (`this`, member param names, the attribute names) intern into the constant pool ahead of the
/// annotation, matching kotlinc's ordering. Scoped to non-data classes for now (data-class synthesized
/// methods carry branches/stack maps and need their own line mapping).
/// Compute a plain property class's ctor/field/accessor descriptors and seed the constant pool in
/// kotlinc's interning order (see [`ClassWriter::seed_plain_class_pool`]). Mirrors the descriptors that
/// `attach_synth_debug_tables` and the natural emission produce, so the seeded entries are reused.
/// Whether the value class `fq_name` is one a downstream compilation can READ as a value class.
///
/// Admission is transitive: a member described as returning/taking `X` is only sound when `X` itself
/// carries a record, because a value class WITHOUT one reads downstream as an ordinary class — the
/// caller casts the carrier to the box and binds an instance accessor where kotlinc emits the static
/// `-impl`, i.e. a ClassCastException. So this answers positively, never by assumption:
///
/// - declared in THIS file: exactly when [`build_class_metadata`] admits it;
/// - declared in another file of this MODULE: exactly when its frozen Pass-1 declaration shape is in
///   `module_readable_value_classes`;
/// - anything else is on the CLASSPATH, where value-class-ness is itself decoded from the `@Metadata`
///   inline record — being known as a value class at all IS the evidence that a record exists.
fn value_class_is_readable(ir: &IrFile, fq_name: crate::types::TypeName) -> bool {
    if let Some(declared) = ir.classes.iter().find(|other| other.fq_name == fq_name) {
        return class_metadata::value_class_metadata_shape_admitted(ir, declared);
    }
    !ir.module_source_value_classes.contains(&fq_name)
        || ir.module_readable_value_classes.contains(&fq_name)
}

/// The JVM accessor spellings a class's synthesized property accessors are emitted under: the value-class
/// pass's stamp when the plain convention is not the physical ABI (`val k: K` → `getK-XLNMDGE`), else the
/// convention itself. The constant-pool seeder, the debug tables (`LineNumberTable`/`LocalVariableTable`)
/// and the `@Metadata` record all key on the accessor by NAME, so they must ask the same question the
/// emission does — a seeded/annotated `getK` beside an emitted `getK-XLNMDGE` interns a constant nothing
/// uses, drops the accessor's debug info, and (in the record) advertises a method that does not exist.
/// The convention itself is [`crate::names::property_getter_name`] — the same helper the accessor
/// EMISSION uses, so a Kotlin `is`-prefixed property (`val isOpen`, whose accessor keeps the source
/// name rather than becoming `getIsOpen`) is spelled one way everywhere.
fn accessor_jvm_names(c: &crate::ir::IrClass, field_name: &str) -> (String, String) {
    let declaration = c.properties.iter().find(|p| p.name == field_name);
    (
        declaration
            .and_then(|p| p.getter_jvm_name.clone())
            .unwrap_or_else(|| crate::names::property_getter_name(field_name)),
        declaration
            .and_then(|p| p.setter_jvm_name.clone())
            .unwrap_or_else(|| crate::names::property_setter_name(field_name)),
    )
}

/// The primary constructor's declared annotations in class-file order — RUNTIME-visible first, then
/// the BINARY-retained invisible ones. The pool seeder and the `@Metadata` mirror both read this
/// single sequence, so the two halves cannot drift apart.
fn primary_ctor_annotations(c: &crate::ir::IrClass) -> Vec<crate::ir::AppliedAnnotation> {
    let (visible, invisible) =
        crate::jvm::classfile::split_declaration_annotations(&c.primary_ctor_annotations);
    visible.into_iter().chain(invisible).collect()
}

/// One synthesized value-class member's JVM name, descriptor, and local-variable table entries.
fn attach_declared_method_debug(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
) {
    let owner = c.fq_name();
    for &fid in &c.methods {
        function_debug::attach_declared_function_debug(ir, override_results, fid, &owner, cw);
    }
}

/// Attach kotlinc's `@org.jetbrains.annotations.NotNull` / `@Nullable` to a plain property class's
/// synthesized members: each non-null reference-typed return/parameter gets `@NotNull`, each nullable
/// reference gets `@Nullable` (primitives get nothing). Covers the ctor's reference params, each
/// getter's reference return, and each `var` setter's reference param — the shape kotlinc emits for a
/// class with reference-typed properties. Call after `attach_synth_debug_tables`.
fn attach_synth_nullability(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    run: &EmitRun,
    cw: &mut ClassWriter,
) {
    cw.realize_leading_late_fields(); // The fields heading the table annotate first.
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    // A reference type (descriptor `L…;`/`[…`) gets `@NotNull` unless it is `Ty::Nullable`, then
    // `@Nullable`; a primitive gets no annotation.
    let ann = |t: Ty| nullability_annotation(field_nullability_kind(t));
    // Interfaces have accessors but no backing fields. The annotation targets the PHYSICAL field
    // (`result$1` when mangled away from a same-named hoisted companion static). The constructor
    // prefix's fields and other compiler-generated storage are not annotated.
    // An interface companion's member fields are still deferred at this point. Annotating them
    // here would intern each name before the field-table visit that pairs it with its descriptor.
    if !c.is_interface && !companion_of_interface(ir, c) {
        for (index, f) in c.fields.iter().enumerate() {
            if !field_visibility::publishes_field_nullability(c, index) {
                continue;
            }
            if let Some(a) = declared_field_annotation(c, index) {
                cw.set_field_nullability(
                    &instance_field_jvm_name(ir, run, c, index),
                    &desc(jvm_value_ty(&f.ty)),
                    a,
                );
            }
        }
    }
    // The `Companion` instance field is a never-null reference (`@NotNull`), and each REFERENCE
    // hoisted companion static is annotated like any other backing field.
    if let Some(companion) = &c.companion_class {
        cw.set_field_nullability(
            crate::jvm::companion_storage::field_name(c.fq_name, *companion),
            &format!("L{};", companion.render()),
            "Lorg/jetbrains/annotations/NotNull;",
        );
        for (index, s) in ir
            .statics
            .iter()
            .enumerate()
            .filter(|(index, s)| {
                ir.is_jvm_companion_hoisted_static(*index as u32) && s.owner_matches(&c.fq_name())
            })
            .filter(|(_, s)| !s.is_lateinit)
        {
            if let Some(a) = declared_static_annotation(s) {
                cw.set_field_nullability(
                    ir.static_field_jvm_name(index as u32),
                    &desc(jvm_declared_ty(&s.ty)),
                    a,
                );
            }
        }
    }
    // Primary constructor: one parameter annotation slot per SOURCE parameter, plain or
    // property-backed. The prefix takes no slot: kotlinc sizes the table by the source parameters,
    // so an inner class's first declared parameter is annotation parameter 0, as in javac's output.
    let source_parameters = primary_ctor_source_parameters(ir, c);
    let ctor_params: Vec<Option<&str>> = source_parameters
        .iter()
        .map(|parameter| nullability_annotation(parameter.nullability))
        .collect();
    let ctor_desc = primary_ctor_descriptor(ir, c);
    // kotlinc annotates none of a script's constructor parameters.
    if !c.is_script && ctor_params.iter().any(|p| p.is_some()) {
        cw.set_method_nullability("<init>", &ctor_desc, None, &ctor_params);
    }
    // HOISTED companion properties: the delegating accessors annotate like ordinary accessors
    // (reference getter return; a `var` reference setter's parameter). `@JvmField` emits no
    // accessors, so there is nothing to annotate (the FIELD's annotations ride the owner class).
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some()
            || static_fields::jvm_field_static_for(ir, c, property_index)
        {
            continue;
        }
        let Some(hoisted) = static_fields::hoisted_static_for(ir, c, property_index) else {
            continue;
        };
        let Some(a) = ann(hoisted.ty) else {
            continue;
        };
        let (getter, setter) = accessor_jvm_names(c, &property.name);
        let pd = desc(hoisted.ty);
        if property
            .getter
            .is_some_and(|getter| !ir.method_visibility(getter).is_private())
        {
            cw.set_method_nullability(&getter, &format!("(){pd}"), Some(a), &[]);
        }
        if hoisted.is_var
            && property
                .setter
                .is_some_and(|setter| !ir.method_visibility(setter).is_private())
        {
            cw.set_method_nullability(&setter, &format!("({pd})V"), None, &[Some(a)]);
        }
    }
    // The USER annotations written on the primary-constructor parameters, per source parameter
    // (the same slots `ctor_params` above describes).
    if !c.ctor_param_annotations.is_empty() {
        let user: Vec<crate::ir::DeclarationAnnotations> = source_parameters
            .iter()
            .map(|parameter| {
                parameter
                    .annotations
                    .cloned()
                    .unwrap_or_else(|| crate::ir::DeclarationAnnotations::new(Vec::new()))
            })
            .collect();
        cw.set_method_param_annotations("<init>", &ctor_desc, &user);
    }
    // Accessors: a reference getter annotates its return; a `var` reference setter its parameter.
    for (index, f) in c
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.is_compiler_generated())
    {
        let Some(a) = declared_property_annotation(c, index) else {
            continue;
        };
        let (getter, setter) = accessor_jvm_names(c, &f.name);
        // The accessor's own descriptor: an explicit backing field (`val x: Any? field: String`)
        // stores a narrower type than the getter returns.
        let accessor_desc = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(index as u32))
            .map_or_else(
                || desc(f.ty),
                |property| desc(declared_property_accessor_jvm(ir, property, f)),
            );
        cw.set_method_nullability(&getter, &format!("(){accessor_desc}"), Some(a), &[]);
        if !f.is_final() {
            cw.set_method_nullability(&setter, &format!("({accessor_desc})V"), None, &[Some(a)]);
        }
    }
    // Data-class synthesized methods: `copy` returns the class (`@NotNull`), `toString` returns
    // `String` (`@NotNull`), `equals`' `other` param is `@Nullable`, and a reference-typed `componentN`
    // return is `@NotNull`.
    if c.is_data {
        let not_null = "Lorg/jetbrains/annotations/NotNull;";
        let data_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
        // A `data object` synthesizes no `copy` (see the metadata assembly), so it takes no annotations.
        // A PRIVATE `copy` (its ctor's visibility under `DataClassCopyRespectsConstructorVisibility`)
        // takes none either: kotlinc omits nullability annotations — return and parameters — on the
        // now-private method.
        let copy = data_copy_fid(ir, c);
        let copy_is_private = copy.is_some_and(|fid| ir.method_visibility(fid).is_private());
        if !data_fields.is_empty() && !copy_is_private {
            let fid = copy.expect("a non-singleton data class records its generated copy identity");
            let function = &ir.functions[fid as usize];
            // `copy`'s parameters mirror the primary-constructor properties, so each reference param
            // takes the SAME `@NotNull`/`@Nullable` annotation kotlinc puts on the constructor's.
            let copy_params: Vec<Option<&str>> = (0..data_fields.len())
                .map(|index| declared_property_annotation(c, index))
                .collect();
            cw.set_method_nullability(
                &function.name,
                &ir_method_desc(&function.params, &function.ret),
                Some(not_null),
                &copy_params,
            );
        }
        if let Some(fid) = ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::ToString) {
            let function = &ir.functions[fid as usize];
            cw.set_method_nullability(
                &function.name,
                &ir_method_desc(&function.params, &function.ret),
                Some(not_null),
                &[],
            );
        }
        if let Some(fid) = ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::Equals) {
            let function = &ir.functions[fid as usize];
            cw.set_method_nullability(
                &function.name,
                &ir_method_desc(&function.params, &function.ret),
                None,
                &[Some("Lorg/jetbrains/annotations/Nullable;")],
            );
        }
        for i in 0..data_fields.len() {
            if let Some(a) = declared_property_annotation(c, i) {
                let fid = ir
                    .data_class_member(c.fq_name_id(), IrDataClassMemberRole::Component(i as u32))
                    .expect("a data-class component records its generated identity");
                // Like a private `copy`, a private `componentN` carries no Java nullability
                // annotation. Its Kotlin nullability remains authoritative in `@Metadata`.
                if ir.method_visibility(fid).is_private() {
                    continue;
                }
                let function = &ir.functions[fid as usize];
                cw.set_method_nullability(
                    &function.name,
                    &ir_method_desc(&function.params, &function.ret),
                    Some(a),
                    &[],
                );
            }
        }
    }
}

/// Emit the companion/static surface shared by declarations that are JVM interfaces: Kotlin
/// interfaces and annotation classes. Common IR keeps those source kinds distinct, but both use
/// interface field constraints and the companion's self-hosted `$$INSTANCE` realization.
fn emit_jvm_interface_companion_surface(
    ir: &IrFile,
    c: &IrClass,
    facade: &str,
    env: &EmitEnv,
    cw: &mut ClassWriter,
) {
    let fq_name = c.fq_name();
    delegated_property_array::declare_in_interface(env, c.fq_name, cw);
    jvm_static_members::emit_companion_bridges(ir, c, cw, env);

    let clinit_statics: Vec<(u32, &crate::ir::IrStatic, crate::ir::ExprId)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_matches(&fq_name))
        .filter_map(|(index, s)| Some((index as u32, s, static_fields::clinit_initializer(ir, s)?)))
        .collect();
    let array = delegated_property_array::exists(env, c.fq_name);
    if c.companion_class.is_some() || !clinit_statics.is_empty() || array {
        cw.reserve_method_name("<clinit>");
        cw.seed_utf8("()V");
        let mut emitter = Emitter::new(
            ir,
            cw,
            env,
            Some(StaticOwner::Class(c.fq_name)),
            &fq_name,
            facade,
            Ty::Unit,
            clinit_statics.iter().map(|&(_, _, init)| init),
        );
        emitter.enter_class_initializer(env.signature_symbols);
        let mut clinit = CodeBuilder::new(0);
        emitter.emit_delegated_property_array(env, c.fq_name, &fq_name, &mut clinit);
        if array {
            emitter.cw.omit_method_lines("<clinit>", "()V");
        }
        emit_companion_init(emitter.cw, &mut clinit, &fq_name, c);
        let mut clinit_lines = Vec::new();
        for &(static_index, s, init) in &clinit_statics {
            let pc = clinit.bytes.len() as u16;
            if let Some(&line) = c
                .companion_class
                .as_ref()
                .and_then(|companion| ir.prop_decl_lines.get(&(*companion, s.name.clone())))
            {
                if line != 0 {
                    clinit_lines.push((pc, line));
                }
            }
            emitter.emit_static_initializer_store(&fq_name, static_index, init, &mut clinit);
        }
        clinit.ret_void();
        clinit.ensure_locals(emitter.frame.max());
        clinit.link();
        emitter.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        if !clinit_lines.is_empty() {
            emitter
                .cw
                .set_method_lines("<clinit>", "()V", &clinit_lines);
        }
    }

    add_companion_field(cw, c);
    // `<clinit>` already interned `Companion`. These constants follow it in the field table, and
    // their names, descriptors, and `ConstantValue` payloads intern together at the field visit —
    // after that alias store, before `@Metadata`. A hoisted `@JvmField` property is visited later
    // because kotlinc places it after both.
    for property in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, property)| {
            property.owner_matches(&fq_name) && !ir.is_jvm_field_static(*index as u32)
        })
        .map(|(_, property)| property)
    {
        let descriptor = ir_type_desc(&property.ty);
        let value = property
            .init
            .and_then(|init| static_fields::constant_value(ir, init));
        let nullability = (descriptor.starts_with('L') || descriptor.starts_with('[')).then(|| {
            if property.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        cw.add_field_late(0x0019, &property.name, &descriptor, value, nullability);
    }

    // A hoisted `@JvmField` property is the public static field itself. Its declaration annotations
    // remain owned by the companion in common IR and are copied onto this target field here.
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| s.owner_matches(&fq_name) && ir.is_jvm_field_static(*index as u32))
        .map(|(_, s)| s)
    {
        let descriptor = ir_type_desc(&s.ty);
        let nullability = (descriptor.starts_with('L') || descriptor.starts_with('[')).then(|| {
            if s.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        cw.add_field_late(0x0019, &s.name, &descriptor, None, nullability);
        if let Some(annotations) = c
            .companion_class
            .and_then(|companion| ir.class_id_by_name(companion))
            .and_then(|companion| {
                ir.classes[companion as usize]
                    .field_annotations
                    .iter()
                    .find(|annotations| annotations.field == s.name)
            })
        {
            cw.set_last_late_field_annotations(&annotations.annotations);
        }
    }
}

/// Construct a `ClassWriter` with the per-file [`EmitOptions`] stamped on — the single place emission
/// builds a writer, so class version + `SourceFile` reach every class (incl. synthetics) explicitly.
fn new_writer(internal: &str, super_internal: &str, opts: &EmitOptions) -> ClassWriter {
    new_writer_generic(internal, None, super_internal, opts)
}

/// The writer for a DECLARED classifier — class, data class, object, interface, enum, enum-entry
/// subclass, annotation implementation. Which classifiers publish a JVM class `Signature` is decided
/// here, once, and never by the declaration's kind at the call site: a generic interface carries one
/// exactly like a generic class, and an enum carries the one its implicit parameterized superclass
/// `java/lang/Enum<E>` gives it even when the declaration itself is not generic.
fn new_classifier_writer(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    super_internal: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> ClassWriter {
    let formatter = JvmSignatureFormatter::new(ir, env);
    let recorded = ir.lowered_class_signature(c.fq_name);
    let signature = if c.is_enum {
        jvm_enum_class_signature(&formatter, c, recorded)
    } else {
        recorded.and_then(|signature| jvm_class_signature(&formatter, signature))
    };
    let signature = supertype_markers::with_markers(signature, ir, c);
    let internal = c.fq_name();
    // kotlinc (ASM) visits `(name, signature, superName)`, so the signature VALUE interns between
    // the two class names — it must reach the writer's constructor, not only `set_signature`.
    let mut cw = new_writer_generic(&internal, signature.as_deref(), super_internal, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    // kotlinc publishes no nullability in a script's classes either.
    cw.set_nullability_annotations(
        !crate::metadata::local_classifiers::is_local(ir, c) && !ir.is_script(),
    );
    cw
}

/// The JVM realizes every Kotlin enum as `java.lang.Enum<Self>`. That parameterized superclass is
/// target representation, not a source supertype, so it does not belong in checked FIR or common IR.
/// Build the classfile `Signature` here and append the declaration's semantic interfaces. When a
/// parameterized interface caused common IR to retain a class signature, consume that exact type;
/// otherwise the classifier's direct interface identities are sufficient for their raw signature.
fn jvm_enum_class_signature(
    formatter: &JvmSignatureFormatter<'_>,
    class: &crate::ir::IrClass,
    recorded: Option<&crate::ir::IrGenericSig>,
) -> Option<String> {
    let mut signature = recorded.map_or_else(String::new, |generic| {
        jvm_type_params(formatter, generic).unwrap_or_default()
    });
    signature.push_str("Ljava/lang/Enum<L");
    signature.push_str(&class.fq_name());
    signature.push_str(";>;");

    if let Some(generic) = recorded {
        for supertype in generic.supers.iter().skip(1) {
            signature.push_str(&formatter.ty_at(supertype, Wildcards::Supertype)?);
        }
    } else {
        for interface in class.interfaces.iter_ids() {
            signature.push_str(&formatter.ty_at(&Ty::obj_name(interface), Wildcards::Supertype)?);
        }
    }
    Some(signature)
}

/// [`new_writer`] for a class with a generic `Signature`, so the signature value interns in kotlinc's
/// position (between the class and superclass names).
fn new_writer_generic(
    internal: &str,
    signature: Option<&str>,
    super_internal: &str,
    opts: &EmitOptions,
) -> ClassWriter {
    let mut cw = ClassWriter::new_generic(internal, signature, super_internal);
    if let Some(major) = opts.class_major {
        cw.set_major(major);
    }
    cw.set_source_file(opts.source_file.clone());
    cw.set_param_assertions(opts.param_assertions);
    cw.set_inner_class_resolver(opts.inner_class_resolver.clone());
    cw.set_value_classes(opts.value_classes.clone());
    cw
}

/// Emit a whole IR file: the facade class of top-level `static` functions, plus one `.class` per
/// `IrClass`. Returns `(internal_name, bytes)` for each, or `None` when the IR uses a construct the
/// JVM backend can't represent (so every emission path skips it rather than miscompiling).
pub(crate) fn emit_all_with_checked_classifiers(
    ir: &IrFile,
    (facade_class, facade): (TypeName, &str),
    bodies: &dyn MethodBodies,
    facts: CheckedEmitFacts<'_>,
    opts: &EmitOptions,
    run: &EmitRun,
) -> Option<Vec<(String, Vec<u8>)>> {
    let env = EmitEnv {
        facade_class,
        multifile_facade: opts.multifile_facade.as_deref(),
        bodies: &bytecode_inline_call::ModuleClassBodies::new(bodies, run),
        run,
        continuation_metadata: facts.metadata.continuations,
        emit_time_machines: facts.metadata.emit_time_machines,
        suspended_result_returns: facts.metadata.suspended_result_returns,
        intrinsic_probe_continuations: facts.metadata.intrinsic_probe_continuations,
        bridge_adaptations: facts.metadata.bridge_adaptations,
        function_argument_arrays: facts.metadata.function_argument_arrays,
        override_results: facts.metadata.override_results,
        collection_method_entry_barriers: facts.metadata.collection_method_entry_barriers,
        collection_members: facts.metadata.collection_members,
        signature_symbols: facts.signature_symbols,
        dependency_callables: facts.dependency_callables,
        dispatch_classifiers: std::rc::Rc::new(
            crate::jvm::member_dispatch::CheckedDispatchClassifiers::new(
                ir,
                facts.signature_symbols,
            ),
        ),
        jvm_default: opts.jvm_default,
        lambda_modes: opts.lambda_modes,
        java_parameters: opts.java_parameters,
        inline_string_concat: opts.inline_string_concat,
        inline_scopes_numbers: opts.inline_scopes_numbers,
        metadata_stamp: opts.metadata_stamp(),
        property_realizations: facts.property_realizations,
        property_reference_realizations: facts.property_reference_realizations,
        sam_wrapper_realizations: facts.sam_wrapper_realizations,
        local_delegate_access: facts.local_delegate_access,
        default_call_operands: facts.default_call_operands,
        inner_classes: crate::jvm::inner_classes::InnerClasses::new(
            ir,
            facts.metadata.override_results,
            facade,
            opts.lambda_modes,
        ),
    };
    emit_all_with_class_meta(ir, facade, &env, facts.metadata.facade, opts, &|_| None)
}

/// `class_meta` may supply per-class `@kotlin.Metadata` keyed by the class's internal name. This
/// lets a separately compiled module expose its Kotlin member signatures to dependent modules.
fn emit_all_with_class_meta(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    // The emitter recurses over IR values (`emit_value_node` → `emit_cond_branch`/`emit_when` → …)
    // as deep as the lowered expression tree, whose depth the lowering pass bounds at 500. Run on a
    // sufficiently large same-thread stack segment, so the guard — not the calling thread's
    // stack — limits expression nesting without changing thread-local behavior (see
    // [`crate::wide_stack`]).
    crate::wide_stack::on_wide_stack(move || {
        emit_all_with_class_meta_impl(ir, facade, env, metadata, opts, class_meta)
    })
}

fn emit_all_with_class_meta_impl(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    let opts = &EmitOptions {
        value_classes: std::rc::Rc::new(value_class_descriptors::of(ir)),
        ..opts.clone()
    };
    // Pass 1 (discovery): emit everything, recording live closure implementations and the subset that
    // actually uses `invokedynamic`. A lambda spliced by the inliner emits neither realization.
    env.run.used_lambdas.borrow_mut().clear();
    env.run.used_indy_lambdas.borrow_mut().clear();
    env.run.dead_lambdas.borrow_mut().clear();
    let empty = crate::fx_hash::HashSet::default();
    let first = emit_pass(
        ir,
        facade,
        env,
        metadata,
        opts,
        class_meta,
        &LambdaSelection {
            dead: &empty,
            rescued: &empty,
        },
    )?;
    let used = env.run.used_lambdas.borrow().clone();
    let used_indy = env.run.used_indy_lambdas.borrow().clone();
    // `invokedynamic` was added in class-file version 51 (Java 7). Do not return a version-50 class
    // containing an opcode that its declared target cannot represent. This check uses the emitter's
    // discovery result rather than the source/IR lambda count: an inline-only lambda that was fully
    // spliced emitted no call site and therefore remains valid for the older target.
    if opts.class_major.is_some_and(|major| major < 51) && !used_indy.is_empty() {
        env.run
            .set_emit_error("krusty: invokedynamic requires JVM target 1.7 or newer".to_string());
        return None;
    }
    // A MUST-INLINE message lambda whose call-site splice FELL BACK to a real `invokedynamic`
    // (pass 1 recorded the use): its impl was pre-marked `inline_only` on the assumption the splice
    // always succeeds — emitting the reference without the method would be a broken class
    // (`NoSuchMethodError`). RESCUE it: re-emit with the impl method present. (A bare-return impl is
    // never rescued — it is not a valid standalone method — and is not in `must_inline_lambdas`.)
    let rescued: crate::fx_hash::HashSet<u32> = used
        .iter()
        .copied()
        .filter(|fid| ir.must_inline_lambdas.contains(fid))
        .collect();
    // Dead = a lambda impl that no emitted closure references. Discovery emits facade and class code,
    // so this applies equally to helpers owned by a class: a lambda spliced into a constructor or
    // accessor must not leave a dead `$lambda` method behind. NB single iteration: an indy inside a
    // dead lambda still marks its inner lambda used, so a nested-dead chain keeps the inner method —
    // rare, and conservative.
    // An anonymous function's impl is reached by an `invokestatic` the SPLICE emits, never by an
    // `invokedynamic` — discovery would otherwise read it as dead and drop the method the splice calls.
    let spliced_as_a_call = splice_called_impls(ir);
    let function_reference_targets: crate::fx_hash::HashSet<u32> = ir
        .classes
        .iter()
        .filter_map(|class| class.func_ref.as_ref())
        .filter_map(|reference| function_reference_target(ir, reference))
        .collect();
    let dead: crate::fx_hash::HashSet<u32> = ir
        .lambda_own_params_from
        .keys()
        .filter(|&&fid| {
            !used.contains(&fid)
                && !spliced_as_a_call.contains(&fid)
                && !function_reference_targets.contains(&fid)
                && !ir.inline_only_fns.contains(&fid)
                && ir
                    .functions
                    .get(fid as usize)
                    .is_some_and(|f| f.dispatch_receiver.is_none())
        })
        .copied()
        .collect();
    // A coroutine machine the emission owns is built on the second pass, from what the first one
    // learned about the post-splice frame: those functions need the re-emit whether or not any
    // lambda turned out dead.
    let machines_to_build = !env.run.machine_plans.borrow().is_empty();
    if dead.is_empty() && rescued.is_empty() && !machines_to_build {
        return Some(first);
    }
    env.run.dead_lambdas.borrow_mut().clone_from(&dead);
    // Pass 2: re-emit without the dead facade impls, plus any rescued must-inline impls
    // (deterministic — identical decisions, minus the dead methods, plus the rescued ones; the
    // facade itself drops when the dead impls were its only members).
    emit_pass(
        ir,
        facade,
        env,
        metadata,
        opts,
        class_meta,
        &LambdaSelection {
            dead: &dead,
            rescued: &rescued,
        },
    )
}

/// Which facade-owned lambda impls a pass drops (`dead`) or keeps despite a pre-marked inline
/// (`rescued`) — the only state that differs between emit pass 1 (both empty) and pass 2.
struct LambdaSelection<'a> {
    dead: &'a crate::fx_hash::HashSet<u32>,
    rescued: &'a crate::fx_hash::HashSet<u32>,
}

fn emit_pass(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
    lambdas: &LambdaSelection,
) -> Option<Vec<(String, Vec<u8>)>> {
    // `emit_all_with_class_meta_impl` may run a discovery pass and then discard all of its bytes in
    // favor of a second pass without dead lambda implementations. Plans are drained while each
    // enclosing class is emitted, and `lambda_classes_written` suppresses duplicate realizations
    // within that pass. Neither accumulator may leak across the discard boundary: otherwise the
    // second pass emits a call site for a class that only existed in the discarded first output.
    env.run.lambda_classes.borrow_mut().clear();
    env.run.lambda_classes_written.borrow_mut().clear();
    env.run.machine_classes.borrow_mut().clear();
    env.run.regenerated_object_names.clear();
    env.run.when_mappings.borrow_mut().clear();
    env.run.entries_mappings.borrow_mut().clear();
    if !jvm_can_emit(ir) {
        emission_readiness::trace_residual_nodes(ir);
        return None;
    }
    *env.run.inline_bail.borrow_mut() = None;
    env.run.emit_bail.set(false);
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver). A function that BELONGS
    // to a class — including a `static` member like the serialization plugin's `serializer()` accessor,
    // which has no dispatch receiver — is emitted on its class (below), NOT here; otherwise two classes'
    // same-signature statics (`C.serializer()`/`D.serializer()`) would collide on the facade.
    let class_member_fids: crate::fx_hash::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    let contexts = static_accessors::emission_contexts(ir, &class_member_fids);
    let member_access_bridges = access_bridges::cross_owner_member_calls(
        ir,
        facade,
        &contexts,
        opts.jvm_default != JvmDefaultMode::Disable,
    );
    env.run
        .private_member_access_bridges
        .borrow_mut()
        .clone_from(&member_access_bridges.private);
    env.run
        .protected_member_access_bridges
        .borrow_mut()
        .clone_from(&member_access_bridges.protected);
    *env.run.static_accessor_plan.borrow_mut() = static_accessors::plan(
        ir,
        env,
        &contexts,
        &class_member_fids,
        &member_access_bridges.private_calls,
    );
    bytecode_inline_call::compile_inline_callees(ir, facade, env, opts, class_meta);
    let mut cw = new_writer(facade, "java/lang/Object", opts);
    // The facade constructs the file's local classes, and a class that references one as a class
    // constant must list it in `InnerClasses` — reflection cross-checks the two sides and throws
    // `IncompatibleClassChangeError` when only one carries the entry. kotlinc emits it here too.
    env.inner_classes.register(&mut cw);
    let mut facade_has_method = false;
    let facade_functions = ir.functions.iter().enumerate().filter_map(|(i, f)| {
        let i = i as u32;
        // Inline-only lambda impls (spliced) and dead ones (inlined at every use) are not facade
        // methods: counting them would emit an empty facade for a class-only file.
        let emitted = !class_member_fids.contains(&i)
            && f.dispatch_receiver.is_none()
            && f.body.is_some()
            && (!ir.inline_only_fns.contains(&i) || lambdas.rescued.contains(&i))
            && !lambdas.dead.contains(&i);
        emitted.then_some(i)
    });
    for member in member_schedule::facade_source_ordered_members(ir, facade_functions) {
        let i = match member {
            member_schedule::FacadeMember::Function(function) => function as usize,
            member_schedule::FacadeMember::DefaultAccessor(static_index, accessor) => {
                static_fields::emit_default_static_accessor(
                    ir,
                    facade,
                    &mut cw,
                    env,
                    opts.param_assertions,
                    static_index,
                    accessor,
                );
                continue;
            }
        };
        let f = &ir.functions[i];
        let rescued = lambdas.rescued.contains(&(i as u32));
        if rescued {
            // A rescued must-inline impl IS emitted despite its `inline_only` mark (see
            // `emit_all_with_class_meta`) — bypass the early return.
            emit_method_inner(
                ir,
                i as u32,
                StaticOwner::Facade,
                facade,
                facade,
                &mut cw,
                false,
                env,
            );
        } else {
            emit_method(
                ir,
                i as u32,
                StaticOwner::Facade,
                facade,
                facade,
                &mut cw,
                false,
                env,
            );
        }
        // A facade has no class declaration to close on.
        function_debug::attach_declared_function_debug(
            ir,
            env.override_results,
            i as u32,
            facade,
            &mut cw,
        );
        facade_has_method = true;
        // A PARAMETERLESS `fun main()` is not a JVM entry point on its own: the launcher looks for
        // `main([Ljava/lang/String;)V`, and the no-arg form is only recognized by JEP 445 (Java 21+
        // preview, final in 25). kotlinc therefore emits a synthetic bridge that calls it, so the
        // output runs on any JVM. A declared `fun main(args: Array<String>)` IS the entry point and
        // gets no bridge; a `suspend fun main` has a continuation parameter after lowering and so is
        // not parameterless here (its bridge needs the coroutine runner and is not emitted yet).
        if f.name == "main" && f.params.is_empty() && matches!(f.ret, Ty::Unit) {
            // kotlinc visits the bridge's header before its body's call to `main()`.
            cw.reserve_method_pool("main", "([Ljava/lang/String;)V", None, &[]);
            let mut bridge = CodeBuilder::new(1);
            let target = cw.methodref(facade, "main", "()V");
            bridge.invokestatic(target, 0, 0);
            bridge.ret_void();
            bridge.ensure_locals(1);
            bridge.link();
            cw.add_method(
                0x1009, // PUBLIC | STATIC | SYNTHETIC
                "main",
                "([Ljava/lang/String;)V",
                &bridge,
            );
            cw.set_method_debug(
                "main",
                "([Ljava/lang/String;)V",
                None,
                &[("args".to_string(), "[Ljava/lang/String;".to_string(), 0)],
            );
        }
        // A top-level function (or extension) with parameter defaults gets kotlinc's
        // `foo$default(params…, int mask, Object marker)` synthetic (dispatches to the real method,
        // filling the masked slots from the defaults), so an omitted-argument caller — same-file or
        // cross-module — resolves against the same ABI kotlinc emits. A default the synthetic frame
        // cannot re-emit is skipped (`toplevel_default_stub_safe`).
        if let Some(defaults) = crate::jvm::default_wrappers::top_level_function(ir, i as u32) {
            // A top-level function's `$default` marker is a plain `Object` (kotlinc's function ABI).
            emit_facade_default_stub(
                DefaultStubSource {
                    ir,
                    fid: i as u32,
                    defaults,
                },
                StaticOwner::Facade,
                facade,
                &mut cw,
                env,
                Ty::obj("java/lang/Object"),
            );
        } else if ir.has_param_defaults(i as u32) {
            crate::trace_compiler!(
                "lower",
                "no $default stub for {}: defaults {:?}",
                f.name,
                ir.param_defaults(i as u32).map(|ds| ds
                    .iter()
                    .map(|d| d.map(|d| format!(
                        "{:?} logical={:?}",
                        ir.exprs[d as usize],
                        ir.logical_types.get(&d)
                    )))
                    .collect::<Vec<_>>())
            );
        }
    }
    // A multifile facade forwards to the part's declared members and their `$default` stubs,
    // never to the synthetic accessors emitted below.
    if env.multifile_facade.is_some() {
        env.run.multifile_part.record_members(cw.declared_methods());
    }
    // A facade's accessors map to the file's first line.
    static_accessors::emit(
        ir,
        env.run,
        &env.run.static_accessor_plan.borrow(),
        StaticOwner::Facade,
        facade,
        1,
        &mut cw,
    );
    static_fields::emit_statics(ir, facade, &mut cw, env);
    // kotlinc emits the `<File>Kt` facade class ONLY when the file has top-level callables/properties
    // (or a facade `@Metadata` payload). A file of only classes/objects gets no facade — emitting an
    // empty one is an ABI divergence (spurious extra class). A facade static is owner-less.
    let facade_has_static = ir.statics.iter().any(|s| s.is_facade_owned());
    if facade_has_method || facade_has_static || metadata.is_some() {
        if let Some(multifile_facade) = env.multifile_facade {
            multifile_facades::write_part_header(&mut cw, ir, metadata, multifile_facade);
        } else {
            cw.set_class_annotations(&ir.file_annotations);
            if let Some(m) = metadata {
                cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
            }
        }
        out.push((facade.to_string(), env.run.finish_class(cw)));
        out.extend(drain_lambda_classes(env, opts));
        out.extend(env.run.machine_classes.borrow_mut().drain(..));
    }
    let unwritten_continuations = transformed_suspensions::unwritten_continuations(ir, env);
    // Each class — with its optional `@Metadata` (the provider returns `None` for the default emit).
    for (class_id, c) in ir.classes.iter().enumerate() {
        let class_id = class_id as ClassId;
        let fq_name = c.fq_name();
        // A function whose suspension points all turned out to be tail calls has no machine.
        if let Some(crate::jvm::classfile::CoroutineOutcome::TailCalls) =
            env.run.transformed_coroutine(&fq_name)
        {
            continue;
        }
        if unwritten_continuations.contains(&c.fq_name) {
            continue;
        }
        // With an interface's `$DefaultImpls` holder (its `name$default` synthetics), if any.
        bytecode_inline_call::write_class(ir, class_id, facade, env, opts, class_meta, &mut out);
        out.extend(drain_lambda_classes(env, opts));
        // A member's coroutine machine builds its continuation class while the method is emitted,
        // which is after the facade drained the ones its own top-level functions produced.
        out.extend(env.run.machine_classes.borrow_mut().drain(..));
    }
    out.extend(drain_lambda_classes(env, opts));
    out.extend(env.run.machine_classes.borrow_mut().drain(..));
    out.extend(env.run.when_mappings.borrow().write_classes(env, opts));
    out.extend(env.run.entries_mappings.borrow().write_classes(env, opts));
    if env.run.emission_failed() {
        return None; // malformed backend input — discard every class from this pass
    }
    Some(out)
}

/// Write one synthetic class for a SAM conversion that cannot use `invokedynamic`.
///
/// Kotlin function lambdas selected for class mode have already become IR classes. Keeping this
/// path SAM-only makes a missed realization an emission error instead of a second implementation.
fn build_lambda_class(plan: &LambdaClassPlan, opts: &EmitOptions) -> (String, Vec<u8>) {
    const OBJECT: &str = "java/lang/Object";
    let mut cw = new_writer_generic(&plan.internal, None, OBJECT, opts);
    cw.set_access(0x0030); // ACC_FINAL | ACC_SUPER
    cw.add_interface(&plan.iface);
    if plan.function_adapter {
        cw.add_interface("kotlin/jvm/internal/FunctionAdapter");
    }

    let field_descs: Vec<String> = plan.captures.iter().map(|t| type_descriptor(*t)).collect();
    for (index, desc) in field_descs.iter().enumerate() {
        // Captured values never change after construction.
        cw.add_field(0x0012, &format!("$captured${index}"), desc); // ACC_PRIVATE | ACC_FINAL
    }

    // <init>: store captures, then delegate to the superclass. `this` plus every capture must be
    // covered by `max_locals`, or the class fails verification before any of it runs.
    let capture_words: u16 = plan.captures.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + capture_words);
    let mut slot: u16 = 1;
    let mut capture_slots = Vec::new();
    for ty in &plan.captures {
        capture_slots.push(slot);
        slot += slot_words(*ty);
    }
    ctor.aload(0);
    let sup = cw.methodref(OBJECT, "<init>", "()V");
    ctor.invokespecial(sup, 0, 0);
    for (index, ty) in plan.captures.iter().enumerate() {
        ctor.aload(0);
        load_slot(&mut ctor, capture_slots[index], *ty);
        let field = cw.fieldref(
            &plan.internal,
            &format!("$captured${index}"),
            &field_descs[index],
        );
        ctor.putfield(field, slot_words(*ty) as i32 + 1);
    }
    ctor.ret_void();
    let ctor_desc = format!("({})V", field_descs.concat());
    cw.add_method(0x0000, "<init>", &ctor_desc, &ctor);

    // `invoke` at the interface's erased descriptor — the slot the JVM dispatches through. Captures
    // come off the fields, the arguments off the frame, and both are converted into the physical
    // types the implementation method declares.
    let (sam_params, sam_ret) = crate::jvm::names::parse_method_descriptor(&plan.sam_desc)
        .unwrap_or_else(|| (Vec::new(), "V"));
    let (impl_params, impl_ret) =
        parse_physical_method_desc(&plan.impl_desc).unwrap_or_else(|| (Vec::new(), Ty::Unit));
    let own_params = impl_params
        .get(plan.captures.len()..)
        .unwrap_or(&[])
        .to_vec();
    let mut invoke_locals: u16 = 1;
    let mut arg_slots = Vec::new();
    for param in &sam_params {
        arg_slots.push(invoke_locals);
        invoke_locals += if *param == "J" || *param == "D" { 2 } else { 1 };
    }
    let mut invoke = CodeBuilder::new(invoke_locals);
    for (index, ty) in plan.captures.iter().enumerate() {
        invoke.aload(0);
        let field = cw.fieldref(
            &plan.internal,
            &format!("$captured${index}"),
            &field_descs[index],
        );
        invoke.getfield(field, slot_words(*ty) as i32);
    }
    for (index, physical) in sam_params.iter().enumerate() {
        let want = own_params.get(index).copied().unwrap_or(Ty::Unit);
        if *physical == "J" {
            invoke.lload(arg_slots[index]);
        } else if *physical == "D" {
            invoke.dload(arg_slots[index]);
        } else if *physical == "F" {
            invoke.fload(arg_slots[index]);
        } else if descriptor_is_reference(physical) {
            invoke.aload(arg_slots[index]);
            let wanted = type_descriptor(want);
            if !matches!(want, Ty::Unit) && !descriptor_is_reference(&wanted) {
                // The erased slot carries a wrapper wherever the body wants a scalar.
                unbox_prim_from_descriptor(&mut cw, &mut invoke, physical, want);
            } else if wanted != *physical {
                // The implementation may have a more specific reference type than the SAM slot.
                let internal = wanted
                    .strip_prefix('L')
                    .and_then(|rest| rest.strip_suffix(';'))
                    .map(str::to_owned)
                    .unwrap_or(wanted);
                let target = cw.class_ref(&internal);
                invoke.checkcast(target);
            }
        } else {
            invoke.iload(arg_slots[index]);
        }
    }
    let impl_words: i32 = impl_params.iter().map(|t| slot_words(*t) as i32).sum();
    let impl_ref = if plan.owner_is_interface {
        cw.interface_methodref(&plan.impl_owner, &plan.impl_name, &plan.impl_desc)
    } else {
        cw.methodref(&plan.impl_owner, &plan.impl_name, &plan.impl_desc)
    };
    invoke.invokestatic(impl_ref, impl_words, slot_words(impl_ret) as i32);
    if descriptor_is_reference(sam_ret) && !descriptor_is_reference(&type_descriptor(impl_ret)) {
        box_prim_free(&mut cw, &mut invoke, impl_ret);
    }
    if sam_ret == "V" {
        match slot_words(impl_ret) {
            1 => invoke.pop(),
            2 => invoke.pop2(),
            _ => {}
        }
        invoke.ret_void();
    } else if descriptor_is_reference(sam_ret) {
        invoke.areturn();
    } else {
        return_primitive(&mut invoke, impl_ret);
    }
    // ACC_PUBLIC | ACC_FINAL: the interface slot, implemented once.
    cw.add_method(0x0011, &plan.sam_method, &plan.sam_desc, &invoke);
    class_mode_lambda_members::finish_lambda_class(cw, plan, &field_descs)
}

/// Take the lambda classes recorded while emitting the class just finished, and write them.
/// Draining per class keeps a synthetic next to its enclosing class in the output order.
fn drain_lambda_classes(env: &EmitEnv, opts: &EmitOptions) -> Vec<(String, Vec<u8>)> {
    let plans: Vec<LambdaClassPlan> = env.run.lambda_classes.borrow_mut().drain(..).collect();
    let mut written = env.run.lambda_classes_written.borrow_mut();
    let mut out = Vec::new();
    for plan in plans {
        // A body can be EMITTED more than once (a class property initializer is emitted into every
        // constructor) while still being one source lambda, and two plans must never resolve to the
        // same class name — the output is a plain list, so a duplicate would silently overwrite the
        // other on disk.
        if !written.insert((plan.impl_owner.clone(), plan.identity)) {
            continue;
        }
        out.push(build_lambda_class(&plan, opts));
    }
    out
}

/// Load a value of `ty` from a local slot — the constructor's capture parameters, which arrive in the
/// implementation's physical types rather than all as references.
fn load_slot(code: &mut CodeBuilder, slot: u16, ty: Ty) {
    match ty {
        Ty::Long => code.lload(slot),
        Ty::Double => code.dload(slot),
        Ty::Float => code.fload(slot),
        Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean => code.iload(slot),
        _ => code.aload(slot),
    }
}

/// Return a primitive result of `ty` (the non-reference SAM return case).
fn return_primitive(code: &mut CodeBuilder, ty: Ty) {
    match ty {
        Ty::Long => code.lreturn(),
        Ty::Double => code.dreturn(),
        Ty::Float => code.freturn(),
        Ty::Unit => code.ret_void(),
        _ => code.ireturn(),
    }
}

/// `(name, descriptor)` of a property's synthetic `$annotations` marker, read from the emitted
/// function so a value-class-mangled getter's marker is named as the class file spells it.
fn property_marker_signature(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    source_order: u32,
) -> Option<(String, String)> {
    let marker = ir
        .property_annotation_markers
        .get(&(c.fq_name_id(), source_order))?;
    let function = ir.functions.get(*marker as usize)?;
    Some((
        function.name.clone(),
        ir_method_desc(&function.params, &function.ret),
    ))
}

fn apply_enum_entry_annotations(cw: &mut ClassWriter, c: &crate::ir::IrClass, field: &str) {
    if let Some(annotations) = c.field_annotations.iter().find(|a| a.field == field) {
        cw.set_last_late_field_annotations(&annotations.annotations);
    }
}

/// The internal class name to `checkcast` a value to when narrowing an erased `Object` to `ty` — or
/// `None` when no narrowing is needed (`Object`/`Any`, a primitive, `Unit`/`Nothing`).
fn checkcast_internal(ty: Ty) -> Option<String> {
    match ty.kind() {
        TyKind::STRING => Some("java/lang/String".to_string()),
        _ if ty.is_array() => Some(type_descriptor(ty)),
        TyKind::Obj(n, _) if !crate::jvm::jvm_class_map::is_jvm_erased_top(n) => {
            Some(crate::jvm::names::classfile_internal_name_of(n).to_string())
        }
        _ => None,
    }
}

/// Synthesize the accessors for the properties a class DECLARES but whose accessor methods the IR does
/// not carry — a plain backing-field property has no source-written accessor, so `getX()`/`setX(v)` are
/// pure realization and belong here, not in the language-level lowering. A property that declares its own
/// accessor (computed, delegated, `field`-using) already has a method with a body and is skipped, as is a
/// private one (kotlinc emits no accessor for it) and any exact name+descriptor a real method already
/// occupies. Accessors are considered independently: a custom getter still needs its implicit default
/// setter synthesized, and a custom setter still needs its implicit default getter. A same-named method
/// with a different return descriptor does not hide the accessor.
#[derive(Clone, Copy)]
pub(super) enum PropertyAccessorSide {
    Getter,
    Setter,
}

pub(in crate::jvm) use declared_property_accessor::declared_signature as declared_property_accessor_signature;
pub(in crate::jvm) use static_fields::declared_default_static_accessor_signature;

/// What emitting one class's property accessors (and the `$annotations` markers beside them) needs
/// besides the IR: the class's JVM name, its file facade, the signature formatter, and the emit
/// environment. Grouped so the accessor walk stays within the argument-count limit, like
/// [`crate::metadata::class_builder::ClassTail`] does for the metadata writer.
struct PropertyAccessorEmit<'a> {
    fq_name: &'a str,
    facade: &'a str,
    formatter: &'a JvmSignatureFormatter<'a>,
    param_assertions: bool,
    env: &'a EmitEnv<'a>,
}

fn emit_declared_property_accessors(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    emit: &PropertyAccessorEmit<'_>,
) {
    let (fq_name, facade, formatter, param_assertions, env) = (
        emit.fq_name,
        emit.facade,
        emit.formatter,
        emit.param_assertions,
        emit.env,
    );
    let accessor_owner = declared_property_accessor::AccessorOwner {
        ir,
        class: c,
        fq_name,
        formatter,
        param_assertions,
        override_results: env.override_results,
        run: env.run,
    };
    for property in &c.properties {
        declared_property_accessor::emit(
            &accessor_owner,
            property,
            PropertyAccessorSide::Getter,
            cw,
        );
        declared_property_accessor::emit(
            &accessor_owner,
            property,
            PropertyAccessorSide::Setter,
            cw,
        );
        // The property's own annotations ride a synthetic marker method, emitted right here so the
        // method table (and the constant pool behind it) matches kotlinc's.
        if let Some(&marker) = ir
            .property_annotation_markers
            .get(&(c.fq_name_id(), property.source_order))
        {
            // The marker is STATIC (kotlinc's shape): no `this` slot.
            emit_method(
                ir,
                marker,
                StaticOwner::Class(c.fq_name),
                fq_name,
                facade,
                cw,
                false,
                env,
            );
        }
    }
}

/// What emitting one member of a class's source schedule needs besides the writer.
struct ScheduledMemberEmission<'a> {
    ir: &'a IrFile,
    class: &'a crate::ir::IrClass,
    fq_name: &'a str,
    facade: &'a str,
    signature_formatter: &'a JvmSignatureFormatter<'a>,
    param_assertions: bool,
    env: &'a EmitEnv<'a>,
    markers: &'a crate::fx_hash::HashSet<u32>,
    byte_parity: bool,
}

/// Emit one member of the class's source schedule: a property's accessors (and its annotation
/// marker), a secondary constructor, or a method with its access bridges and `$default` stub.
fn emit_scheduled_member(
    emission: &ScheduledMemberEmission<'_>,
    member: SourceOrderedMember<'_>,
    cw: &mut ClassWriter,
) {
    let ScheduledMemberEmission {
        ir,
        class: c,
        fq_name,
        facade,
        signature_formatter,
        param_assertions,
        env,
        markers,
        byte_parity,
    } = *emission;
    let fid = match member {
        SourceOrderedMember::Property(property) => {
            let accessor_owner = declared_property_accessor::AccessorOwner {
                ir,
                class: c,
                fq_name,
                formatter: signature_formatter,
                param_assertions,
                override_results: env.override_results,
                run: env.run,
            };
            declared_property_accessor::emit(
                &accessor_owner,
                property,
                PropertyAccessorSide::Getter,
                cw,
            );
            if let Some(getter) = property.getter {
                emit_scheduled_member(emission, SourceOrderedMember::Function(getter), cw);
            }
            declared_property_accessor::emit(
                &accessor_owner,
                property,
                PropertyAccessorSide::Setter,
                cw,
            );
            if let Some(setter) = property.setter {
                emit_scheduled_member(emission, SourceOrderedMember::Function(setter), cw);
            }
            // The property's own annotations ride a synthetic marker method, which kotlinc emits
            // directly after that property's accessors — it has no source order of its own.
            if let Some(&marker) = ir
                .property_annotation_markers
                .get(&(c.fq_name_id(), property.source_order))
            {
                // The marker is STATIC (kotlinc's shape): no `this` slot.
                emit_method(
                    ir,
                    marker,
                    StaticOwner::Class(c.fq_name),
                    fq_name,
                    facade,
                    cw,
                    false,
                    env,
                );
            }
            return;
        }
        SourceOrderedMember::StaticDefaultAccessor(static_index, accessor) => {
            static_fields::emit_default_static_accessor(
                ir,
                fq_name,
                cw,
                env,
                param_assertions,
                static_index,
                accessor,
            );
            return;
        }
        SourceOrderedMember::Function(fid)
            if markers.contains(&fid) || standalone_method_is_elided(ir, fid, env) =>
        {
            return;
        }
        SourceOrderedMember::Function(fid) => fid,
        SourceOrderedMember::SecondaryConstructor(ordinal, constructor) => {
            // Each `<init>(p)` delegates to an exact checked target, then runs its body. A
            // `super(…)`-reaching body already includes the class initialization steps.
            SecondaryConstructorEmitter {
                ir,
                class: c,
                owner: fq_name,
                facade,
                env,
                writer: cw,
                owner_prefix: &OwnerConstructorPrefix::none(),
            }
            .emit(ordinal, constructor);
            return;
        }
    };
    let f = &ir.functions[fid as usize];
    if f.body.is_some() {
        // A `static` member (e.g. a value class's `box-impl`/`constructor-impl`) emits with no
        // `this` slot; an ordinary member is an instance method.
        emit_method(
            ir,
            fid,
            StaticOwner::Class(c.fq_name),
            fq_name,
            facade,
            cw,
            !f.is_static,
            env,
        );
        seed_value_class_method_locals(byte_parity, ir, c, fid, cw);
        if ir.function_reference_access_bridges.contains(&fid) {
            access_bridges::emit_function_reference_access_bridge(
                ir,
                fid,
                fq_name,
                cw,
                false,
                static_accessors::class_declaration_line(c),
            );
        }
        bridge_emission::emit_value_class_interface_entries(
            ir,
            c,
            cw,
            fid,
            signature_formatter,
            env,
        );
    } else {
        // An abstract member is still a source declaration. Its annotations are the same payload a
        // concrete member writes from `emit_method`; omitting them drops `@Deprecated` on an
        // interface or abstract-class member.
        function_annotations::add_abstract(ir, cw, fid, signature_formatter, env);
    }
    // A method with default-valued parameters gets a `<name>$default(…, mask, marker)` synthetic stub
    // (the JVM realization of default arguments). A STATIC method (a value class's `constructor-impl`)
    // has no `self`, so it uses the facade-style stub keyed on the class as owner; an instance member
    // uses the self-carrying variant.
    if let Some((fid, defaults)) = crate::jvm::default_wrappers::member_function(ir, fid) {
        let f = &ir.functions[fid as usize];
        if f.is_static {
            let marker = static_default_stub_marker(ir, fid);
            emit_facade_default_stub(
                DefaultStubSource { ir, fid, defaults },
                StaticOwner::Class(c.fq_name),
                fq_name,
                cw,
                env,
                marker,
            );
        } else {
            emit_default_stub(
                DefaultStubSource { ir, fid, defaults },
                Some(StaticOwner::Class(c.fq_name)),
                fq_name,
                facade,
                cw,
                env,
                false,
            );
        }
    }
}

/// The `ACC_PUBLIC` bit a class's own access flags carry. A `private` declaration — of ANY kind, at
/// top level or nested — is package-private in the class file: the JVM has no class-level private,
/// so kotlinc drops the bit and records the real visibility in `@Metadata` and `InnerClasses`.
/// `internal` stays public (the module boundary is a Kotlin-only fact).
fn class_public_bit(ir: &IrFile, c: &crate::ir::IrClass) -> u16 {
    match ir.class_visibilities.get(&c.fq_name_id()) {
        Some(crate::types::Visibility::Private) => 0x0000,
        _ => 0x0001,
    }
}

/// Every method this class emits must carry a DETERMINED signature.
///
/// `Ty::Error` and `Ty::Pending` are answers about resolution, not types, and neither survives the
/// descriptor: both erase to `java/lang/Object`. An override whose return did not resolve therefore
/// emits `(Ljava/lang/Object;)Ljava/lang/Object;` where the interface declares `(…)V`, which is a
/// DIFFERENT method — the class verifies, loads, and then throws `AbstractMethodError` at the first
/// call. Nothing between the frontend and the JVM notices, so the invariant is asserted here, where
/// the signature is fixed, rather than hoped for. The `@Metadata` encoder asserts the same thing for
/// the classes that carry metadata; an anonymous object carries none, which is exactly the shape
/// that reached a real JVM as `AbstractMethodError`.
fn assert_determined_member_signatures(ir: &IrFile, c: &crate::ir::IrClass) {
    let undetermined = |ty: &Ty| ty.mentions_pending() || ty.mentions_error();
    for &fid in &c.methods {
        let Some(function) = ir.functions.get(fid as usize) else {
            continue;
        };
        if let Some(ty) = function
            .params
            .iter()
            .chain(std::iter::once(&function.ret))
            .find(|ty| undetermined(ty))
        {
            panic!(
                "undetermined type {ty:?} in the emitted signature of {}.{}: a resolution answer is \
                 not a type, and erases to `java/lang/Object` in the descriptor",
                c.fq_name(),
                function.name,
            );
        }
    }
}

fn emit_class(
    ir: &IrFile,
    class_id: ClassId,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> Vec<u8> {
    assert_determined_member_signatures(ir, c);
    if c.is_enum {
        return emit_enum_class(ir, c, facade, env, opts);
    }
    if let Some(iface) = &c.annotation_impl_of {
        return emit_annotation_impl_class(ir, c, &iface.render(), facade, env, opts);
    }
    if c.is_annotation {
        return annotation_interface::emit_annotation_class(ir, c, facade, env, opts, class_meta);
    }
    if c.is_interface {
        return emit_interface_class(ir, c, facade, env, opts, class_meta, extra);
    }
    if c.is_enum_entry {
        return enum_entry_subclass::emit_enum_entry_subclass(ir, c, facade, env, opts);
    }
    if c.prop_ref.is_some() {
        return property_reference_class::emit_prop_ref_class(ir, c, facade, env, opts);
    }
    if c.func_ref.is_some() {
        return function_reference_class::emit_func_ref_class(ir, c, facade, env, opts);
    }
    if let Some(lambda) = &c.lambda {
        return lambda_class::emit_lambda_class(ir, c, lambda, facade, env, opts);
    }
    if let Some(wrapper) = &c.sam_wrapper {
        return sam_wrapper_class::emit_sam_wrapper_class(ir, c, wrapper, facade, env, opts);
    }
    if let Some(lambda) = env.emit_time_machines.suspend_lambda(c.fq_name_id()) {
        return suspend_lambda_class::emit_suspend_lambda_class(ir, c, lambda, facade, env, opts);
    }
    let fq_name = c.fq_name();
    let superclass = c.superclass();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let mut cw = new_classifier_writer(ir, c, &superclass, env, opts);
    // A LOCAL or ANONYMOUS class carries kotlinc's `EnclosingMethod` attribute: without it
    // reflection reads the class as top-level and `simpleName` reports the whole `owner$Local` name.
    // Lowering records the exact semantic scope; the backend realizes its owner and descriptor.
    if let Some((owner, method)) = class_enclosure(ir, env.override_results, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    enclosure::start_inline_body_source_map(ir, c, &mut cw);
    let transformed = env.run.transformed_coroutine(&fq_name);
    let transformed_metadata = env
        .continuation_metadata
        .get(&fq_name)
        .and_then(|metadata| {
            transformed_suspensions::continuation_metadata(metadata, transformed.as_ref()?)
        });
    let continuation_metadata = transformed_metadata
        .as_ref()
        .or(env.continuation_metadata.get(&fq_name));
    if let Some(metadata) = continuation_metadata {
        cw.set_enclosing_method(
            &metadata.enclosing_class,
            &metadata.enclosing_method,
            &metadata.enclosing_descriptor,
        );
    }
    sealed_subtypes::register(
        &mut cw,
        ir,
        c,
        opts.class_major.unwrap_or(MAJOR_JAVA8) >= 61,
    );
    env.inner_classes.register(&mut cw);
    // The class HEADER's interface refs intern BEFORE any member entry (kotlinc visits the header
    // first — `object Fast : Factory` pool: this, super, `lib/Factory`, then `<init>`), so add them
    // ahead of the pool seeding below.
    supertype_markers::add_interfaces(&mut cw, ir, c);
    // Seed the constant pool in kotlinc's interning order for a plain property class that will carry a
    // computed `@Metadata` + debug tables — so the emitted class is byte-identical, not just
    // structurally equal. Gated exactly like the debug tables (opt-in, non-data, qualifying shape).
    // A cross-module `class_meta` PROVIDER record (none exists today) deliberately does NOT seed:
    // provider metadata makes the class correct, not byte-identical — byte identity is only claimed
    // for the computed path this gate mirrors.
    // For a generic class, the `<init>` carries a `Signature` whose type-parameter params read `T<tp>;`
    // (`class Box<T>(var a: T)` → `(TT;)V`); a PARAMETERIZED concrete-type param reads its full generic
    // signature (`List<String>` → `Ljava/util/List<Ljava/lang/String;>;`). `None` when no param needs it.
    // Computed once here: the pool seeder interns it and the `<init>` emission attaches it.
    let is_continuation = is_continuation_class(c);
    let has_continuation_receiver = c.ctor_args.iter().any(|argument| {
        argument.provenance == crate::ir::IrCtorParameterProvenance::ContinuationDispatchReceiver
    });
    let ctor_signature = continuation_metadata
        .map(|metadata| {
            if has_continuation_receiver {
                format!(
                    "(L{};Lkotlin/coroutines/Continuation<-L{fq_name};>;)V",
                    metadata.enclosing_class
                )
            } else {
                format!("(Lkotlin/coroutines/Continuation<-L{fq_name};>;)V")
            }
        })
        .or_else(|| {
            constructor_signatures::primary_constructor_signature(&signature_formatter, ir, c)
        });
    let value_param_ctor = ir.has_value_param_ctor(&fq_name);
    let ctor_access =
        method_access::primary_constructor_access(ir, c, is_continuation, value_param_ctor);
    let ctor_signature = method_signatures::written_signature(
        ctor_access,
        false,
        &primary_ctor_descriptor(ir, c),
        ctor_signature,
    );
    // An anonymous object carries kotlinc's minimal record (see the metadata assembly below) and
    // the same debug tables, so it is seeded like any class with a computed record.
    let byte_parity = !is_coroutine_state_machine(c)
        && opts.emit_class_metadata
        && (c.is_anonymous_object || build_class_metadata(ir, c, opts, env).is_some());
    let pool_seed = || PlainClassPoolSeed {
        ir,
        class: c,
        fq_name: &fq_name,
        ctor_signature: ctor_signature.as_deref(),
        java_parameters: env.java_parameters,
    };
    seed_ctor_before_members(byte_parity, pool_seed(), &mut cw);
    // Access: an extended or abstract class must not be `final`; a class with an emitted abstract
    // method is `ACC_ABSTRACT`. An inline-splice implementation is deliberately body-less after its
    // checked body has been consumed, but it is not a JVM method declaration at all.
    let extended = ir.classes.iter().any(|o| o.superclass_matches(&fq_name));
    let has_abstract = c.methods.iter().any(|&fid| {
        !standalone_method_is_elided(ir, fid, env) && ir.functions[fid as usize].body.is_none()
    });
    // A synthesized continuation class is package-private in kotlinc.
    let mut access = if is_continuation {
        0x0020 // SUPER (package-private)
    } else {
        class_public_bit(ir, c) | 0x0020 // [PUBLIC |] SUPER
    };
    // A SEALED class is abstract (kotlinc: sealed implies no direct instantiation), and an
    // `abstract class` is too — both alongside any class with an abstract (body-less) member.
    let is_abstract = has_abstract || c.is_sealed || c.is_abstract;
    if !extended && !is_abstract && !c.is_open {
        access |= 0x0010;
    } // FINAL
    if is_abstract {
        access |= 0x0400;
    } // ABSTRACT
    if ir.is_synthetic_class(c.fq_name_id()) {
        access |= 0x1000;
    } // ACC_SYNTHETIC (a `$$serializer` object)
    cw.set_access(access);
    if ir.is_deprecated_class(c.fq_name_id()) {
        cw.set_deprecated();
    } // Deprecated attribute (a HIDDEN-deprecated `$$serializer` object)
    crate::trace_compiler!(
        "value_classes",
        "class {} signature: raw={:?}",
        fq_name,
        ir.class_signature(&fq_name)
    );
    // (The class `Signature` was set with the writer; interface refs were added with the header,
    // before the pool seeding.)
    // A class with a `companion object`: its `public static final Companion` field LEADS the field
    // table (kotlinc's order), before the instance fields and any hoisted statics — but its pool
    // entries intern LATE (the `<clinit>` body's `putstatic` introduces them; the field visit dedups).
    if let Some(companion) = c.companion_class {
        let desc = format!("L{};", companion.render());
        let field = (
            companion_field_access(ir, c, companion),
            crate::jvm::companion_storage::field_name(c.fq_name, companion),
            desc.as_str(),
        );
        cw.add_field_late_leading(field, None, Some("Lorg/jetbrains/annotations/NotNull;"));
    }
    // `$$delegatedProperties` follows it; a singleton's follows its INSTANCE, below.
    if !static_storage(ir, c) {
        delegated_property_array::declare(env, c.fq_name, &mut cw);
    }
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    // Backing fields are private; access goes through the synthesized `getX()`/`setX()` accessors
    // (kotlinc does the same) — for both normal classes and objects.
    // A static-storage object's field table leads with INSTANCE, then its backing fields.
    let mut field_order: Vec<(usize, &crate::ir::IrField)> = if static_storage(ir, c) {
        Vec::new()
    } else {
        capture_order::physical_fields(ir, c, c.fields.iter().enumerate().collect())
    };
    // kotlinc appends captures and the outer instance (the constructor prefix) after declared fields.
    let prefix = captured_storage::constructor_prefix_fields(c).min(field_order.len());
    field_order.rotate_left(prefix);
    if is_continuation {
        field_order.sort_by_key(|(_, field)| match field.name.as_str() {
            "result" => 1,
            "this$0" => 2,
            "label" => 3,
            _ => 0,
        });
    }
    transformed_suspensions::add_spill_fields(&mut cw, transformed.as_ref());
    for (field_index, field) in field_order {
        let name = &field.name;
        let ty = &field.ty;
        let acc = if is_continuation {
            // kotlinc's continuation field layout: everything package-private; `result` is SYNTHETIC,
            // the captured receiver `this$0` is FINAL|SYNTHETIC; `label` and the `L$N` spills are plain.
            match name.as_str() {
                "result" => 0x1000,
                "this$0" => 0x1010,
                _ => 0x0000,
            }
        } else if captured_storage::stores_constructor_prefix(c, field_index) {
            0x1010
        } else {
            declared_field_access(c, field_index, static_storage(ir, c))
        };
        // A bare type parameter field (`val a: A`) signs `TA;`, a parameterized one its generic type;
        // a property's `x$delegate` storage signs nothing (kotlinc's `PROPERTY_DELEGATE` origin).
        let type_parameter = ir
            .field_signatures(&fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| fname == name))
            .map(|(_, parameter)| parameter.as_str())
            .or(field.type_param.as_deref());
        let field_sig = signature_formatter
            .field_signature(
                ty,
                type_parameter,
                field_visibility::backs_var_property(c, field_index),
            )
            .filter(|_| !field.is_property_delegate());
        let physical_name = instance_field_jvm_name(ir, env.run, c, field_index);
        let field_desc = type_descriptor(jvm_value_ty(ty));
        // Coroutine continuation fields are compiler-generated storage, visited eagerly with no
        // nullability annotation. Declared properties (a data class's too) use the later field-table
        // visit, after their methods, and carry Kotlin's nullability annotation.
        if is_continuation && matches!(name.as_str(), "result" | "this$0" | "label") {
            // kotlinc interns SPILL field names at the field visit, but `result`, `this$0` and `label`
            // where they are USED (`this$0` in the constructor's `putfield`, the others in
            // `invokeSuspend`); declaring them eagerly shifted every later pool entry.
            cw.add_field_late_sig(
                acc,
                &physical_name,
                &field_desc,
                field_sig.as_deref(),
                None,
                None,
            );
        } else if is_continuation {
            cw.add_field_sig(acc, &physical_name, &field_desc, field_sig.as_deref());
        } else {
            // Through the SHARED classification, so this field's annotation cannot disagree with the
            // pool seeder's ordering or the constructor's `LineNumberTable` start pc. It reads the
            // field's TYPE PARAMETER (and that parameter's bound) rather than the erased descriptor —
            // the local `type_parameter` above is the `Signature` attribute's spelling, a wider source
            // that must not become a second answer to the same question.
            let field_ann = field_visibility::publishes_field_nullability(c, field_index)
                .then(|| declared_field_annotation(c, field_index))
                .flatten();
            cw.add_field_late_sig(
                acc,
                &physical_name,
                &field_desc,
                field_sig.as_deref(),
                None,
                field_ann,
            );
            // A FIELD-targeted property annotation (`@Target(AnnotationTarget.FIELD)`) belongs to the
            // backing field itself, not to the property's marker method.
            if let Some(annotations) = c
                .field_annotations
                .iter()
                .find(|annotations| annotations.field == *name)
            {
                cw.set_last_late_field_annotations(&annotations.annotations);
            }
        }
    }
    // `@DebugMetadata` interns AFTER the field table: its `s` array names the very spill fields just
    // declared (`L$0`, `I$0`, …), and kotlinc's pool carries each of those strings once, first visited
    // with the FIELD. Building the annotation earlier interned the subset the array happens to mention
    // ahead of the table and left the rest to the fields, reordering the whole pool.
    if let Some(metadata) = continuation_metadata {
        cw.set_debug_metadata(&crate::jvm::bytecode_passes::coroutines::DebugMetadata {
            source_file: opts.source_file.clone().unwrap_or_default(),
            line_numbers: metadata.l.clone(),
            next_line_numbers: metadata.nl.clone(),
            index_to_label: metadata.i.clone(),
            spilled: metadata.s.clone(),
            local_names: metadata.n.clone(),
            method_name: metadata.m.clone(),
            class_name: metadata.c.clone(),
            version: metadata.v,
        });
    }
    static_fields::declare(ir, env.run, c, &fq_name, &signature_formatter, &mut cw);
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    // The constructor takes ALL primary-ctor params (`ctor_args`), in declaration order — `val`/`var`
    // params back a field, plain params are arguments only. (Synthesized classes have empty `ctor_args`
    // and fall back to the leading `ctor_param_count` fields.)
    // `(start_pc, line)` for the ctor's LineNumberTable — one per body-property initializer, plus the
    // trailing `return`. Empty when the class has no body properties (kotlinc emits a single entry).
    let mut ctor_lines: Vec<(u16, u32)> = Vec::new();
    let mut ctor_restored_lines: Vec<u16> = Vec::new();
    // `init`-block locals, with the ranges emission recorded. Empty ranges stay until the method
    // is written so an unused local's store is not removed as a temporary.
    let mut init_locals: Vec<(u16, u16, u16, String, String)> = Vec::new();
    let mut primary_ctor_debug = None;
    let param_tys = class_ctor_jvm_tys(ir, c);
    let value_tys = declaration_types::ctor_jvm_tys_by_identity(c);
    crate::trace_compiler!(
        "lower",
        "JVM constructor layout class={} params={:?} ctor_args={:?} fields={:?} prefix={} property_params={} explicit_stores={}",
        fq_name,
        param_tys,
        c.ctor_args,
        c.fields,
        c.constructor_prefix_count,
        c.ctor_param_count,
        c.explicit_param_stores,
    );
    let serialization_constructor = ir.generated_secondary_constructor_by_owner(
        c.fq_name_id(),
        crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
    );
    let markers = member_schedule::property_annotation_marker_fids(ir, c);
    let member_emission = ScheduledMemberEmission {
        ir,
        class: c,
        fq_name: &fq_name,
        facade,
        signature_formatter: &signature_formatter,
        param_assertions: opts.param_assertions,
        env,
        markers: &markers,
        byte_parity,
    };
    // A value class's declared members and `Any` overrides precede its private primary `<init>`,
    // which its generated representation members follow; every other class starts with `<init>`.
    let (before_primary, after_primary) = split_around_primary_constructor(
        ir,
        c,
        source_ordered_members(ir, c, serialization_constructor),
    );
    for member in before_primary {
        emit_scheduled_member(&member_emission, member, &mut cw);
    }
    if c.is_value {
        inherited_default_forwarders::emit_value_class_inherited_defaults(ir, c, &mut cw, env);
    }
    seed_value_ctor_after_members(byte_parity, pool_seed(), &mut cw);
    // A class with NO primary constructor emits no primary `<init>` — every `<init>` comes from a
    // secondary constructor (below). Otherwise emit the primary `<init>` here.
    if c.has_primary_ctor {
        let ctor_desc = primary_ctor_descriptor(ir, c);
        let ctor_parameters = primary_constructor_method_parameters(ir, c, env.java_parameters);
        // A `MethodParameters` name is interned with the method HEADER, before the body's constants
        // and before the `@NotNull`/`@Nullable` parameter descriptors — ASM's `visitParameter` order.
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &ctor_desc,
            ctor_signature.as_deref(),
            &[],
            &crate::ir::DeclarationAnnotations::default(),
            &ctor_parameters,
        );
        let params_words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        // The superclass constructor's parameter types (empty for the erased top type — the front end
        // names it `kotlin/Any`, which this backend maps to `java/lang/Object`).
        let max_slot;
        let mut init_diverges = false;
        {
            let mut e = Emitter::new(
                ir,
                &mut cw,
                env,
                Some(StaticOwner::Class(c.fq_name)),
                &fq_name,
                facade,
                Ty::Unit,
                c.super_arg_prelude
                    .iter()
                    .copied()
                    .chain(c.super_args.iter().copied())
                    .chain(c.init_body),
            );
            e.this_uninitialized = true;
            e.regeneration_site = Some(bytecode_inline_call::RegenerationSite::constructor(
                &ctor_desc,
                env.signature_symbols,
            ));
            e.record_locals = byte_parity;
            let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj_name(c.fq_name));
            e.slots.insert(0, (receiver, Ty::obj_name(c.fq_name)));
            for identity in capture_order::physical(ir, c, (0..value_tys.len()).collect()) {
                let (value, t) = (identity as u32 + 1, value_tys[identity]);
                let s = e.frame.enter(FrameKey::Value(value), t);
                e.slots.insert(value, (s, t));
            }
            // kotlinc guards each non-null reference constructor parameter with checkNotNullParameter at
            // the very start of `<init>` — before the super() call.
            for (i, a) in c.ctor_args.iter().enumerate() {
                if let Some(name) = &a.check {
                    if let Some(&(slot, _)) = e.slots.get(&(i as u32 + 1)) {
                        e.checked_parameters.insert(i as u32 + 1);
                        ctor.aload(slot);
                        ctor.push_string(name, e.cw);
                        let m = e.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNullParameter",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
                        ctor.invokestatic(m, 2, 0);
                    }
                }
            }
            let ctor_param_fields = primary_ctor_parameter_fields(c, param_tys.len());
            let debug_start = e.emit_pre_super_field_stores(c, &fq_name, &value_tys, &mut ctor);
            primary_ctor_debug = Some((
                ctor_desc.clone(),
                u16::try_from(debug_start).expect("a JVM method body fits in u16"),
            ));
            ctor_lines.extend(e.emit_primary_super_call(c, &superclass, &mut ctor));
            e.this_uninitialized = false;
            // Interface delegation runs after `super(…)` and before constructor-property stores.
            // The delegate expression sees parameters, not the properties those parameters become.
            let initializer = c.init_body.filter(|_| !static_storage(ir, c));
            let initializer_rest = initializer
                .map(|body| e.emit_interface_delegation_initializers(class_id, body, &mut ctor))
                .unwrap_or_default();
            // Store this class's own primary-constructor parameter fields: each `val`/`var` param's arg is
            // stored to its field (the property fields are `fields[0..]` in declaration order among params);
            // a plain param is skipped (it stays a local for the initializer body). `is_field` flags come
            // from `ctor_args`; a synthesized class (empty `ctor_args`) stores all leading param fields.
            // SKIPPED when `explicit_param_stores` is set — a desugared class already stores them via
            // explicit `SetField`s at the head of `init_body`; auto-storing too would double-store.
            if !c.explicit_param_stores {
                for (i, t) in value_tys.iter().enumerate() {
                    if let Some(field_i) = ctor_param_fields.get(i).copied().flatten() {
                        // Fields already stored before `super(…)` are not stored again here. The cutoff
                        // is semantic constructor metadata, independent of their physical ABI names.
                        if !c
                            .pre_super_param_fields
                            .iter()
                            .any(|&(_, pre_super_field)| pre_super_field as usize == field_i)
                        {
                            // kotlinc maps this field store to the parameter's own source line —
                            // capture the pc where it starts.
                            let pc = ctor.bytes.len() as u16;
                            if let Some(line) = property_line(ir, c, field_i as u32) {
                                ctor_lines.push((pc, line));
                            }
                            ctor.aload(0);
                            load(*t, e.slots[&(i as u32 + 1)].0, &mut ctor);
                            let physical_name = instance_field_jvm_name(ir, env.run, c, field_i);
                            let fref =
                                e.cw.fieldref(&fq_name, &physical_name, &type_descriptor(*t));
                            ctor.putfield(fref, slot_words(*t) as i32);
                        }
                    }
                }
            }
            if !initializer_rest.is_empty() {
                let marks_before = ctor.line_marks().len();
                e.record_locals = true;
                e.constructor_initializer_class = Some(class_id);
                e.render_initializer_boundaries = true;
                e.emit_initializer_statements(&initializer_rest, &mut ctor);
                e.render_initializer_boundaries = false;
                e.constructor_initializer_class = None;
                e.record_locals = false;
                let lines = (&mut ctor_lines, &mut ctor_restored_lines);
                initializer_lines::record_initializer_lines(&ctor, marks_before, lines);
                init_locals.extend(ctor.named_local_entries().iter().filter_map(
                    |&(start, length, slot, ref name, ref desc)| {
                        length.map(|length| (start, length, slot, name.clone(), desc.clone()))
                    },
                ));
            }
            if let Some(init_body) = initializer {
                init_diverges = e.discarding_diverges(init_body);
            }
            max_slot = e.frame.max();
        }
        // A diverging `init` (e.g. `init { throw … }`) leaves no fall-through — the trailing `return`
        // would be dead code after `athrow` (which the verifier rejects without a frame).
        if !init_diverges {
            // The trailing `return` goes back on the class-declaration line, closing the ctor's table.
            // A script has no declaration line: its `return` follows the last statement's.
            if !ctor_lines.is_empty() && !c.is_script {
                // A closing `init` line still pending owns a `nop` before the return's line.
                let line = primary_constructor_line(ir, c);
                ctor.mark_line(line);
                ctor_lines.push((ctor.bytes.len() as u16, line));
            }
            ctor.ret_void();
        }
        ctor.ensure_locals(max_slot);
        ctor.link();
        if byte_parity {
            seed_plain_constructor_locals(pool_seed(), &ctor, &mut cw);
        }
        cw.add_method_sig(
            ctor_access,
            "<init>",
            &ctor_desc,
            &ctor,
            ctor_signature.as_deref(),
        );
        cw.set_method_parameters("<init>", &ctor_desc, &ctor_parameters);
        if byte_parity {
            seed_plain_constructor_tail(pool_seed(), &mut cw);
        }
        // A continuation's constructor table is attached HERE, not in the trailing debug pass:
        // kotlinc interns a method's `LocalVariableTable` names with that method, before it visits
        // the next one, so batching every table at the end reordered the pool from `<init>` onward.
        if continuation_metadata.is_some() {
            let mut ctor_locals: Vec<(String, String, u16)> =
                vec![("this".to_string(), format!("L{fq_name};"), 0)];
            let identities = crate::jvm::parameter_names::constructor_identities(&c.ctor_args);
            assert_eq!(
                identities.len(),
                c.ctor_args.len(),
                "a continuation constructor's local identities must match its parameters"
            );
            for (slot, (argument, identity)) in (1u16..).zip(c.ctor_args.iter().zip(&identities)) {
                let name = crate::jvm::parameter_names::local_variable(identity, "<init>")
                    .expect("a continuation constructor parameter has a JVM local name");
                ctor_locals.push((name, local_variable_desc(argument.ty), slot));
            }
            cw.set_method_debug("<init>", &ctor_desc, None, &ctor_locals);
        }
        // Declared PRIMARY-constructor annotations (`class C @Mark constructor(…)`), with the same
        // `Deprecated` / `ACC_SYNTHETIC` companions a secondary constructor's carry.
        let primary_annotations = &c.primary_ctor_annotations;
        if !primary_annotations.retains_none() {
            let ctor_desc = method_descriptor(&param_tys, Ty::Unit);
            cw.set_method_annotations("<init>", &ctor_desc, primary_annotations);
            if primary_annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &ctor_desc);
            }
            if function_annotations::hidden_realization_is_synthetic(primary_annotations) {
                cw.set_method_synthetic("<init>", &ctor_desc);
            }
        }
        // A default on any primary-ctor parameter → kotlinc's synthetic
        // `<init>(params…, int mask, DefaultConstructorMarker)` overload (fills the masked slots from the
        // defaults, then `invokespecial` the real `<init>`).
        if let Some(defaults) = crate::jvm::default_wrappers::primary_constructor(ir, c) {
            // (The stub emits its own LineNumberTable — class line, per-fill parameter lines, the
            // ctor's closing-`)` line at `return` — collapsed for single-line declarations.)
            let prefix_count = c.constructor_prefix_count as usize;
            let (prefix_params, source_params) = value_tys
                .split_at_checked(prefix_count)
                .expect("constructor default prefix exceeds its parameters");
            let source_defaults = defaults
                .get(prefix_count..)
                .expect("constructor default prefix exceeds its defaults");
            constructor_defaults::emit_ctor_default_stub_with_prefix(
                ir,
                constructor_defaults::ConstructorDefaultStub {
                    owner: &fq_name,
                    owner_identity: c.fq_name,
                    facade,
                    prefix: prefix_params,
                    logical_prefix_count: prefix_count,
                    real_params: source_params,
                    defaults: source_defaults,
                    secondary_lines: None,
                    target_uses_marker_accessor: value_param_ctor || c.is_sealed,
                    deprecated: c.primary_ctor_annotations.deprecated(),
                    // A declared private primary's overload is package-private, the way kotlinc
                    // gives any private constructor's; a sealed class's is its public way in.
                    access: if ir.ctor_visibilities.get(&c.fq_name_id())
                        == Some(&crate::types::Visibility::Private)
                        && !c.is_sealed
                        && !value_param_ctor
                    {
                        0x1000
                    } else {
                        0x1001
                    },
                },
                &mut cw,
                env,
            );
        }
    } // end `if c.has_primary_ctor`

    // kotlinc adds a script's `main` with the script's own members, so it precedes every member
    // a later lowering adds: the lifted local functions and lambdas, bridges, and accessors.
    // A script that declares a function named `main` gets no generated one, whatever its shape.
    let mut script_main = c.is_script
        && !c.methods.iter().any(|&function| {
            !ir.lifted_functions.contains_key(&function)
                && ir.functions[function as usize].name == "main"
        });
    for member in after_primary {
        if script_main
            && matches!(member, SourceOrderedMember::Function(function) if ir.lifted_functions.contains_key(&function))
        {
            script_main::emit(&fq_name, opts.param_assertions, &mut cw);
            script_main = false;
        }
        emit_scheduled_member(&member_emission, member, &mut cw);
    }
    if script_main {
        script_main::emit(&fq_name, opts.param_assertions, &mut cw);
    }
    // EVERY parameter defaulted → kotlinc also emits the no-arg convenience `<init>()`
    // (`AuditFilters()` in Java/reflection), delegating to the `$default` overload with a full
    // mask — AFTER the declared methods (kotlinc's member order), at the primary's declared
    // visibility (a PROTECTED primary gets a protected convenience ctor; one hidden behind its
    // marker accessor for a value-class parameter is still public). A declared constructor without
    // value parameters already is that `<init>()`, so kotlinc's JvmDefaultConstructorLowering adds
    // none beside it.
    let declares_no_arg_ctor = c.secondary_ctors.iter().any(|sc| sc.params.is_empty());
    if c.has_primary_ctor && !declares_no_arg_ctor {
        let param_tys = class_ctor_jvm_tys(ir, c);
        let ctor_access = if is_continuation || c.is_anonymous_object {
            0x0000
        } else if c.is_singleton() || c.is_value || c.is_sealed {
            0x0002
        } else {
            match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => 0x0004,
                Some(crate::types::Visibility::Private) => 0x0002,
                _ => 0x0001,
            }
        };
        if let Some(defaults) = ir.class_ctor_defaults(&fq_name) {
            if !param_tys.is_empty()
                && defaults.len() == param_tys.len()
                && defaults.iter().all(Option::is_some)
                && !c.is_sealed
                && (ctor_access == 0x0001 || ctor_access == 0x0004)
            {
                // ASM interns a method's name and descriptor at its header visit, before its body.
                cw.reserve_method_name("<init>");
                cw.reserve_descriptor("()V");
                let mut z = CodeBuilder::new(1);
                z.aload(0);
                for &t in &param_tys {
                    push_zero(t, &mut z, &mut cw);
                }
                for mask in full_default_masks(param_tys.len()) {
                    z.push_int(mask, &mut cw);
                }
                z.aconst_null();
                let mut stub_params = param_tys.clone();
                stub_params.extend(std::iter::repeat_n(
                    Ty::Int,
                    default_mask_count(param_tys.len()),
                ));
                stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                let aw: i32 = 1 + stub_params
                    .iter()
                    .map(|t| slot_words(*t) as i32)
                    .sum::<i32>();
                let m = cw.methodref(
                    &fq_name,
                    "<init>",
                    &method_descriptor(&stub_params, Ty::Unit),
                );
                z.invokespecial(m, aw, 0);
                z.ret_void();
                z.ensure_locals(1);
                z.link();
                cw.add_method(ctor_access, "<init>", "()V", &z);
                // kotlinc gives the convenience ctor a `this` LocalVariableTable (no line table).
                cw.set_method_debug(
                    "<init>",
                    "()V",
                    None,
                    &[("this".to_string(), format!("L{fq_name};"), 0)],
                );
                // The convenience ctor STANDS IN for the declared primary, so kotlinc repeats the
                // primary's declared annotations on it (the `$default` overload between them, being
                // synthetic, gets none). Their descriptors already interned at the primary.
                let annotations = &c.primary_ctor_annotations;
                if !annotations.retains_none() {
                    cw.set_method_annotations("<init>", "()V", annotations);
                    if annotations.deprecated() {
                        cw.mark_method_deprecated("<init>", "()V");
                    }
                    if function_annotations::hidden_realization_is_synthetic(annotations) {
                        cw.set_method_synthetic("<init>", "()V");
                    }
                }
            }
        }
    }
    // The child-serializer cache follows both constructors (kotlinc's order), outside the schedule.
    let deferred = accessor_emission_order::deferred_cache_accessor(ir, c);
    for &fid in c
        .methods
        .iter()
        .filter(|fid| ir.serialization_cache_methods.contains(fid) && Some(**fid) != deferred)
    {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            emit_method(
                ir,
                fid,
                StaticOwner::Class(c.fq_name),
                &fq_name,
                facade,
                &mut cw,
                !f.is_static,
                env,
            );
        }
    }
    jvm_static_members::emit_companion_bridges(ir, c, &mut cw, env);
    // `-jvm-default=disable`: an inherited body gets an explicit override forwarding to the holder,
    // or every inherited call is an `AbstractMethodError`. kotlinc adds them before the bridges, and
    // `<clinit>` follows both.
    emit_default_impls_forwarders(ir, c, &mut cw, env);
    bridge_emission::emit_bridges(ir, c, &mut cw, env);
    accessor_emission_order::emit(ir, c, &fq_name, facade, env, &mut cw);
    if !static_storage(ir, c) {
        static_fields::emit_class_static_initializer(
            ir,
            c,
            facade,
            env,
            &fq_name,
            byte_parity,
            &mut cw,
        );
    }
    // A singleton object's JVM storage and initializer form one backend-owned responsibility.
    if static_storage(ir, c) {
        object_static_initialization::emit(
            ir,
            c,
            facade,
            env,
            &signature_formatter,
            &fq_name,
            &mut cw,
        );
    }
    // A cross-module provider's `@Metadata` wins; otherwise compute one from the IR (bounded shapes).
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts, env))
        .flatten();
    // A value class visits generated nullability before its class-level `@JvmInline`.
    let early_value_nullability =
        c.is_value && computed.is_some() && !is_coroutine_state_machine(c);
    if early_value_nullability {
        attach_synth_nullability(ir, c, env.run, &mut cw);
        // `set_class_annotations` visits the deferred underlying field first, so a generic
        // underlying's `Signature` (`TT;`) interns before `@JvmInline` too.
        cw.set_class_annotations(&super::value_classes::class_file_annotations(c));
    }
    // Debug tables + nullability annotations follow a successfully computed `@Metadata` shape.
    if computed.is_some() && !is_coroutine_state_machine(c) {
        attach_synth_debug_tables(
            ir,
            env.override_results,
            c,
            &mut cw,
            opts.param_assertions,
            synth_debug_tables::PrimaryConstructorDebug {
                method: primary_ctor_debug
                    .as_ref()
                    .map(|(descriptor, pc)| (descriptor.as_str(), *pc)),
                lines: &ctor_lines,
                restored_lines: &ctor_restored_lines,
                init_locals: &init_locals,
            },
        );
        attach_declared_method_debug(ir, env.override_results, c, &mut cw);
        if !early_value_nullability {
            attach_synth_nullability(ir, c, env.run, &mut cw);
        }
    }
    if !early_value_nullability {
        cw.set_class_annotations(&super::value_classes::class_file_annotations(c));
    }
    if continuation_metadata.is_some() {
        let invoke_suspend = c
            .methods
            .iter()
            .copied()
            .find(|&fid| {
                ir.function_parameter_identities(fid)
                    .is_some_and(|identities| {
                        matches!(
                            identities,
                            [crate::ir::IrParameterIdentity {
                                role: crate::ir::IrParameterRole::Generated(
                                    crate::ir::IrGeneratedParameterRole::ContinuationResult
                                ),
                                ..
                            }]
                        )
                    })
            })
            .expect("a continuation class owns its recorded resume method");
        let function = &ir.functions[invoke_suspend as usize];
        let parameters = jvm_function_params(ir, invoke_suspend);
        let descriptor = method_descriptor(
            &parameters,
            jvm_declared_ty(&env.override_results.physical_result(ir, invoke_suspend)),
        );
        let result_name =
            crate::jvm::parameter_names::function_locals(ir, invoke_suspend, &parameters)
                .and_then(|names| names.into_iter().next().flatten())
                .expect("a continuation result has a JVM local name");
        let self_desc = format!("L{fq_name};");
        cw.set_method_debug(
            &function.name,
            &descriptor,
            None,
            &[
                ("this".to_string(), self_desc, 0),
                (result_name, "Ljava/lang/Object;".to_string(), 1),
            ],
        );
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
    }
    // Intern the `EnclosingMethod` refs, then every retained `InnerClasses` row's outer-class ref
    // and simple name, at kotlinc's post-metadata pool position, in the table's sorted order. An
    // ANONYMOUS class has neither for its own row; its enclosing refs use the window in `finish`.
    if !is_coroutine_state_machine(c) && !c.is_anonymous_object {
        cw.intern_post_metadata_attribute_refs();
    }
    env.run.finish_class(cw)
}

pub(crate) fn parse_physical_method_desc(desc: &str) -> Option<(Vec<Ty>, Ty)> {
    super::physical_type::method_operand_tys(desc)
}

/// The wrapper class internal name for a primitive (`Int` → `java/lang/Integer`), for casting an
/// erased `Object` argument before unboxing.
/// Exact in-file function invoked by a function-reference carrier. Direct source references retain
/// their stable checked callable identity; structural adapters retain the generated common-IR id.
fn function_reference_target(ir: &IrFile, reference: &crate::ir::FuncRef) -> Option<u32> {
    reference.local_target.or_else(|| {
        reference
            .module_target
            .and_then(|target| ir.checked_callable_functions.get(&target).copied())
    })
}

fn verif_for_jvm_free(cw: &mut ClassWriter, t: Ty) -> VerifType {
    match t.kind() {
        _ if is_jvm_int_category(t) => VerifType::Integer,
        TyKind::LONG => VerifType::Long,
        TyKind::DOUBLE => VerifType::Double,
        TyKind::FLOAT => VerifType::Float,
        TyKind::STRING => VerifType::Object(cw.class_ref("java/lang/String")),
        _ if t.is_array() => VerifType::Object(cw.class_ref(&type_descriptor(t))),
        TyKind::Obj(n, _) => {
            VerifType::Object(cw.class_ref(crate::jvm::names::classfile_internal_name_of(n)))
        }
        TyKind::Null => VerifType::Null,
        _ => VerifType::Top,
    }
}

/// The `kotlin/jvm/internal/Ref$XxxRef` holder class and its `element` field descriptor for a boxed
/// mutable local of element type `elem` (a primitive picks its specialized `Ref`, any reference uses
/// `Ref$ObjectRef` whose `element` is `Object`).
fn ref_class(elem: &Ty) -> (&'static str, &'static str) {
    crate::jvm::shared_captures::holder_class(elem)
}

fn throw_assertion_error(cw: &mut ClassWriter, code: &mut CodeBuilder) {
    let ae = cw.class_ref("java/lang/AssertionError");
    code.new_obj(ae);
    code.dup();
    let init = cw.methodref("java/lang/AssertionError", "<init>", "()V");
    code.invokespecial(init, 0, 0);
    code.athrow();
}

fn finish_code<const ACCESS: u16>(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
) {
    finish_code_sig::<ACCESS>(cw, name, desc, code, locals, None);
}

/// [`finish_code`] for a method that carries a generic `Signature` (a facade accessor for a
/// parameterized property type — the descriptor erases its type arguments).
fn finish_code_sig<const ACCESS: u16>(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
    signature: Option<&str>,
) {
    code.ensure_locals(locals);
    code.link();
    cw.add_method_sig(ACCESS, name, desc, code, signature);
}

/// `Arrays.equals`/`Arrays.hashCode`/`Arrays.toString` parameter descriptor for an array member: a
/// primitive specialized array has its own overload (`[I`), a reference `Array<T>` uses
/// `[Ljava/lang/Object;` (array covariance lets a `String[]`/`Enum[]` flow in). Keyed off the array
/// KIND (its class), not the element — `Array<Int>` is a reference `Integer[]`, not `[I`.
fn arrays_param_desc(array: Ty) -> String {
    if array.is_reference_array() {
        "[Ljava/lang/Object;".to_string()
    } else {
        type_descriptor(array)
    }
}

/// Emit an `interface`: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/Object`. A method
/// with no body is a `public abstract` declaration; a method WITH a body is a Kotlin default method —
/// emitted as a concrete instance method (Code, no `ACC_ABSTRACT`), which the JVM treats as a default
/// method.
fn emit_interface_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> Vec<u8> {
    let fq_name = c.fq_name();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let mut cw = new_classifier_writer(ir, c, "java/lang/Object", env, opts);
    cw.set_access(class_public_bit(ir, c) | 0x0200 | 0x0400); // [PUBLIC |] INTERFACE | ABSTRACT
    supertype_markers::add_interfaces(&mut cw, ir, c);
    sealed_subtypes::register(
        &mut cw,
        ir,
        c,
        opts.class_major.unwrap_or(MAJOR_JAVA8) >= 61,
    );
    env.inner_classes.register(&mut cw);
    let mut default_impls: Option<ClassWriter> = None;
    // Whether this compilation publishes the `<Iface>$DefaultImpls` compatibility holder at all.
    let emits_default_impls = opts.jvm_default != JvmDefaultMode::NoCompatibility;
    // `disable` puts NO body on the interface: every member is abstract and the bodies live on the
    // holder as statics taking the receiver as parameter 0. The other two modes emit the body here as
    // a JVM default method.
    let bodies_on_interface = opts.jvm_default != JvmDefaultMode::Disable;
    // `enable` additionally publishes the compatibility surface next to each default method: an
    // `access$<name>$jd` bridge on the interface (emitted after every member, kotlinc's order) and a
    // holder forward per non-private body.
    let enable_compat = opts.jvm_default == JvmDefaultMode::Enable;
    let mut jd_bridge_fids: Vec<u32> = Vec::new();
    // kotlinc emits interface members in SOURCE order — a property's accessors sit at the
    // property's declared position among the functions — while lowering registers declared
    // functions before property accessors. Sort by the recorded source offset (stable, so a
    // property's getter stays before its setter); synthesized members without one trail in
    // registration order, kotlinc's placement for them too.
    let mut member_fids: Vec<u32> = c.methods.clone();
    member_fids.sort_by_key(|fid| ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX));
    for &fid in &member_fids {
        if standalone_method_is_elided(ir, fid, env) {
            continue;
        }
        let f = &ir.functions[fid as usize];
        // A STATIC member of an interface is not a default method: a lambda synthetic
        // (`f$lambda$0`) is a private static helper that stays where it is. Moving it to the holder
        // prepended a receiver its body never reads, and published it abstract on the interface —
        // the JVM rejected the result (`VerifyError: Bad type on operand stack`).
        if f.body.is_some() && (bodies_on_interface || f.is_static) {
            // A default method — concrete instance method on the interface.
            emit_method(
                ir,
                fid,
                StaticOwner::Class(c.fq_name),
                &fq_name,
                facade,
                &mut cw,
                !f.is_static,
                env,
            );
            if ir.function_reference_access_bridges.contains(&fid) {
                access_bridges::emit_function_reference_access_bridge(
                    ir,
                    fid,
                    &fq_name,
                    &mut cw,
                    true,
                    static_accessors::class_declaration_line(c),
                );
            }
            // A PRIVATE default stays a plain private instance method: kotlinc gives it no bridge,
            // no holder entry, and no forwarders anywhere (measured: an interface whose only body
            // is private has NO `$DefaultImpls` at all). A synthesized erasure bridge or an
            // inline-only lambda impl is not a source member either.
            if enable_compat
                && !f.is_static
                && !ir.method_visibility(fid).is_private()
                && !ir.bridge_methods.contains(&fid)
                && !ir.inline_only_fns.contains(&fid)
            {
                jd_bridge_fids.push(fid);
                let di = default_impls.get_or_insert_with(|| {
                    let mut w =
                        new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
                    w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                    w
                });
                interface_compatibility::emit_own_member_forward(
                    ir,
                    c,
                    fid,
                    di,
                    &signature_formatter,
                    opts,
                    env,
                );
            }
        } else {
            if f.body.is_some() && !f.is_static {
                // `disable`: the body moves to the holder as a receiver-first static, and the
                // interface keeps only the abstract declaration emitted below.
                let di = default_impls.get_or_insert_with(|| {
                    let mut w =
                        new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
                    w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                    w
                });
                emit_holder_method(ir, fid, c.fq_name, &fq_name, facade, di, env);
            }
            // A private member has NO interface-side declaration under `disable`: kotlinc writes
            // only the holder's private static (measured on 2.4.20), and a private abstract method
            // is not even a legal class-file shape (ACC_PRIVATE|ACC_ABSTRACT fails to load).
            if !ir.method_visibility(fid).is_private() {
                function_annotations::add_abstract(ir, &mut cw, fid, &signature_formatter, env);
            }
        }
        // An interface method with default parameters gets a STATIC `<name>$default(iface, params…, mask,
        // marker)` (the JVM realization of interface default args) — it applies the defaults then dispatches
        // to the abstract method via `invokeinterface`. kotlinc emits it ON THE INTERFACE (call sites use
        // it) AND, under a mode that keeps the compatibility holder, a copy on the
        // `<Iface>$DefaultImpls` class (`public final`).
        if let Some((default_fid, defaults)) =
            crate::jvm::default_wrappers::member_function(ir, fid)
        {
            // `disable` puts NOTHING executable on the interface, the `$default` stub included: call
            // sites go to the holder's copy instead.
            if bodies_on_interface {
                emit_default_stub(
                    DefaultStubSource {
                        ir,
                        fid: default_fid,
                        defaults,
                    },
                    Some(StaticOwner::Class(c.fq_name)),
                    &fq_name,
                    facade,
                    &mut cw,
                    env,
                    true,
                );
            }
            // `-jvm-default=no-compatibility` emits NO `$DefaultImpls` at all. Emitting one anyway
            // would publish a holder class the build says does not exist — a downstream compilation
            // resolving against it links to a class kotlinc would never have produced.
            if emits_default_impls {
                let di = default_impls.get_or_insert_with(|| {
                    let mut w =
                        new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
                    w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                    w
                });
                if enable_compat {
                    // The interface owns the real default application; the holder's copy is a
                    // thin synthetic forward to it (kotlinc's `enable` shape).
                    default_impls::emit_default_stub_forward(ir, default_fid, &fq_name, di);
                } else {
                    emit_default_stub(
                        DefaultStubSource {
                            ir,
                            fid: default_fid,
                            defaults,
                        },
                        None,
                        &fq_name,
                        facade,
                        di,
                        env,
                        true,
                    );
                }
            }
        }
    }
    // A collection special bridge follows the declared member it adapts and precedes the
    // compatibility access bridge for that member (`size()` before `access$getSize$jd`).
    bridge_emission::emit_bridges(ir, c, &mut cw, env);
    // The synthetic accessors of private statics come next, then the `access$…$jd` bridges
    // (kotlinc's order): declared bodies first, then the republished surface for inherited
    // defaults this interface does not redeclare.
    static_accessors::emit(
        ir,
        env.run,
        &env.run.static_accessor_plan.borrow(),
        StaticOwner::Class(c.fq_name),
        facade,
        static_accessors::class_declaration_line(c),
        &mut cw,
    );
    for &fid in &jd_bridge_fids {
        let f = &ir.functions[fid as usize];
        let physical_params = jvm_function_params(ir, fid);
        let parameter_names = crate::jvm::parameter_names::placed(
            ir,
            fid,
            Some(c.fq_name),
            crate::jvm::parameter_names::function_locals(ir, fid, &physical_params),
        )
        .expect("an access bridge carries exact declaration parameter identities");
        access_bridges::emit_jd_access_bridge(
            &mut cw,
            c.fq_name,
            static_accessors::class_declaration_line(c),
            access_bridges::JdAccessBridgeMember {
                name: &f.name,
                param_tys: &physical_params,
                parameter_names: &parameter_names,
                ret: jvm_declared_ty(&env.override_results.physical_result(ir, fid)),
                varargs: method_access::varargs_access(ir, fid),
                // A split interface member's source declaration is the trampoline, while
                // `suspend_funs` stays with the `$suspendImpl` body carrier that the coroutine
                // transformer owns. The copied declared signature is the exact semantic fact
                // this declaration-side bridge needs; do not recover it from the CPS descriptor.
                suspend: ir.suspend_declared_sigs.contains_key(&fid),
            },
        );
    }
    if emits_default_impls {
        interface_compatibility::emit_inherited_default_surface(
            ir,
            c,
            &mut cw,
            &mut default_impls,
            env.dependency_callables,
            opts,
        );
    }
    let emitted_default_impls = default_impls.is_some();
    if let Some(mut di) = default_impls {
        let holder = format!("{fq_name}$DefaultImpls");
        di.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: holder.clone(),
            outer: Some(fq_name.clone()),
            name: Some("DefaultImpls".to_string()),
            access: 0x0019, // PUBLIC | STATIC | FINAL
        });
        // A compiler-generated implementation class carries the minimal synthetic-class metadata
        // record. Kotlin reflection and downstream metadata readers rely on `k=3` to classify it.
        let xi = synthetic_class_xi(SYNTHETIC_PUBLIC);
        di.set_kotlin_metadata(3, opts.metadata_stamp(), xi, &[], &[]);
        extra.push((holder, env.run.finish_class(di)));
    }
    emit_jvm_interface_companion_surface(ir, c, facade, env, &mut cw);
    // An interface is a VIEW of the same `IrClass` every other kind is — compute its `@Metadata` (and
    // therefore its debug tables/annotations) through the shared path, exactly like `emit_class`.
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts, env))
        .flatten();
    if computed.is_some() {
        attach_synth_debug_tables(
            ir,
            env.override_results,
            c,
            &mut cw,
            opts.param_assertions,
            synth_debug_tables::PrimaryConstructorDebug::default(),
        );
        attach_declared_method_debug(ir, env.override_results, c, &mut cw);
        attach_synth_nullability(ir, c, env.run, &mut cw);
    }
    // Class annotations follow all members and their debug/nullability attributes, before metadata.
    cw.set_class_annotations(&c.applied_annotations);
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
    }
    if emitted_default_impls {
        let holder = format!("{fq_name}$DefaultImpls");
        // The outer interface must publish the same nested-class relation as the synthetic holder.
        cw.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: holder,
            outer: Some(fq_name.clone()),
            name: Some("DefaultImpls".to_string()),
            access: 0x0019,
        });
    }
    env.run.finish_class(cw)
}

/// Emit an `enum class`: extends `java/lang/Enum`, a private `(String name, int ordinal, …)` ctor →
/// `super(name, ordinal)`, a `public static final` field per entry plus a `$VALUES` array, a
/// `<clinit>` that constructs the entries and fills `$VALUES`, and synthetic `values()`/`valueOf`.
fn emit_enum_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    const ACC_ENUM: u16 = 0x4000;
    const ACC_SYNTHETIC: u16 = 0x1000;
    let fq = c.fq_name();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let self_desc = format!("L{fq};");
    let arr_desc = format!("[{self_desc}");
    let synthetic_enum_fields = enum_synthetic_fields::names(ir, c);
    // An enum extends the PARAMETERIZED `java.lang.Enum<E>`, so it carries a class `Signature` —
    // whose value interns between the class and superclass names, as ASM visits them.
    let mut cw = new_classifier_writer(ir, c, "java/lang/Enum", env, opts);
    // An enum with an abstract member is `ACC_ABSTRACT`; one with any bodied entry (so a subclass
    // extends it) must not be `final`. A plain enum stays `final`.
    let has_abstract = c.methods.iter().any(|&fid| {
        !standalone_method_is_elided(ir, fid, env) && ir.functions[fid as usize].body.is_none()
    });
    let has_subclass = c.enum_entries.iter().any(|e| e.subclass.is_some());
    let mut access = class_public_bit(ir, c) | 0x0020 | ACC_ENUM; // [PUBLIC |] SUPER | ENUM
    if has_abstract {
        access |= 0x0400;
    } // ABSTRACT
    if !has_abstract && !has_subclass {
        access |= 0x0010;
    } // FINAL
    cw.set_access(access);
    // Every enum extends the generic `java.lang.Enum<Self>`, so kotlinc emits a class `Signature`
    // (`Ljava/lang/Enum<LSelf;>;` plus a raw `L<itf>;` for each superinterface). The erased
    // descriptor already names `java/lang/Enum`; the Signature carries the `<Self>` type argument.
    // (The class `Signature` came with the writer, from the recorded signature — a hand-rolled one
    // here would erase the type arguments of every implemented interface.)
    // The file's whole nest, as every other classifier path registers it: `finish` keeps only the
    // rows this class references. Without it an enum's table was built from resolver lookups alone,
    // which see only the classes already in the pool — so a nested enum listed its own row and its
    // `Companion`'s, but not the ENCLOSING row kotlinc writes for the class that contains it.
    env.inner_classes.register(&mut cw);
    // Interfaces the enum implements (`enum class E : I`) — without these the JVM rejects an
    // interface-typed call with `IncompatibleClassChangeError`.
    supertype_markers::add_interfaces(&mut cw, ir, c);

    let field_tys = field_jvm_tys(&c.fields);
    // (bridges emitted after the methods below — `emit_bridges` references emitted method refs)
    let n_params = c.ctor_param_count as usize;
    let user_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    let all_param_tys = class_ctor_jvm_tys(ir, c);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(all_param_tys.iter().copied())
        .collect();
    let ctor_desc = method_descriptor(&ctor_params, Ty::Unit);
    let ctor_sig = constructor_signatures::enum_constructor_signature(&signature_formatter, ir, c)
        .expect("an enum constructor always signs its source parameters");
    let ctor_parameters = if env.java_parameters {
        super::method_parameters::enum_constructor(c).to_vec()
    } else {
        Vec::new()
    };
    // Property backing fields are private (kotlinc), reached through the synthesized `getX()`/`setX()`
    // accessors — for both the primary-constructor fields and body member-property fields
    // (`enum class E { A; val x = … }`), initialized in the constructor via `init_body`.
    // kotlinc emits an ENUM's `Companion` field FIRST — ahead of the constructor properties, the
    // entry constants and `$VALUES`/`$ENTRIES` — unlike an ordinary class, where it follows the
    // instance fields. Emitting it last also interned its name and descriptor late, so the class
    // differed in constant-pool order even where every member matched.
    let owner_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_matches(&fq))
        .map(|(index, s)| (index as u32, s))
        .collect();
    // The `Companion` field LEADS the field table, but kotlinc interns its name and descriptor at
    // the field VISIT — late, not here. Emitting it eagerly put those strings at the head of the
    // constant pool and reordered nearly all of it.
    if let Some(companion) = c.companion_class {
        let desc = format!("L{};", companion.render());
        let field = (
            companion_field_access(ir, c, companion),
            crate::jvm::companion_storage::field_name(c.fq_name, companion),
            desc.as_str(),
        );
        cw.add_field_late_leading(field, None, Some("Lorg/jetbrains/annotations/NotNull;"));
    }
    // `$$delegatedProperties` follows `Companion`, as in an ordinary class.
    delegated_property_array::declare(env, c.fq_name, &mut cw);
    // kotlinc visits the whole CONSTRUCTOR before the entry constants — name, descriptor, its generic
    // `Signature` (the two synthetic `Enum` params are erased, leaving `()V`), then its
    // LocalVariableTable strings. `add_field` would otherwise claim those slots for the first entry.
    cw.reserve_method_pool_with_annotations(
        "<init>",
        &ctor_desc,
        Some(&ctor_sig),
        &[],
        &crate::ir::DeclarationAnnotations::default(),
        &ctor_parameters,
    );
    // The ctor BODY's `super(name, ordinal)` call resolves before its LocalVariableTable strings.
    cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    // The property backing fields are visited after the methods: each name interns where the
    // constructor body first stores it, after that body's own constants.
    for (field_index, (f, t)) in c.fields.iter().zip(&field_tys).enumerate() {
        let physical_name = instance_field_jvm_name(ir, env.run, c, field_index);
        let nullability = (!f.is_lateinit())
            .then(|| declared_field_annotation(c, field_index))
            .flatten();
        let access = field_visibility::declared_field_access(c, field_index, false);
        let signature = signature_formatter
            .field_signature(
                &f.ty,
                f.type_param.as_deref(),
                field_visibility::backs_var_property(c, field_index),
            )
            .filter(|_| !f.is_property_delegate());
        cw.add_field_late_sig(
            access,
            &physical_name,
            &type_descriptor(*t),
            signature.as_deref(),
            None,
            nullability,
        );
    }
    static_fields::emit_enum_static_fields(ir, c, &fq, &signature_formatter, &mut cw);
    // One static-final constant per entry, plus the private `$VALUES` array. kotlinc visits the
    // fields after the methods, so each name interns where a body first references it (`$values`
    // reads every entry), or at the field visit.
    for entry in &c.enum_entries {
        cw.add_field_late(
            0x0001 | 0x0008 | 0x0010 | ACC_ENUM,
            &entry.name,
            &self_desc,
            None,
            None,
        );
        apply_enum_entry_annotations(&mut cw, c, &entry.name);
    }
    cw.add_field_late(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        &synthetic_enum_fields.values,
        &arr_desc,
        None,
        None,
    );
    // The `entries` property backing (Kotlin 2.x emits this on EVERY enum): a `private static final`
    // `kotlin/enums/EnumEntries`, initialized in `<clinit>` from `EnumEntriesKt.enumEntries($VALUES)`.
    cw.add_field_late(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        &synthetic_enum_fields.entries,
        "Lkotlin/enums/EnumEntries;",
        None,
        None,
    );
    // The owner-scoped statics the serialization plugin synthesized were emitted with the leading
    // fields above, next to `Companion`, where kotlinc puts them; this binding is the one `<clinit>`
    // below initializes.
    // Private constructor `(Ljava/lang/String;I<user params>)V` → `super(name, ordinal)` then store the
    // property params / run the body-property initializers. The user params are ALL primary-ctor params
    // (from `ctor_args`) — a `val`/`var` param backs a field, a plain param is an argument only (in scope
    // for a body-property initializer), so `all_param_tys` can be wider than the `n_params` fields.
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    ctor.aload(1);
    load(Ty::Int, 2, &mut ctor);
    let super_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    ctor.invokespecial(super_init, 2, 0);
    // kotlinc maps the `super(name, ordinal)` call to where the declaration starts (annotations
    // included), then maps each property store to that property's own declaration line.
    let mut store_lines = initializer_lines::ConstructorInitializerLines::default();
    let mut max_locals = 1 + ctor_words;
    // Store constructor-property parameters unless lowering already represented those stores in the
    // init body. The existence of a body property is not that contract: an init body can contain
    // only body-property stores while leaving `explicit_param_stores` false.
    if !c.explicit_param_stores {
        let mut slot = 3u16;
        let mut field_i = 0usize;
        for (argument, ty) in c.ctor_args.iter().zip(&all_param_tys) {
            if argument.is_field {
                let name = instance_field_jvm_name(ir, env.run, c, field_i);
                if let Some(line) = property_line(ir, c, field_i as u32) {
                    store_lines.push((ctor.bytes.len() as u16, line));
                }
                ctor.aload(0);
                load(*ty, slot, &mut ctor);
                let field = cw.fieldref(&fq, &name, &type_descriptor(*ty));
                ctor.putfield(field, slot_words(*ty) as i32);
                field_i += 1;
            }
            slot += slot_words(*ty);
        }
    }
    // Emit body-property stores and `init` statements through the standard IR emitter, mapping value
    // ids onto the enum's slot layout: `this` at 0, then every user parameter at slots 3+ after the
    // synthetic name and ordinal. A pure SetField block retains each store's source line.
    if let Some(init_body) = c.init_body {
        let marks_before = ctor.line_marks().len();
        let mut e = Emitter::new(
            ir,
            &mut cw,
            env,
            Some(StaticOwner::Class(c.fq_name)),
            &fq,
            facade,
            Ty::Unit,
            [init_body],
        );
        let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj_name(c.fq_name));
        e.slots.insert(0, (receiver, Ty::obj_name(c.fq_name)));
        // The synthetic name and ordinal come first; no semantic value names them.
        e.frame.enter(FrameKey::Parameter(0), Ty::String);
        e.frame.enter(FrameKey::Parameter(1), Ty::Int);
        for (i, t) in all_param_tys.iter().enumerate() {
            let value = i as u32 + 1;
            let s = e.frame.enter(FrameKey::Value(value), *t);
            e.slots.insert(value, (s, *t));
        }
        e.render_initializer_boundaries = true;
        e.emit_constructor_init_body(c, init_body, &mut ctor, &mut store_lines);
        e.render_initializer_boundaries = false;
        max_locals = max_locals.max(e.frame.max());
        store_lines.record_restored(&ctor, marks_before);
    }
    // The `return` maps to the primary constructor's start; a pending closing `init` line owns a
    // `nop` before that transition.
    let return_line = primary_constructor_line(ir, c);
    if return_line != 0 && !store_lines.is_empty() {
        ctor.mark_line(return_line);
    }
    let ctor_return_pc = ctor.bytes.len() as u16;
    ctor.ret_void();
    ctor.ensure_locals(max_locals);
    ctor.link();
    // An enum's ctor is `private`: an entry subclass reaches it via its marker accessor or `$default`.
    let base_ctor_acc = 0x0002;
    // kotlinc's `Signature` on the enum ctor lists only the USER params, without the synthetic
    // leading `(String, int)`: `()V` for a plain enum, `(I)V` for `E(val n: Int)`.
    // An enum declaring ONLY secondary constructors has no primary to emit: every entry names one
    // of the secondaries. Registering the synthesized primary anyway collided with a no-argument
    // secondary — both are `(String, int)V` — and the class failed to load with a
    // `ClassFormatError: Duplicate method name "<init>"`. Its bytes are still built above so the
    // constant pool interns in kotlinc's order.
    seed_enum_constructor_locals(ir, c, &self_desc, &mut cw);
    let emits_primary_ctor = c.has_primary_ctor || c.secondary_ctors.is_empty();
    if emits_primary_ctor {
        cw.add_method_sig(
            base_ctor_acc | method_access::primary_constructor_varargs(c),
            "<init>",
            &ctor_desc,
            &ctor,
            Some(&ctor_sig),
        );
        cw.set_method_parameters("<init>", &ctor_desc, &ctor_parameters);
    }
    if emits_primary_ctor {
        let header = c.decl_line;
        let start = if c.decl_start_line == 0 {
            header
        } else {
            c.decl_start_line
        };
        if header != 0 {
            // Each retained store maps to its property's line and the `return` to the constructor
            // start. A missing line is not reconstructed in the backend.
            let entries = store_lines.method_entries(
                (0, start),
                (ctor_return_pc, primary_constructor_line(ir, c)),
            );
            cw.set_method_lines("<init>", &ctor_desc, &entries);
        }
    }
    if let Some(defaults) = crate::jvm::default_wrappers::primary_constructor(ir, c) {
        constructor_defaults::emit_ctor_default_stub_with_prefix(
            ir,
            constructor_defaults::ConstructorDefaultStub {
                owner: &fq,
                owner_identity: c.fq_name,
                facade,
                prefix: &[Ty::String, Ty::Int],
                logical_prefix_count: 0,
                real_params: &all_param_tys,
                defaults,
                secondary_lines: None,
                target_uses_marker_accessor: false,
                deprecated: false,
                access: ACC_SYNTHETIC,
            },
            &mut cw,
            env,
        );
    }

    // The constructors' LocalVariableTable strings follow their bodies, and the property accessors
    // (`getTag`, its descriptor, its `@NotNull`) come next, each at its method visit.
    for (f, t) in c.fields[..n_params].iter().zip(&user_tys) {
        cw.reserve_method_name(&property_getter_name(&f.name));
        cw.reserve_descriptor(&format!("(){}", type_descriptor(*t)));
        // A parameterized getter's generic `Signature` interns at its method visit, before the
        // nullability annotation.
        if let Some(getter) =
            property_jvm_signatures(&signature_formatter, &f.ty, f.type_param.as_deref()).getter
        {
            cw.reserve_descriptor(&getter);
        }
        // The nullability comes from the declared type; `t` is its erased JVM form.
        match field_nullability_kind(f.ty) {
            1 => cw.reserve_descriptor("Lorg/jetbrains/annotations/NotNull;"),
            2 => cw.reserve_descriptor("Lorg/jetbrains/annotations/Nullable;"),
            _ => {}
        }
    }

    emit_declared_property_accessors(
        ir,
        c,
        &mut cw,
        &PropertyAccessorEmit {
            fq_name: &fq,
            facade,
            formatter: &signature_formatter,
            param_assertions: opts.param_assertions,
            env,
        },
    );
    // An enum's SECONDARY constructors, after the declared accessors — the order kotlinc emits for
    // `enum class My(val s: String) { ENTRY; constructor(): this("OK") }`: `My(String)`, `getS()`,
    // `My()`. The enum writer is a separate path from `emit_class`, so they were not emitted at
    // all, and an entry declared with no arguments called an `<init>` that does not exist: the
    // class failed to load with a `NoSuchMethodError`.
    for (ordinal, constructor) in c.secondary_ctors.iter().enumerate() {
        SecondaryConstructorEmitter {
            ir,
            class: c,
            owner: &fq,
            facade,
            env,
            writer: &mut cw,
            owner_prefix: &OwnerConstructorPrefix::enum_class(),
        }
        .emit(ordinal, constructor);
    }
    let markers = member_schedule::property_annotation_marker_fids(ir, c);
    // kotlinc's enum member order is `<init>`, the DECLARED members, `values`/`valueOf`/
    // `getEntries`, the lifted local functions, `$values`, the lambdas, anything a PLUGIN
    // synthesized onto the class (`_init_$_anonymous_`, `access$…`), then `<clinit>`.
    let schedule = member_schedule::enum_member_schedule(ir, c);
    let emit_members = |cw: &mut ClassWriter, members: &[u32]| {
        for &fid in members {
            if markers.contains(&fid) || standalone_method_is_elided(ir, fid, env) {
                continue; // already emitted beside its property's accessors
            }
            let f = &ir.functions[fid as usize];
            if f.body.is_some() {
                // Honor `is_static` (an extension-synthesized `static` member like serialization's
                // `serializer()` accessor) — emitting it as an instance method breaks an `E.serializer()`
                // static call (`IncompatibleClassChangeError`).
                emit_method(
                    ir,
                    fid,
                    StaticOwner::Class(c.fq_name),
                    &fq,
                    facade,
                    cw,
                    !f.is_static,
                    env,
                );
                if ir.function_reference_access_bridges.contains(&fid) {
                    access_bridges::emit_function_reference_access_bridge(
                        ir,
                        fid,
                        &fq,
                        cw,
                        false,
                        static_accessors::class_declaration_line(c),
                    );
                }
                // A defaulted member needs its `<name>$default` synthetic here too. The enum writer is a
                // separate path from `emit_class`, so a member declared `fun m(a: Int = 1)` on an enum
                // silently had no stub at all — a call omitting the argument had nothing to dispatch to.
                // Same call as the class path, so the super-call guard rides along: an enum IS
                // inheritable (an entry body subclasses it), which is why kotlinc guards the stub.
                if let Some((fid, defaults)) =
                    crate::jvm::default_wrappers::member_function(ir, fid)
                {
                    emit_default_stub(
                        DefaultStubSource { ir, fid, defaults },
                        Some(StaticOwner::Class(c.fq_name)),
                        &fq,
                        facade,
                        cw,
                        env,
                        false,
                    );
                }
            } else {
                // An abstract enum member (`abstract fun t(): String`) — declared `ACC_ABSTRACT`, the
                // entry subclasses override it.
                function_annotations::add_abstract(ir, &mut *cw, fid, &signature_formatter, env);
            }
        }
    };
    emit_members(&mut cw, &schedule.declared);
    // The synthesized members follow the declared ones, in kotlinc's visit order, each with the entries its body
    // references: `values()` reads `$VALUES` and calls `Object.clone()`; `valueOf` delegates to
    // `Enum.valueOf` and names its parameter `value`; `getEntries` returns the `@NotNull`
    // `$ENTRIES`. Only after all of them does kotlinc reach the entry constants themselves.
    cw.reserve_method_name("values");
    cw.reserve_descriptor(&format!("(){arr_desc}"));
    cw.reserve_method_name(&synthetic_enum_fields.values);
    cw.reserve_descriptor(&arr_desc);
    cw.fieldref(&fq, &synthetic_enum_fields.values, &arr_desc);
    cw.class_ref("java/lang/Object");
    cw.methodref("java/lang/Object", "clone", "()Ljava/lang/Object;");
    // `values()` casts the `clone()` result back: `checkcast [LE;`.
    cw.class_ref(&arr_desc);
    let value_of_desc = format!("(Ljava/lang/String;){self_desc}");
    let value_of_parameters = if env.java_parameters {
        super::method_parameters::enum_value_of().to_vec()
    } else {
        Vec::new()
    };
    cw.reserve_method_pool_with_annotations(
        "valueOf",
        &value_of_desc,
        None,
        &[],
        &crate::ir::DeclarationAnnotations::default(),
        &value_of_parameters,
    );
    cw.methodref(
        "java/lang/Enum",
        "valueOf",
        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
    );
    cw.reserve_method_name("value");
    cw.reserve_method_name("getEntries");
    cw.reserve_descriptor("()Lkotlin/enums/EnumEntries;");
    cw.reserve_descriptor(&format!("()Lkotlin/enums/EnumEntries<{self_desc}>;"));
    cw.reserve_descriptor("Lorg/jetbrains/annotations/NotNull;");
    cw.reserve_method_name(&synthetic_enum_fields.entries);
    cw.reserve_descriptor("Lkotlin/enums/EnumEntries;");
    cw.fieldref(
        &fq,
        &synthetic_enum_fields.entries,
        "Lkotlin/enums/EnumEntries;",
    );
    cw.reserve_method_name("$values");
    // values(): `$VALUES.clone()` cast back to the array type.
    let mut vals = CodeBuilder::new(0);
    let valref = cw.fieldref(&fq, &synthetic_enum_fields.values, &arr_desc);
    vals.getstatic(valref, 1);
    // kotlinc invokes `clone()` via `java/lang/Object` (not the `[LE;` array type).
    let clone_m = cw.methodref("java/lang/Object", "clone", "()Ljava/lang/Object;");
    vals.invokevirtual(clone_m, 0, 1);
    let arr_cls = cw.class_ref(&arr_desc);
    vals.checkcast(arr_cls);
    vals.areturn();
    finish_code::<0x0009>(&mut cw, "values", &format!("(){arr_desc}"), &mut vals, 0);

    // valueOf(String): `Enum.valueOf(E.class, name)` cast to E.
    let mut vof = CodeBuilder::new(1);
    vof.ldc_class(&fq, &mut cw);
    vof.aload(0);
    let veo = cw.methodref(
        "java/lang/Enum",
        "valueOf",
        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
    );
    vof.invokestatic(veo, 2, 1);
    let cc = cw.class_ref(&fq);
    vof.checkcast(cc);
    vof.areturn();
    finish_code::<0x0009>(&mut cw, "valueOf", &value_of_desc, &mut vof, 1);
    cw.set_method_parameters("valueOf", &value_of_desc, &value_of_parameters);

    // getEntries(): the `entries` property accessor → `return $ENTRIES`. Carries the generic
    // `Signature` `()Lkotlin/enums/EnumEntries<LSelf;>;` kotlinc emits.
    let mut gent = CodeBuilder::new(0);
    let entref = cw.fieldref(
        &fq,
        &synthetic_enum_fields.entries,
        "Lkotlin/enums/EnumEntries;",
    );
    gent.getstatic(entref, 1);
    gent.areturn();
    gent.ensure_locals(0);
    gent.link();
    cw.add_method_sig(
        0x0009,
        "getEntries",
        "()Lkotlin/enums/EnumEntries;",
        &gent,
        Some(&format!("()Lkotlin/enums/EnumEntries<L{fq};>;")),
    );
    emit_members(&mut cw, &schedule.local_functions);
    // $values(): build the backing array — `new E[n]` filled with each entry constant (kotlinc factors
    // this out of `<clinit>`). Private static final synthetic, returning `E[]`.
    let mut vbuild = CodeBuilder::new(1);
    vbuild.push_int(c.enum_entries.len() as i32, &mut cw);
    let acls = cw.class_ref(&fq);
    vbuild.anewarray(acls);
    vbuild.astore(0);
    for (i, entry) in c.enum_entries.iter().enumerate() {
        vbuild.aload(0);
        vbuild.push_int(i as i32, &mut cw);
        let fref = cw.fieldref(&fq, &entry.name, &self_desc);
        vbuild.getstatic(fref, 1);
        vbuild.array_store(0x53, 1); // aastore
    }
    vbuild.aload(0);
    vbuild.areturn();
    vbuild.ensure_locals(1);
    vbuild.link();
    cw.add_method(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$values",
        &format!("(){arr_desc}"),
        &vbuild,
    );

    emit_members(&mut cw, &schedule.lambdas);
    emit_members(&mut cw, &schedule.plugin_generated);
    // An `enum class` implementing an interface needs the same holder forwarders an ordinary class
    // does, and the erased bridges for a generic-interface method it overrides (`enum E : A<String>
    // { …; override fun foo(t: String) }` → bridge `foo(Object)`→`foo(String)`). kotlinc adds both
    // before `<clinit>`, forwarders first.
    jvm_static_members::emit_companion_bridges(ir, c, &mut cw, env);
    emit_default_impls_forwarders(ir, c, &mut cw, env);
    bridge_emission::emit_bridges(ir, c, &mut cw, env);
    constructor_accessors::emit_sealed_accessors(ir, c, &fq, &mut cw);
    static_accessors::emit(
        ir,
        env.run,
        &env.run.static_accessor_plan.borrow(),
        StaticOwner::Class(c.fq_name),
        facade,
        static_accessors::class_declaration_line(c),
        &mut cw,
    );
    // `<clinit>` is RESERVED and BUILT here, after the plugin-generated members: kotlinc interns
    // their names, descriptors and body constants between the entry constants and `<clinit>`, so
    // building the initializer earlier claimed those pool slots first.
    // `<clinit>`'s NAME interns before anything its body references (the `EnumEntriesKt.enumEntries`
    // machinery), as kotlinc reaches a method's signature before its code.
    cw.reserve_method_name("<clinit>");
    // …with its descriptor, which kotlinc interns alongside the name and before the body's
    // constants.
    cw.reserve_descriptor("()V");
    // <clinit>: construct each entry, then `$VALUES = $values()` and
    // `$ENTRIES = EnumEntriesKt.enumEntries($VALUES)`. BUILT here but ADDED last (kotlinc orders it
    // after values/valueOf/getEntries/$values); the linked `CodeBuilder` is self-contained.
    let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
    let clinit = {
        let mut e = Emitter::new(
            ir,
            &mut cw,
            env,
            Some(StaticOwner::Class(c.fq_name)),
            &fq,
            facade,
            Ty::Unit,
            c.enum_entries
                .iter()
                .flat_map(|entry| entry.argument_prelude.iter().chain(&entry.args).copied())
                .chain(ir.companion_clinit_body(c.fq_name)),
        );
        e.enter_class_initializer(env.signature_symbols);
        let mut clinit = CodeBuilder::new(0);
        e.emit_delegated_property_array(env, c.fq_name, &fq, &mut clinit);
        if delegated_property_array::exists(env, c.fq_name) {
            e.cw.omit_method_lines("<clinit>", "()V");
        }
        // kotlinc gives each entry's construction its own `<clinit>` LineNumberTable entry, on that
        // Consecutive enum entries on one source line share one LNT entry.
        for (i, entry) in c.enum_entries.iter().enumerate() {
            if entry.decl_line != 0 && clinit_lines.last().map(|&(_, l)| l) != Some(entry.decl_line)
            {
                clinit_lines.push((clinit.bytes.len() as u16, entry.decl_line));
            }
            for &statement in &entry.argument_prelude {
                e.emit(statement, &mut clinit);
            }
            let args = &entry.args;
            // An entry arg that cannot carry the operand stack (`X(try { 1 } finally {})`) runs on a
            // clean stack: spill all args to temps first, then construct (as the `New` node does).
            let spill = args.iter().any(|&a| e.spills_operand_prefix(a));
            let temps = if spill {
                e.spill_to_temps(args, &mut clinit)
            } else {
                Vec::new()
            };
            // A bodied entry is an instance of its synthesized subclass (`new Enum$ENTRY(...)`); the
            // subclass constructor shares the enum's `(String,int,<user>)V` descriptor.
            let new_class = entry
                .subclass
                .map(TypeName::render)
                .unwrap_or_else(|| fq.clone());
            let cls = e.cw.class_ref(&new_class);
            clinit.new_obj(cls);
            clinit.dup();
            clinit.push_string(&entry.name, e.cw);
            clinit.push_int(i as i32, e.cw);
            let entry_parameter_types = &entry.constructor_parameter_types;
            if spill {
                let mut supplied = temps.iter();
                for (parameter, ty) in entry_parameter_types.iter().copied().enumerate() {
                    if entry.default_parameters.contains(&(parameter as u32)) {
                        push_zero(ty, &mut clinit, e.cw);
                    } else {
                        let (slot, ty, _) = supplied
                            .next()
                            .expect("checked enum constructor supplied-argument count");
                        load(*ty, *slot, &mut clinit);
                    }
                }
                e.release_operand_spills(&temps);
            } else {
                let mut supplied = args.iter().copied();
                for (parameter, ty) in entry_parameter_types.iter().copied().enumerate() {
                    if entry.default_parameters.contains(&(parameter as u32)) {
                        push_zero(ty, &mut clinit, e.cw);
                    } else {
                        e.emit_value(
                            supplied
                                .next()
                                .expect("checked enum constructor supplied-argument count"),
                            &mut clinit,
                        );
                    }
                }
            }
            let mut entry_ctor_params = vec![Ty::String, Ty::Int];
            entry_ctor_params.extend(entry_parameter_types.iter().copied());
            let (entry_ctor_desc, entry_ctor_argw) = if entry.default_parameters.is_empty() {
                let words = entry_ctor_params
                    .iter()
                    .map(|ty| slot_words(*ty) as i32)
                    .sum();
                (method_descriptor(&entry_ctor_params, Ty::Unit), words)
            } else {
                emit_constructor_default_arguments(
                    &entry.default_parameters,
                    entry_parameter_types.len(),
                    &mut clinit,
                    e.cw,
                );
                entry_ctor_params.extend(std::iter::repeat_n(
                    Ty::Int,
                    default_mask_count(entry_parameter_types.len()),
                ));
                entry_ctor_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                let words = entry_ctor_params
                    .iter()
                    .map(|ty| slot_words(*ty) as i32)
                    .sum();
                (method_descriptor(&entry_ctor_params, Ty::Unit), words)
            };
            let ctor_ref = e.cw.methodref(&new_class, "<init>", &entry_ctor_desc);
            clinit.invokespecial(ctor_ref, entry_ctor_argw, 0);
            let fref = e.cw.fieldref(&fq, &entry.name, &self_desc);
            clinit.putstatic(fref, 1);
        }
        // `$VALUES = $values()` — kotlinc factors the array build into a private `$values()` helper.
        let vfn = e.cw.methodref(&fq, "$values", &format!("(){arr_desc}"));
        clinit.invokestatic(vfn, 0, 1);
        let valref = e.cw.fieldref(&fq, &synthetic_enum_fields.values, &arr_desc);
        clinit.putstatic(valref, 1);
        // `$ENTRIES = EnumEntriesKt.enumEntries((Enum[]) $VALUES)`.
        clinit.getstatic(valref, 1);
        let enumarr = e.cw.class_ref("[Ljava/lang/Enum;");
        clinit.checkcast(enumarr);
        let entries_fn = e.cw.methodref(
            "kotlin/enums/EnumEntriesKt",
            "enumEntries",
            "([Ljava/lang/Enum;)Lkotlin/enums/EnumEntries;",
        );
        clinit.invokestatic(entries_fn, 1, 1);
        let entref = e.cw.fieldref(
            &fq,
            &synthetic_enum_fields.entries,
            "Lkotlin/enums/EnumEntries;",
        );
        clinit.putstatic(entref, 1);
        // An ENUM initializes its `Companion` FIRST, then the serializer statics that read through
        // it (`$cachedSerializer$delegate`) — the reverse of a plain class's `<clinit>` order, and
        // the same precedence the leading field block uses.
        emit_companion_init(e.cw, &mut clinit, &fq, c);
        // A static's store gets its OWN `<clinit>` line entry, as each entry's construction does;
        // after a generated (not hoisted) one the trailing `return` maps to the closing line.
        // `add_method` DROPS a `<clinit>` builder's line marks: only `set_method_lines` lands them.
        let mut stepped_away = false;
        for &(static_index, s) in &owner_statics {
            let Some(init) = static_fields::stored_clinit_initializer(ir, s) else {
                continue;
            };
            if s.line != 0 && clinit_lines.last().map(|&(_, l)| l) != Some(s.line) {
                clinit_lines.push((clinit.bytes.len() as u16, s.line));
                stepped_away |= !ir.is_jvm_companion_hoisted_static(static_index);
            }
            e.emit_static_initializer_store(&fq, static_index, init, &mut clinit);
        }
        if let Some(body) = ir.companion_clinit_body(c.fq_name) {
            e.emit_companion_clinit_body(c, &fq, body, &mut clinit, &mut clinit_lines);
        }
        // The trailing `return` is mapped back only when a generated store STEPPED AWAY from the
        // entries' line; otherwise kotlinc writes no closing entry.
        if stepped_away
            && c.decl_end_line != 0
            && clinit_lines.last().map(|&(_, line)| line) != Some(c.decl_end_line)
        {
            // The frontend's recorded source end, not a guess from entry lines.
            clinit_lines.push((clinit.bytes.len() as u16, c.decl_end_line));
        }
        clinit.ret_void();
        // `max_locals` is exactly what the body allocated (kotlinc writes 0, not a floor of 2).
        clinit.ensure_locals(e.frame.max());
        clinit.link();
        clinit
    };

    // <clinit> is added LAST (built earlier), matching kotlinc's member order.
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);

    // An enum is a VIEW of the same `IrClass` — compute its `@Metadata` (and hence debug tables /
    // annotations) through the shared path, exactly like `emit_class` and `emit_interface_class`.
    let class_metadata = opts
        .emit_class_metadata
        .then(|| build_class_metadata(ir, c, opts, env))
        .flatten();
    if class_metadata.is_some() {
        attach_synth_debug_tables(
            ir,
            env.override_results,
            c,
            &mut cw,
            opts.param_assertions,
            synth_debug_tables::PrimaryConstructorDebug {
                method: emits_primary_ctor.then_some((ctor_desc.as_str(), 0)),
                ..Default::default()
            },
        );
        attach_declared_method_debug(ir, env.override_results, c, &mut cw);
        attach_synth_nullability(ir, c, env.run, &mut cw);
        // kotlinc's synthesized enum members: `valueOf` names its parameter `value` in a
        // LocalVariableTable (with no LineNumberTable), and `getEntries` returns `@NotNull`.
        // `values()` gets neither.
        cw.set_method_debug(
            "valueOf",
            &format!("(Ljava/lang/String;)L{};", c.fq_name()),
            None,
            &[("value".to_string(), "Ljava/lang/String;".to_string(), 0)],
        );
        cw.set_method_nullability(
            "getEntries",
            "()Lkotlin/enums/EnumEntries;",
            Some("Lorg/jetbrains/annotations/NotNull;"),
            &[],
        );
        if !clinit_lines.is_empty() {
            cw.set_method_lines("<clinit>", "()V", &clinit_lines);
        }
    }
    // A user annotation on an enum interns with the CLASS-ATTRIBUTE window, immediately before
    // `@Metadata` — kotlinc's order. It is independent of metadata emission: disabling Kotlin
    // metadata must not discard the class's declared annotations.
    cw.set_class_annotations(&c.applied_annotations);
    if let Some(m) = class_metadata {
        cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
    }
    // The `EnclosingMethod` refs, then each retained row's outer-class ref and simple name, intern
    // at kotlinc's post-metadata window — the same step every other classifier path takes.
    cw.intern_post_metadata_attribute_refs();
    env.run.finish_class(cw)
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
fn emit_method(
    ir: &IrFile,
    fid: u32,
    static_owner: StaticOwner,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
) {
    // An inline-only lambda impl (its body has a non-local `return`) is never a real callable method —
    // it exists only to be spliced via its `inline_body`. Emitting it would produce an invalid, dead
    // method (an `areturn` of the enclosing fn's type from the lambda's signature). Skip it.
    if standalone_method_is_elided(ir, fid, env) {
        return;
    }
    emit_method_inner(ir, fid, static_owner, owner, facade, cw, instance, env);
}

/// Emit `fid`'s body as a `$DefaultImpls` static: same code and slot layout as the instance method
/// (`this` is slot 0), but `public static` and a descriptor whose first parameter is the receiver.
/// This is the shape `-jvm-default=disable` puts every interface body into.
fn emit_holder_method(
    ir: &IrFile,
    fid: u32,
    receiver: crate::types::TypeName,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    emit_placed_method(
        ir,
        fid,
        MethodPlacement::Holder(receiver),
        owner,
        facade,
        cw,
        env,
    );
}

fn emit_method_inner(
    ir: &IrFile,
    fid: u32,
    static_owner: StaticOwner,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
) {
    let singleton = jvm_static_members::singleton_static_owner(ir, fid).filter(|_| instance);
    let placement = match singleton {
        Some(singleton) => MethodPlacement::Singleton {
            static_owner,
            singleton,
        },
        None => MethodPlacement::Declared {
            static_owner,
            instance,
        },
    };
    emit_placed_method(ir, fid, placement, owner, facade, cw, env);
    if crate::jvm::suspend_inline_functions::has_for_inline_copy(ir, fid)
        && env.emit_time_machines.transformed(fid).is_some()
    {
        let copy = MethodPlacement::ForInlineCopy {
            static_owner,
            instance: instance && singleton.is_none(),
            singleton,
        };
        emit_placed_method(ir, fid, copy, owner, facade, cw, env);
    }
}

fn emit_placed_method(
    ir: &IrFile,
    fid: u32,
    placement: MethodPlacement,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let (static_owner, instance, holder_receiver, singleton) = match placement {
        MethodPlacement::Declared {
            static_owner,
            instance,
        } => (Some(static_owner), instance, None, None),
        MethodPlacement::ForInlineCopy {
            static_owner,
            instance,
            singleton,
        } => (Some(static_owner), instance, None, singleton),
        MethodPlacement::Holder(receiver) => (None, true, Some(receiver), None),
        MethodPlacement::Singleton {
            static_owner,
            singleton,
        } => (Some(static_owner), false, None, Some(singleton)),
    };
    // Common IR numbers a member's receiver as value 0 even where the JVM method has no `this`.
    let receiver_value = instance || singleton.is_some();
    let f = &ir.functions[fid as usize];
    let method_name = placement.method_name(&f.name);
    let body = f.body.unwrap();
    let param_tys = jvm_function_params(ir, fid);
    let ret = jvm_declared_ty(&env.override_results.physical_result(ir, fid));
    let mut e = Emitter::new(ir, cw, env, static_owner, owner, facade, ret, [body]);
    e.function = Some(fid);
    e.method_exports_private_access = static_accessors::method_exports_private_calls(
        ir,
        &env.run.static_accessor_plan.borrow(),
        fid,
    );
    e.export_private_calls = e.method_exports_private_access;
    e.finally_markers = ir.inline_fns.contains(&fid);
    // kotlinc's transformer keeps a suspend body's own local names: they name the spills.
    let transformed = env.emit_time_machines.transformed(fid);
    // A body kept for inlining stays as codegen wrote it, its suspension markers included.
    let marked = placement.is_for_inline_copy()
        || crate::jvm::suspend_inline_functions::keeps_marked_body(ir, fid);
    // Suspend lowering does not preserve source-local expression IDs.
    e.record_locals = (ir.fn_decl_lines.contains_key(&fid)
        || ir.fn_debug_locals.contains(&fid)
        || ir
            .generated_function_publication(fid)
            .is_some_and(|publication| publication.debug.records_locals()))
        && (!ir.suspend_funs.contains(&fid) || transformed.is_some());
    if instance {
        let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj(owner));
        e.slots.insert(0, (receiver, Ty::obj(owner)));
    }
    e.singleton_receiver = singleton;
    for (i, t) in param_tys.iter().enumerate() {
        let vi = i as u32 + u32::from(receiver_value);
        let slot = e.frame.enter(FrameKey::Value(vi), *t);
        e.slots.insert(vi, (slot, *t));
    }
    let transformed = transformed.map(|machine| {
        // A suspend lambda's `invokeSuspend` is its own continuation and takes no completion.
        let completion = machine.lambda.is_none().then(|| {
            param_tys
                .len()
                .checked_sub(1)
                .and_then(|index| u32::try_from(index).ok())
                .expect("a transformed suspend function has a physical completion parameter")
                + u32::from(receiver_value)
        });
        let slot = e.arm_transformed_machine(machine, completion);
        if marked {
            let inlined = crate::jvm::suspend_inline_functions::inlined_invokes(ir, fid, machine);
            e.leave_inlined_invokes_unmarked(inlined);
        }
        (machine, slot)
    });
    // A function whose coroutine machine emission owns reads its continuation from a slot this
    // emitter picks. While the frame is being discovered that is the `$completion` parameter, which
    // is one `aload` exactly like the machine's own local, so both passes allocate the same slots.
    // A function whose coroutine machine emission owns gets its machine's own locals FIRST, right
    // above the parameters and below everything the body allocates. Leasing them as backend
    // temporaries is what makes every frame describe them — including the merged frames of a spliced
    // lambda, which are built from the emitter's own slot view. Reserved identically on both passes,
    // so the spill plan the first one reads is expressed in the slots the second one uses.
    let machine_slots = env.emit_time_machines.suspensions(fid).map(|suspensions| {
        let object = Ty::nullable(Ty::obj("kotlin/Any"));
        // Typed as the machine's OWN continuation class: every read of this slot goes on to touch
        // its `label`, `result` and spill fields, which the supertype does not declare.
        let continuation_ty = Ty::obj(&coroutine_machine::continuation_internal(
            ir, fid, owner, &f.name,
        ));
        // Held for the whole method: nothing leaves them.
        let [result, continuation, suspended] = [object, continuation_ty, object].map(|ty| {
            let slot = e.frame.enter_temp(TempRole::CoroutineMachine, ty).slot();
            e.lease_temporary(slot, ty);
            slot
        });
        e.continuation_slot = Some(continuation);
        e.machine_suspensions = suspensions
            .iter()
            .map(|suspension| (suspension.call, suspension.resumed_box))
            .collect();
        crate::trace_compiler!(
            "suspend",
            "machine wanted fid={fid} calls={:?}",
            suspensions.iter().map(|s| s.call).collect::<Vec<_>>()
        );
        coroutine_machine::MachineSlots {
            result,
            continuation,
            suspended,
        }
    });
    // kotlinc's writer visits a method HEADER before its code, so the name, descriptor, generic
    // `Signature` and annotation types precede every constant the body introduces. krusty builds the
    // body first, so reserve those entries here to land them in the same order.
    let reserved_desc = match holder_receiver {
        // The holder's static takes the receiver as parameter 0: `f(I)`, `g(I, int)`.
        Some(receiver) => {
            let mut with_receiver = vec![Ty::obj_name(receiver)];
            with_receiver.extend_from_slice(&param_tys);
            method_descriptor(&with_receiver, ret)
        }
        None => method_descriptor(&param_tys, ret),
    };
    crate::trace_compiler!(
        "emit_method",
        "method fid={fid} name={} params={:?} descriptor={reserved_desc}",
        f.name,
        f.params,
    );
    e.regeneration_site = bytecode_inline_call::RegenerationSite::of_function(
        ir,
        fid,
        holder_receiver.is_none(),
        &reserved_desc,
        env,
    );
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    // The cache's compiler-invented factories and accessor record no generic `Signature`: the
    // attribute exists for a source or Java caller, and nothing can name these methods to call
    // them. Keep this scoped to the producer's exact identities; unrelated synthetic methods may
    // still have a source-visible generic contract.
    // kotlinc maps a lambda literal's body without generics, and a compiler-synthesized lambda's;
    // an anonymous function's keeps them.
    let synthesized = ir.lambda_class_provenance(fid)
        == Some(crate::ir::type_reflection::LambdaClassProvenance::SynthesizedAdapter);
    let method_sig = (!ir.serialization_cache_methods.contains(&fid)
        && !ir.is_lambda_literal(fid)
        && !synthesized)
        .then(|| declared_method_signature(&signature_formatter, ir, env.override_results, fid))
        .flatten();
    let reserved_sig = match holder_receiver {
        Some(receiver) => default_impls::holder_method_signature(
            &signature_formatter,
            ir,
            receiver,
            method_sig.as_deref(),
            &method_descriptor(&param_tys, ret),
        ),
        None => match ir.jvm_suspend_impl_bodies.get(&fid).copied() {
            Some((receiver, _)) => default_impls::holder_method_signature(
                &signature_formatter,
                ir,
                receiver,
                method_sig.as_deref(),
                &method_descriptor(f.params.get(1..).unwrap_or_default(), ret),
            ),
            None => method_sig,
        },
    };
    let access = placement.method_access(method_access::declared_method_access(
        ir,
        fid,
        owner,
        instance,
        holder_receiver.is_some(),
        env.lambda_modes,
    ));
    let reserved_sig = method_signatures::written_signature(
        access,
        method_signatures::keeps_signature_when_synthetic(ir, fid),
        &reserved_desc,
        reserved_sig,
    );
    let lambda_impl = ir.lambda_own_params_from.contains_key(&fid);
    let declared_nullability::DeclaredNullability {
        result: ret_ann,
        parameters: param_anns,
    } = override_result_emission::declared_nullability(ir, e.override_results, fid);
    let emitted_param_anns = holder_receiver.map_or_else(
        || param_anns.clone(),
        |_| {
            std::iter::once(Some("Lorg/jetbrains/annotations/NotNull;"))
                .chain(param_anns.iter().copied())
                .collect()
        },
    );
    let declared_annotations = ir
        .function_annotations
        .get(&fid)
        .cloned()
        .unwrap_or_default();
    let nullability_annotated =
        function_annotations::nullability_annotated(ir, fid, &declared_annotations)
            && !placement.is_for_inline_copy();
    // The USER annotations on this function's parameters.
    let user_param_anns: &[crate::ir::DeclarationAnnotations] = ir
        .fn_param_annotations
        .get(&fid)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let method_parameters = if env.java_parameters {
        super::method_parameters::function(ir, fid, &param_tys, holder_receiver)
    } else {
        Vec::new()
    };
    e.cw.reserve_method_pool_with_annotations(
        &method_name,
        &reserved_desc,
        reserved_sig.as_deref(),
        &[],
        &declared_annotations,
        &method_parameters,
    );
    e.cw.reserve_method_parameter_annotation_pool(
        ret_ann,
        user_param_anns,
        &emitted_param_anns,
        nullability_annotated,
    );
    let mut code = CodeBuilder::new(e.frame.size());
    method_entry::emit(
        fid,
        &param_tys,
        receiver_value,
        holder_receiver,
        env,
        &mut e,
        &mut code,
    );
    // A LAMBDA IMPL's LineNumberTable starts at the post-guard pc, mapped to the body's line.
    if let Some(line) = lambda_impl
        .then(|| function_debug::lambda_entry_line(ir, fid))
        .flatten()
    {
        code.mark_line(line);
    }
    // kotlinc opens every EMITTED `inline fun` body with an inline-depth marker: `iconst_0;
    // istore_<n>` into a synthetic `$i$f$<name>` int local covering the body — its inliner tracks
    // splice depth through these. Emitted after the parameter guards; the body's LineNumberTable
    // then naturally starts at the post-store pc.
    let inline_marker: Option<(u16, u16)> =
        function_debug::opens_with_inline_depth_marker(ir, fid, receiver_value).then(|| {
            let slot = e
                .frame
                .enter_temp(TempRole::InlineDepthMarker, Ty::Int)
                .slot();
            code.push_int(0, e.cw);
            store(Ty::Int, slot, &mut code);
            (slot, code.bytes.len() as u16)
        });
    // The marker and the parameter guards carry no line. The method's first entry is the first
    // statement, or the expression body — kotlinc does not also record the signature line there.
    // Building the machine: everything the discovery pass learned is in hand, so the entry,
    // the dispatch and the state it re-enters can be emitted around the same body.
    let machine_states = match (machine_slots, env.run.machine_plan(fid)) {
        (Some(slots), Some(plan)) => {
            // The continuation the machine is re-entered on is the trailing `$completion`
            // parameter. Resolve it BEFORE anything is armed: arming is what makes the body emit
            // markers and spills, and a machine armed for a prologue that then declined would leave
            // both behind — markers no erasure pass is reached for, over a slot never assigned.
            let completion = param_tys.len().saturating_sub(1) as u32 + u32::from(receiver_value);
            match e.slots.get(&completion).map(|&(slot, _)| slot) {
                Some(completion) => {
                    let internal =
                        coroutine_machine::continuation_internal(ir, fid, owner, &f.name);
                    e.machine = Some(coroutine_machine::Machine {
                        plan,
                        slots,
                        internal,
                        // An instance method is re-entered on its receiver, which the continuation
                        // keeps.
                        receiver: instance.then(|| owner.to_string()),
                        // A private method is not callable from the continuation class, whether it
                        // is a member or a top-level function; a synthetic static on the owner is.
                        bridge: ir
                            .method_visibility(fid)
                            .is_private()
                            .then(|| coroutine_machine::access_bridge_name(&f.name)),
                    });
                    e.emit_machine_prologue(completion, &mut code)
                }
                // No continuation to dispatch on, so there is no machine to build. Decline the
                // compile — as a discovery pass that found no marker does — and emit the rest of
                // this method with nothing armed, so the bytes that get discarded carry neither a
                // marker nor a read of a slot that was never assigned.
                None => {
                    crate::trace_compiler!("suspend", "machine fid={fid} NO CONTINUATION SLOT");
                    e.machine_suspensions.clear();
                    env.run
                        .set_inline_bail("a coroutine machine with no continuation parameter");
                    None
                }
            }
        }
        _ => None,
    };
    // The verifier's view of the frame on entry: what the discovery pass computes every later
    // state from. Taken before the body runs, while the parameters are the only locals assigned.
    let entry_locals = machine_slots
        .filter(|_| !e.machine_suspensions.is_empty() && !env.run.has_machine_plan(fid))
        .map(|slots| e.verif_slots_upto(slots.result));
    e.emit(body, &mut code);
    // Discovery: this body is the post-splice bytecode the spill set has to be read from. Read it,
    // then erase the markers so nothing synthetic can reach a class file even though these bytes are
    // about to be discarded.
    if let Some(entry_locals) = entry_locals {
        // One state per site emitted, which is what the markers name.
        let expected = e.machine_next_ordinal;
        match coroutine_machine::discover(&code, machine_slots, &entry_locals, e.cw, expected) {
            Some(plan) => {
                crate::trace_compiler!(
                    "suspend",
                    "machine plan fid={fid} body_locals={} suspensions={:?}",
                    plan.body_locals,
                    plan.suspensions
                );
                env.run.record_machine_plan(fid, plan);
            }
            None => {
                // No marker means the body never reached the suspension: the emitter declined the
                // splice, so the lambda is a real closure after all and its suspension belongs to a
                // method that has no continuation to pass. Routing has already taken the standalone
                // `invoke` away — emitting now would leave an `invokedynamic` pointing at a method
                // that does not exist. Decline the whole compile instead, as this shape did before
                // the machine existed.
                crate::trace_compiler!("suspend", "machine plan fid={fid} DECLINED");
                env.run
                    .set_inline_bail("suspension inside a lambda that was not spliced");
            }
        }
    }
    // The implicit `return` for a `Unit` function is dead code when the body already diverges
    // (`fun foo() { throw … }`): an unreachable `return` after `athrow` has no stack-map frame and
    // the verifier rejects it. Skip it exactly when the body can't fall through.
    if ret == Ty::Unit && !e.discarding_diverges(body) {
        // kotlinc maps the implicit `return` to the body's closing-`}` line. `fn_close_lines` has
        // entries only for PARSED declarations, so a synthesized body (no entry) keeps its
        // decl-line fallback table.
        if let Some(&close) = ir.fn_close_lines.get(&fid) {
            code.mark_line(close);
        }
        code.implicit_ret_void();
    }
    // The dispatch's states live inside the spliced body, where only a marker could name them.
    // Bind each one where its marker ended up, then erase every marker: they exist to survive the
    // splice, not to reach a class file.
    if let Some((_, resumes, default)) = machine_states {
        let source_file = e.cw.source_file_name();
        // Bind the states BEFORE the default block: that block ends in `athrow`, and binding an
        // offset is refused once the stream is dead.
        let found = code.marker_positions().unwrap_or_default();
        for (at, kind, ordinal) in found {
            let is_resume = match kind {
                crate::jvm::classfile::CoroutineMarker::Resume => true,
                crate::jvm::classfile::CoroutineMarker::Join => false,
                crate::jvm::classfile::CoroutineMarker::Suspension => continue,
            };
            // Bind the label the dispatch's restore block jumps to (the resume point) and the join
            // the two paths out of the suspension meet at.
            let Some(machine) = e.machine.clone().filter(|_| e.machine_entered) else {
                continue;
            };
            if machine.plan.suspensions.get(ordinal as usize).is_none() {
                continue;
            }
            let target = match is_resume {
                true => e.machine_resumes.get(ordinal as usize),
                false => e.machine_joins.get(ordinal as usize),
            };
            let Some(&target) = target else {
                continue;
            };
            // The marker sits AT the position: it is the first instruction there, and becomes `nop`s.
            code.bind_target_at(target, at);
        }
        e.emit_machine_states(&resumes, &mut code);
        e.emit_machine_default(default, &mut code);
        if let Some(machine) = e.machine.clone() {
            let bytes =
                coroutine_machine::build_continuation_class(coroutine_machine::ContinuationClass {
                    internal: &machine.internal,
                    outer: owner,
                    outer_method: &f.name,
                    outer_descriptor: &reserved_desc,
                    plan: &machine.plan,
                    major: Some(e.cw.major()),
                    source_file: source_file.as_deref(),
                    receiver: machine.receiver.as_deref(),
                    bridge: machine.bridge.as_deref(),
                    completion_box: crate::jvm::suspend::completion_box(ir, fid),
                });
            env.run
                .machine_classes
                .borrow_mut()
                .push((machine.internal.clone(), bytes));
            // The bridge itself lives on the owner, beside the method it re-enters.
            if let Some(bridge) = machine.bridge.as_deref() {
                if let Some((name, descriptor, body)) = coroutine_machine::build_access_bridge(
                    e.cw,
                    owner,
                    &f.name,
                    &reserved_desc,
                    instance,
                ) {
                    debug_assert_eq!(name, bridge);
                    // `public static final synthetic`, as kotlinc emits it.
                    e.cw.add_method(0x1019, &name, &descriptor, &body);
                }
            }
        }
    }
    // The `$i$f$<name>` marker's LocalVariableTable entry covers the body from the post-store pc —
    // kotlinc writes it even when no other local is recorded. The table's strings intern when the
    // method is added, right after this body (kotlinc's per-method visit order).
    if let Some((slot, start)) = inline_marker {
        let marker_name = format!("$i$f${}", ir.name_before_part_suffix(fid));
        code.add_local_entry(start, None, slot, &marker_name, "I");
    }
    // Method locals precede `this` and parameters in kotlinc's table order.
    if e.record_locals {
        let locals = std::mem::take(&mut e.open_locals);
        e.record_open_locals(locals, |_| None, &mut code);
    }
    // Suspend rewriting invalidates source-local expression ids, but its physical parameters remain
    // stable and reflection/debug tooling still expects `$completion` (plus the declared receiver and
    // arguments) in the LocalVariableTable.
    let continuation_result = ir
        .function_parameter_identities(fid)
        .is_some_and(|identities| {
            matches!(
                identities,
                [crate::ir::IrParameterIdentity {
                    role: crate::ir::IrParameterRole::Generated(
                        crate::ir::IrGeneratedParameterRole::ContinuationResult
                    ),
                    ..
                }]
            )
        });
    if continuation_result {
        assert!(
            instance,
            "a continuation's invokeSuspend is an instance method"
        );
        assert_eq!(
            param_tys.len(),
            1,
            "a continuation's invokeSuspend has exactly its resume result"
        );
        let result_name = crate::jvm::parameter_names::function_locals(ir, fid, &param_tys)
            .and_then(|names| names.into_iter().next().flatten())
            .expect("a continuation result has a JVM local name");
        let this_desc = format!("L{owner};");
        code.add_local_entry(0, None, 0, "this", &this_desc);
        code.add_local_entry(0, None, 1, &result_name, "Ljava/lang/Object;");
    } else if e.record_locals || ir.suspend_funs.contains(&fid) || holder_receiver.is_some() {
        let parameter_identities = ir.function_parameter_identities(fid);
        if let Some(identities) = parameter_identities {
            assert_eq!(
                identities.len(),
                param_tys.len(),
                "debug parameter identities exactly match physical arity"
            );
        }
        if instance {
            let this_desc = format!("L{owner};");
            let receiver_name = holder_receiver.map_or("this", |_| "$this");
            code.add_local_entry(0, None, 0, receiver_name, &this_desc);
        }
        let mut slot = u16::from(instance);
        let local_names = parameter_identities.and_then(|_| {
            let names = crate::jvm::parameter_names::function_locals(ir, fid, &param_tys);
            crate::jvm::parameter_names::placed(ir, fid, holder_receiver, names)
        });
        for (i, t) in param_tys.iter().enumerate() {
            let pname = local_names
                .as_ref()
                .and_then(|names| names.get(i))
                .and_then(Clone::clone);
            if let Some(pname) = pname {
                let pdesc = local_variable_desc(*t);
                code.add_local_entry(0, None, slot, &pname, &pdesc);
            }
            slot += slot_words(*t);
        }
    }
    // Every coroutine marker, on every path out of this function. A marker exists to survive being
    // relocated into a spliced inline body and is read once the bytes are final; `impdep1` is
    // reserved by JVMS §6.2 and must never reach a class file. Erasing here — after the last byte
    // is emitted and before the code is linked, with no `return` in between — is what makes that a
    // property of method emission rather than of whichever machine branch happened to run.
    let _ = code.erase_markers();
    code.ensure_locals(e.frame.max());
    code.link();
    // A method with own type parameters (`fun <T> …`) → the tparam-based signature; otherwise a method
    // whose concrete param/return type is PARAMETERIZED (`getXs(): List<String>`, `copy(List<String>)`)
    // → its generic signature. `f.params`/`f.ret` are the SOURCE types (retain `<…>` args); `param_tys`/
    // `ret` are erased.
    let desc = reserved_desc;
    // A method whose emission failed can stop with an incomplete operand stack; the pass is
    // discarded, so its frames are never computed.
    if env.run.emission_failed() {
        return;
    }
    if marked {
        code.keep_for_inlining();
    }
    e.cw.add_method_sig(access, &method_name, &desc, &code, reserved_sig.as_deref());
    if let Some((machine, slot)) = transformed.filter(|_| !marked) {
        let method = (method_name.as_ref(), desc.as_str());
        transformed_suspensions::request_transform(ir, e.cw, fid, method, machine, slot);
    }
    e.cw.set_method_parameters(&method_name, &desc, &method_parameters);
    // kotlinc annotates a reference return and each reference parameter of a declared method.
    if nullability_annotated
        && (ret_ann.is_some() || emitted_param_anns.iter().any(Option::is_some))
    {
        e.cw.set_method_nullability(&method_name, &desc, ret_ann, &emitted_param_anns);
    }
    // The USER parameter annotations (their complete payloads are already interned, above).
    if !user_param_anns.is_empty() {
        e.cw.set_method_param_annotations(&method_name, &desc, user_param_anns);
    }
    if ir.deprecated_methods.contains(&fid) {
        e.cw.mark_method_deprecated(&method_name, &desc);
    }
    // User annotations declared on the function. A HIDDEN-deprecated declaration additionally gets
    // `ACC_SYNTHETIC`: kotlinc keeps it only for binary compatibility, and a consumer reads both
    // facts (the annotation for resolution, the flag for the JVM) off this realization.
    function_annotations::emit_recorded(ir, e.cw, fid, &method_name, &desc);
}

/// A `Ty` as a JVM generic-signature type element: a primitive in a generic position is its BOXED wrapper
/// (`Int` → `Ljava/lang/Integer;`), a reference maps its internal (`kotlin/Any` → `java/lang/Object`) and
/// carries its (recursively formatted) type arguments. `None` for a shape not representable here.
/// The generic `Signature` element for a parameterized concrete type (`List<String>` →
/// `Ljava/util/List<Ljava/lang/String;>;`); `None` when erasure loses nothing. `T?` unwraps (generics
/// survive nullability). Retained formals include an enclosing declaration's parameters.
/// A FINAL field's generic `Signature`, which follows the return-position rule: no declaration-site
/// wildcards (a non-final field's goes through [`JvmSignatureFormatter::field_signature`]). A
/// constructor PARAMETER of the same declared type takes them, so it goes through
/// [`parameterized_sig_at`] with the parameter mode instead.
fn parameterized_sig(formatter: &JvmSignatureFormatter<'_>, ty: &Ty) -> Option<String> {
    parameterized_sig_at(formatter, ty, Wildcards::Suppressed)
}

fn parameterized_sig_at(
    formatter: &JvmSignatureFormatter<'_>,
    ty: &Ty,
    wildcards: Wildcards,
) -> Option<String> {
    formatter.declaration_type_use(ty, wildcards)
}

fn property_jvm_signatures(
    formatter: &JvmSignatureFormatter<'_>,
    ty: &Ty,
    type_parameter: Option<&str>,
) -> JvmPropertySignatures {
    formatter.property_signatures(ty, type_parameter)
}

/// A method's generic `Signature`, from whichever source applies: its own declared type parameters, the
/// `suspend` CPS shape, or a parameterized concrete parameter/return type. `None` when erasure loses
/// nothing. Shared by concrete and abstract emission — an abstract method has no body, which is not a
/// reason to drop its signature.
/// `Signature` for a member whose SEMANTIC types mention enclosing-class type parameters: each
/// position is a bare `T<name>;` reference or a plain non-generic descriptor. `None` when any
/// position needs deeper generic formatting (those flow through the ordinary formatters).
fn method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    fid: u32,
    f: &crate::ir::IrFunction,
) -> Option<String> {
    let signature = method_signature_shape(formatter, ir, fid, f)?;
    // A `Signature` attribute exists to say what the DESCRIPTOR cannot — a type variable, a type
    // argument, a wildcard. One that spells the descriptor back carries nothing, and kotlinc omits it:
    // `fun a(x: Array<String>)` signs `([Ljava/lang/String;)V`, which is already its descriptor, so
    // the attribute is absent. (Only equality is tested, so a shape whose descriptor is computed
    // differently — suspend, value-class mangling — simply keeps its attribute.)
    (signature != ir_method_desc(&f.params, &f.ret)).then_some(signature)
}

fn method_signature_shape(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    fid: u32,
    f: &crate::ir::IrFunction,
) -> Option<String> {
    // Pick the semantic signature category first. Formatting returns `None` both when no optional
    // Signature attribute is needed and when the selected shape is invalid, so chaining formatters
    // with `or_else` would incorrectly treat an encoding error as permission to try a less complete
    // shape. `JvmSignatureFormatter` records a precise emit error for the latter case.
    if let (Some(generic), Some((_, declared_ret))) =
        (ir.signatures.get(&fid), ir.suspend_declared_sigs.get(&fid))
    {
        let ret = generic.ret.as_ref().unwrap_or(declared_ret);
        return suspend_generic_method_sig(formatter, generic, ret);
    }
    if let Some(generic) = ir.jvm_value_class_member_signatures.get(&fid) {
        return formatter.method_signature(generic, f, None);
    }
    if let Some(generic) = ir.signatures.get(&fid) {
        let generic = value_class_signatures::physical_generic_signature(ir, fid, generic);
        return formatter.method_signature(&generic, f, vararg_parameter_position(ir, fid));
    }
    if let (Some((params, ret)), Some(_)) = (
        ir.member_semantic_sigs.get(&fid),
        ir.suspend_declared_sigs.get(&fid),
    ) {
        return suspend_method_sig(formatter, params, ret);
    }
    if let Some((params, ret)) = value_class_signatures::physical_member_signature(ir, fid) {
        // A member using ENCLOSING-CLASS type parameters declares none: they belong to the class
        // header's own signature (`(TT;)TT;`).
        return formatter.method_positions(&params, &ret);
    }
    if let Some((params, declared_ret)) = ir.suspend_declared_sigs.get(&fid) {
        // A value-class RETURN survives in the continuation's type argument as the value class
        // itself (`Continuation<? super OrganizationId>`), not its erasure. The VC pass ran
        // before the suspend pass and already erased `declared_ret` to the underlying, so recover the
        // declared return from `vc_declared_sigs` when this function had one.
        let ret = ir
            .vc_declared_sigs
            .get(&fid)
            .map(|(_, _, value_class_ret)| value_class_ret)
            .unwrap_or(declared_ret);
        return suspend_method_sig(formatter, params, ret);
    }
    if let Some((params, ret)) = value_class_signatures::declared_value_class_signature(ir, fid) {
        return formatter.method_positions(&params, &ret);
    }
    method_parameterized_sig(
        formatter,
        &signature_function_params(ir, fid),
        &f.ret,
        vararg_parameter_position(ir, fid),
    )
}

fn suspend_generic_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    generic: &crate::ir::IrGenericSig,
    ret: &Ty,
) -> Option<String> {
    let mut signature = jvm_type_params(formatter, generic)?;
    signature.push_str(&suspend_method_sig(formatter, &generic.params, ret)?);
    Some(signature)
}

/// The generic `Signature` of a `suspend fun`'s CPS method. The declared return type survives only in
/// the trailing continuation's type argument — `suspend fun f(a: String)` compiles to
/// `f(String, Continuation): Object` but signs as
/// `(Ljava/lang/String;Lkotlin/coroutines/Continuation<-Lkotlin/Unit;>;)Ljava/lang/Object;`, the `-`
/// being `? super`. Takes the DECLARED parameters and return, not the rewritten ones.
fn suspend_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
) -> Option<String> {
    // The suspend return travels as `Continuation<-RET>`, a PARAMETER of the erased method, and
    // kotlinc wildcards inside it: `suspend fun <U> f(): Cont<U>` signs
    // `(Lkotlin/coroutines/Continuation<-LCont<+TU;>;>;)Ljava/lang/Object;`.
    let continuation = formatter.continuation(*ret, Wildcards::Declared)?;
    let mut s = String::from("(");
    for p in params {
        s.push_str(&formatter.parameter_ty(p)?);
    }
    s.push_str(&continuation);
    s.push_str(")Ljava/lang/Object;");
    Some(s)
}

/// The position of `fid`'s `vararg` among its lowered parameters. The recorded index counts the
/// declared context and value parameters; captures and the extension receiver are lowered among
/// them, so the position is the parameter whose identity is that declared parameter.
fn vararg_parameter_position(ir: &IrFile, fid: u32) -> Option<usize> {
    let vararg = ir.fn_varargs.get(&fid)?;
    ir.fn_params
        .get(&fid)?
        .identities
        .iter()
        .enumerate()
        .filter(|(_, identity)| identity.role.is_declared_parameter())
        .nth(vararg.index)
        .map(|(position, _)| position)
}

/// A method's generic `Signature` when a concrete param/return type is parameterized (`getXs()` →
/// `()Ljava/util/List<Ljava/lang/String;>;`); non-generic positions keep their erased descriptor,
/// `None` when none are parameterized.
fn method_parameterized_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
    vararg_index: Option<usize>,
) -> Option<String> {
    // Runs for every emitted method — bail before building any string when no position can carry one.
    let is_parameterized = |t: &Ty| {
        let inner = t.non_null();
        matches!(inner.kind(), TyKind::Fun(_) | TyKind::Intersection(_))
            || matches!(inner.kind(), TyKind::Obj(_, arguments) if !arguments.is_empty())
    };
    if !params
        .iter()
        .chain(std::iter::once(ret))
        .any(is_parameterized)
    {
        return None;
    }
    // A `Unit` parameter is the `kotlin.Unit` object, not `void`.
    let s = formatter.method_positions_with_vararg(params, ret, vararg_index)?;
    (s != ir_method_desc(params, ret)).then_some(s)
}

fn default_mask_count(param_count: usize) -> usize {
    param_count.div_ceil(32).max(1)
}

fn default_mask_bit(param_index: usize) -> i32 {
    (1u32 << (param_index % 32)) as i32
}

fn full_default_masks(param_count: usize) -> Vec<i32> {
    (0..default_mask_count(param_count))
        .map(|chunk| {
            let start = chunk * 32;
            let end = ((chunk + 1) * 32).min(param_count);
            (start..end).fold(0i32, |mask, i| mask | default_mask_bit(i))
        })
        .collect()
}

/// The owner and JVM carrier of a value class adapted through its own `box-impl`/`unbox-impl`.
/// The carrier comes from the IR's value-class facts, which cover a value class declared in
/// another file of the module or in a dependency as well as one this file declares.
fn vc_adapter_shape(ir: &IrFile, vc: &Ty) -> (String, Ty) {
    let classifier = vc
        .obj_internal()
        .expect("a value-class adapter names its value class");
    let carrier = crate::jvm::value_classes::boxed_value_class_carrier(ir, classifier)
        .expect("a value-class adapter names a value class the IR's facts carry");
    (classifier.render(), jvm_declared_ty(&carrier))
}

/// Emit `VC.box-impl(<underlying>)LVC;` (static) — boxes the underlying value on the stack into `VC`.
fn emit_box_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let (fq, u) = vc_adapter_shape(ir, vc);
    let m = cw.methodref(&fq, "box-impl", &format!("({})L{fq};", type_descriptor(u)));
    code.invokestatic(m, slot_words(u) as i32, 1);
}

/// Emit `VC.unbox-impl()<underlying>` (virtual) — unboxes the `VC` on the stack to its underlying.
fn emit_unbox_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let (fq, u) = vc_adapter_shape(ir, vc);
    let m = cw.methodref(&fq, "unbox-impl", &format!("(){}", type_descriptor(u)));
    code.invokevirtual(m, 0, slot_words(u) as i32);
}

/// The target-neutral identity and selected declaration shape shared by JVM property reads and writes.
/// Keeping these fields together prevents the two realizers from accepting subtly different owner,
/// interface, or physical-type inputs as the semantic operation grows more metadata.
struct PropertyOperation<'a> {
    expression: crate::ir::ExprId,
    receiver: Option<crate::ir::ExprId>,
    owner: TypeName,
    name: &'a str,
    ty: &'a Ty,
    interface: bool,
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    override_results: &'a crate::jvm::override_results::OverrideResults,
    /// The narrow bytecode provider — lets the emitter read a cross-module `inline fun`'s compiled
    /// body (`bodies.body`) to splice it at the call site (the bytecode inliner).
    bodies: &'a dyn MethodBodies,
    /// The per-emit-run accumulators — the deep sites record a used lambda / an emit-or-inline bail
    /// here (formerly thread-locals).
    run: &'a EmitRun,
    /// `-jvm-default`, so a call site can tell where an interface's `$default` synthetic lives.
    jvm_default: JvmDefaultMode,
    /// The `@Metadata` stamp an anonymous object regenerated by an inline call of this class carries.
    metadata_stamp: crate::jvm::classfile::MetadataStamp,
    property_realizations: &'a crate::jvm::property_realizations::PropertyRealizations,
    default_call_operands: &'a crate::jvm::default_call_operands::DefaultCallOperands,
    sam_wrapper_realizations: &'a crate::jvm::sam_wrappers::SamWrapperRealizations,
    local_delegate_access: &'a crate::jvm::local_delegate_accessors::HelperAccess,
    suspended_result_returns: &'a crate::jvm::suspend::SuspendedResultReturns,
    intrinsic_probe_continuations: &'a crate::jvm::suspend::IntrinsicProbeContinuations,
    /// The machines kotlinc's transformer builds, which a placed suspend lambda's points join.
    emit_time_machines: &'a crate::jvm::suspend::EmitTimeMachines,
    /// The exact source class whose code this emitter is writing. A generated holder has no
    /// source-static ownership; it must route every private static access through the owner.
    static_owner: Option<StaticOwner>,
    /// Source or backend-generated function whose body is currently being emitted.
    function: Option<u32>,
    /// Declaration scope of a scratch inline-lambda body, without changing emission ownership.
    type_parameter_function: Option<u32>,
    export_private_calls: bool,
    method_exports_private_access: bool,
    /// This method is `inline`, so each copy of a `finally` is bracketed by
    /// `InlineMarker.finallyStart`/`finallyEnd`.
    finally_markers: bool,
    /// How many `finally` bodies are currently being emitted. The marker argument is one more
    /// than the bodies already open, so the outermost copy is `1`.
    finally_marker_depth: u32,
    /// Interface companion `$$INSTANCE` self-reads for this class, fixed from `static_owner`.
    self_companion: Option<TypeName>,
    /// Checked classifier declarations: which kind of classifier an operand's type names.
    classifiers: &'a dyn BackendClassifierSource,
    /// One checked inventory for physical call owners and semantic array-cast leaf proofs.
    dispatch_classifiers: std::rc::Rc<crate::jvm::member_dispatch::CheckedDispatchClassifiers<'a>>,
    owner: String,
    facade: String,
    /// The public facade a multifile part's non-private members are invoked through.
    multifile_facade: Option<String>,
    slots: HashMap<u32, (u16, Ty)>,
    /// The slots the backend owns, leased and released by `backend_temporaries`. They are not
    /// semantic locals and deliberately do not live in `slots`, which is keyed by real value ids.
    temporaries: backend_temporaries::BackendTemporaries,
    /// Semantic locals that are in lexical scope but not definitely assigned on the current edge.
    /// JVM frames render their physical slots as `top` until every incoming edge has stored them.
    unassigned_values: HashSet<u32>,
    /// Incoming definite-assignment state by control-flow label. Union is the verifier lattice:
    /// unassigned on any incoming edge means `top` at the merge.
    label_unassigned_values: HashMap<Label, HashSet<u32>>,
    /// A semantic value carried on the operand stack by every jump to a label. The value becomes
    /// stack-resident only when that label is bound; intervening handler emission must not consume
    /// an earlier jump's value.
    label_stack_resident_values: HashMap<Label, u32>,
    /// Safe-call guards that share one null exit (`a?.b?.c`), by guard `When`: the label, and whether
    /// this guard is the chain's outermost one, which binds it and yields the null result.
    safe_call_null_exits: HashMap<u32, (Label, bool)>,
    /// A safe-call receiver already on the operand stack. The one read of this temporary consumes
    /// it in place (`dup; ifnull`) instead of loading a slot that was never stored.
    stack_resident_value: Option<u32>,
    /// The exact selector currently emitted by a safe-call guard. Its copied inline frame may
    /// need to adapt a value before the selector joins the null arm.
    active_safe_call_selector: Option<u32>,
    /// A chain's receiver temporaries declared after its first guard already jumped to the shared
    /// null exit, by that exit: none of them is assigned on every path into it.
    safe_call_exit_temporaries: HashMap<Label, Vec<u32>>,
    /// Every `Variable` index → the JVM type of the slot it owns (file-wide); a `value_ty(GetValue)`
    /// fallback for a slot not registered in `slots` (queried before its declaration emits, or after
    /// its block's scope closed — e.g. an inline result temporary a discarded block reads).
    var_types: HashMap<u32, Ty>,
    /// Values stored into semantic locals, used only for pre-emission semantic non-null facts.
    value_stores: non_null_operands::ValueStores,
    /// Parameters whose emitted entry assertion establishes a semantic non-null fact.
    checked_parameters: HashSet<u32>,
    /// Every local slot this method's code uses is entered in, and left from, this frame.
    frame: frame_map::FrameMap,
    /// Where `IrExpr::CurrentContinuation` reads the continuation from, for a function whose
    /// coroutine machine this emission owns. `None` for every other function.
    continuation_slot: Option<u16>,
    /// The suspensions of the function being emitted, by call expression, each with the value
    /// class box its resumption unboxes. Empty for every function whose machine the IR pass owns.
    machine_suspensions: HashMap<u32, Option<(TypeName, Ty)>>,
    /// The deferred result value of each inline-return frame, by the frame's label.
    inline_return_frame_results: HashMap<String, (u32, Ty)>,
    /// Inline-return results whose expansion is discarded. A break to that frame leaves the
    /// value in its slot: the landing label does not read it, so it must not travel on the stack.
    discarded_inline_results: HashSet<u32>,
    /// The suspension points kotlinc's coroutine transformer takes, when it takes this function.
    transformed_suspensions: transformed_suspensions::TransformedSuspensions,
    /// The declarations that read a suspend lambda's parameters from their fields, in the
    /// `invokeSuspend` being emitted. Each is marked for the transformer.
    suspend_lambda_parameter_reads: HashSet<crate::ir::ExprId>,
    /// Function-value invocations whose consumer takes `invoke`'s erased `Object` as it is.
    erased_invocations: HashSet<crate::ir::ExprId>,
    /// Values held boxed in an erased slot whose semantic type is this built-in primitive: an
    /// inlined type-parameter value (`b.also { }`). A read unboxes the value.
    boxed_scalar_values: HashMap<u32, Ty>,
    /// Reads of such a value a reference consumer takes as the box the slot holds.
    boxed_scalar_reads: HashSet<crate::ir::ExprId>,
    inline_coercions: block_scope::InlineCoercionState,
    /// The next state's ordinal. A state belongs to an emission SITE, not to an expression: an
    /// inline function that invokes its lambda twice splices the same body twice, and each copy
    /// suspends on its own locals. Both passes emit the same sequence, so both number it alike.
    machine_next_ordinal: usize,
    /// The machine being built, on the pass that builds it.
    machine: Option<coroutine_machine::Machine>,
    /// The locals the dispatch can prove at a resume: the state on entry, before the body stored
    /// anything. A resume's only predecessor is that dispatch.
    machine_entered: bool,
    /// Where each state re-enters the body. The dispatch's restore block jumps here, so these are
    /// the enclosing method's labels; a `Resume` marker says where the splice put each one. The
    /// block there rethrows a failed resumption INSIDE the body, where a `try` around the
    /// suspension can catch it, then falls into the join.
    machine_resumes: Vec<Label>,
    /// Where the two paths out of a suspension meet, with the call's result on the stack; a `Join`
    /// marker says where the splice put each one.
    machine_joins: Vec<Label>,
    ret: Ty,
    /// Active loops: `(continue target, break target, checked common-IR target identity,
    /// active-finalizer depth on entry)`. FIR checking resolves a source label to a control target;
    /// lowering replaces its spelling with this generated identity before the backend sees it. The
    /// depth makes a `break`/`continue` run exactly the finalizers it leaves — those pushed inside
    /// the loop — and no outer one.
    loop_stack: Vec<(Label, Label, Option<String>, usize)>,
    /// The loops outside an inline lambda's node, which a `break`/`continue` leaves it for.
    non_local_jumps: non_local_jumps::NonLocalJumps,
    /// Operand-stack verification types sitting BELOW the expression currently being emitted (an
    /// arithmetic LHS held on the stack across a branchy RHS, e.g. a data-class `hashCode` accumulator
    /// `result*31 + <branchy nullable-field hash>`). Prepended to every recorded stack-map frame's stack
    /// so the pending operand is typed through the branch (matching kotlinc), avoiding the spill-to-temp
    /// krusty would otherwise need. Pushed/popped around the branchy RHS in `emit_binop`.
    /// Open source locals, in declaration order.
    open_locals: Vec<block_scope::OpenLocal>,
    /// Method-local identity source for inline frames. This never crosses the JVM boundary; it
    /// keeps debug-table ordering tied to producer provenance instead of generated local names.
    next_inline_frame_identity: u64,
    /// The current lambda body's captures that are literal lambdas it only invokes.
    inline_lambda_aliases: HashMap<u32, inline_lambda_aliases::InlineLambdaAlias>,
    /// Current block nesting depth; the function body is depth 1.
    block_depth: usize,
    /// The source line of the statement currently being emitted, when it has one. An operand that
    /// carries its own line leaves that line in effect; the instruction that CONSUMES the operand
    /// belongs to the statement, and kotlinc marks it back to this line.
    statement_line: Option<u32>,
    /// The comparison being emitted and its source line. The jump carries that line, mapped when
    /// the comparison is a copied inline expression.
    comparison_line: Option<(ExprId, u32)>,
    /// A `try` a local declaration stores: kotlinc's `visitVariable` marks the initializer's line
    /// before it loads the `try`'s saved value.
    declared_try: Option<ExprId>,
    /// Whether this method records source-local debug entries.
    record_locals: bool,
    /// The source class whose primary-constructor property initializers and `init` blocks are
    /// currently being emitted. The checked class identity keeps physical field realization from
    /// recovering the class through its rendered JVM owner name.
    constructor_initializer_class: Option<ClassId>,
    /// This physical body realizes source class initialization and therefore renders the exact
    /// anonymous-initializer block identities recorded by common IR. Ordinary functions derived
    /// from the same source body (notably a value class's `constructor-impl`) keep the provenance
    /// but do not render constructor boundary entries.
    render_initializer_boundaries: bool,
    /// kotlinc's `isInsideCondition`: a `when` branch condition is being emitted, so an inlined
    /// call in it marks its own line again after the inlined code.
    inside_condition: bool,
    /// The operand binding of a same-module inline call being emitted: kotlinc stores the operand
    /// into its parameter slot without marking a line.
    copied_operand_binding: Option<ExprId>,
    /// The `&&`/`||` operand whose line is marked again before the jump that consumes it.
    short_circuit_operand_line: Option<ExprId>,
    /// The enclosing unsigned operation's decision to move its eager source boundary.
    unsigned_receiver_line_moves: Option<bool>,
    /// A plain local operand of an unsigned bitwise member is being loaded, so it records no line.
    suppress_unsigned_local_line: bool,
    /// An assignment of an unsigned bitwise value keeps its line on the receiver load. The store
    /// writes that line again after `constructor-impl`.
    unsigned_assignment_line: bool,
    /// Slot 0 remains the verifier's special uninitialized receiver until the constructor delegates.
    this_uninitialized: bool,
    /// Independent realization strategies for plain lambdas and SAM conversions.
    lambda_modes: LambdaModes,
    /// `-Xstring-concat=inline`: never build a concatenation with `invokedynamic`.
    inline_string_concat: bool,
    /// The scope numbering of the method being emitted, under `-Xuse-inline-scopes-numbers`.
    inline_scopes: Option<crate::jvm::inliner::InlineScopes>,
    /// Placed lambdas whose bodies are being emitted, innermost last, with their callers' numbering.
    inline_scope_units: Vec<(u64, Option<crate::jvm::inliner::InlineScopes>)>,
    /// Active `finally` bodies, outermost first. A source-level control transfer executes these
    /// before leaving its protected region; the stack carries exact IR identities, not syntax.
    return_finalizers: Vec<u32>,
    /// Protected-region accumulators for the active `try`s, outermost first. A copy of a finalizer
    /// must not lie inside the ranges of its own `try` or of any `try` nested in it, or an exception
    /// raised while the finalizer runs re-enters a handler the transfer has already left.
    protected_regions: Vec<ProtectedRegion>,
    /// A statement at the lexical tail of a loop body may branch directly to the loop's next
    /// iteration. This is an emitter control-flow target, not a semantic `continue` manufactured in
    /// common IR. Blocks pass it only to their terminal statement.
    terminal_statement_target: Option<Label>,
    /// The method an inline call's anonymous objects are regenerated for, when it is one.
    regeneration_site: Option<bytecode_inline_call::RegenerationSite<'a>>,
    /// The named object whose instance value 0 reads in a `@JvmStatic` member's static method.
    singleton_receiver: Option<TypeName>,
}

impl<'a> Emitter<'a> {
    fn new(
        ir: &'a IrFile,
        cw: &'a mut ClassWriter,
        env: &EmitEnv<'a>,
        static_owner: Option<StaticOwner>,
        owner: &str,
        facade: &str,
        ret: Ty,
        roots: impl IntoIterator<Item = u32>,
    ) -> Self {
        let roots: Vec<u32> = roots.into_iter().collect();
        Self {
            ir,
            cw,
            override_results: env.override_results,
            bodies: env.bodies,
            run: env.run,
            jvm_default: env.jvm_default,
            metadata_stamp: env.metadata_stamp,
            property_realizations: env.property_realizations,
            default_call_operands: env.default_call_operands,
            sam_wrapper_realizations: env.sam_wrapper_realizations,
            local_delegate_access: env.local_delegate_access,
            suspended_result_returns: env.suspended_result_returns,
            self_companion: singleton_instance_load::self_companion(ir, static_owner),
            intrinsic_probe_continuations: env.intrinsic_probe_continuations,
            emit_time_machines: env.emit_time_machines,
            static_owner,
            function: None,
            type_parameter_function: None,
            export_private_calls: false,
            method_exports_private_access: false,
            finally_markers: false,
            finally_marker_depth: 0,
            classifiers: env.signature_symbols,
            dispatch_classifiers: env.dispatch_classifiers.clone(),
            owner: owner.to_string(),
            facade: facade.to_string(),
            multifile_facade: env.multifile_facade.map(str::to_string),
            slots: HashMap::default(),
            temporaries: backend_temporaries::BackendTemporaries::default(),
            unassigned_values: HashSet::default(),
            label_unassigned_values: HashMap::default(),
            label_stack_resident_values: HashMap::default(),
            safe_call_null_exits: HashMap::default(),
            stack_resident_value: None,
            active_safe_call_selector: None,
            safe_call_exit_temporaries: HashMap::default(),
            var_types: collect_body_var_types(ir, roots.iter().copied()),
            value_stores: non_null_operands::ValueStores::collect(ir, &roots),
            checked_parameters: HashSet::default(),
            frame: frame_map::FrameMap::default(),
            continuation_slot: None,
            machine_suspensions: HashMap::default(),
            inline_return_frame_results: HashMap::default(),
            discarded_inline_results: HashSet::default(),
            transformed_suspensions: Default::default(),
            suspend_lambda_parameter_reads: HashSet::default(),
            erased_invocations: HashSet::default(),
            boxed_scalar_values: inline_body_emission::collect_boxed_scalar_values(
                ir,
                roots.iter().copied(),
            ),
            boxed_scalar_reads: HashSet::default(),
            inline_coercions: block_scope::InlineCoercionState::default(),
            machine_next_ordinal: 0,
            machine_entered: false,
            machine_resumes: Vec::new(),
            machine_joins: Vec::new(),
            machine: None,
            ret,
            loop_stack: Vec::new(),
            non_local_jumps: Default::default(),
            open_locals: Vec::new(),
            next_inline_frame_identity: 0,
            inline_lambda_aliases: HashMap::default(),
            block_depth: 0,
            statement_line: None,
            comparison_line: None,
            declared_try: None,
            record_locals: false,
            constructor_initializer_class: None,
            render_initializer_boundaries: false,
            inside_condition: false,
            copied_operand_binding: None,
            short_circuit_operand_line: None,
            unsigned_receiver_line_moves: None,
            suppress_unsigned_local_line: false,
            unsigned_assignment_line: false,
            this_uninitialized: false,
            lambda_modes: env.lambda_modes,
            inline_string_concat: env.inline_string_concat,
            inline_scopes: env
                .inline_scopes_numbers
                .then(crate::jvm::inliner::InlineScopes::default),
            inline_scope_units: Vec::new(),
            return_finalizers: Vec::new(),
            protected_regions: Vec::new(),
            terminal_statement_target: None,
            regeneration_site: None,
            singleton_receiver: None,
        }
    }

    /// Whether code emitted by this source owner reaches a private member of another source class.
    /// Generated holders have no source owner and therefore always use the declaring class's bridge.
    fn reaches_through_bridge(&self, owner: TypeName, function: u32) -> bool {
        self.static_owner != Some(StaticOwner::Class(owner))
            && self
                .run
                .private_member_access_bridges
                .borrow()
                .contains(&function)
    }

    /// Record a spliced body's line marks, mapping the dependency's lines into output lines the
    /// caller's source map gives meaning to.
    ///
    /// The dependency's own numbers cannot be written down as they are: two files would then claim
    /// the same lines. The map reserves a fresh output range for this region and says where it came
    /// from, which is what a debugger reads back. A spliced lambda's marks are the caller's own
    /// source and pass through untouched.
    fn record_spliced_lines(
        &mut self,
        lines: &[(u16, u16, bool)],
        body: &crate::jvm::classreader::MethodCode,
        inline_only: bool,
        shift: usize,
        code: &mut CodeBuilder,
    ) {
        if lines.is_empty() {
            return;
        }
        let call_line = code.current_line().unwrap_or(1);
        // The highest line the class's own code can claim. `source_line_count` already counts the
        // position past the last line, where a synthesized mark (a closing brace's implicit return)
        // is recorded.
        let claimable = u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1);
        for &(at, line, inlined) in lines {
            let Ok(at) = u16::try_from(at as usize + shift) else {
                continue;
            };
            if !inlined {
                code.add_line_mark_at(at, line);
                continue;
            }
            if let Some(output) =
                self.map_inlined_line(body, inline_only, line, call_line, claimable)
            {
                code.add_line_mark_at(at, output);
            }
        }
    }

    /// The machine's entry: take or make the continuation, read what a resume left in it, and
    /// dispatch to the state it stopped in.
    ///
    /// Returns the label the body starts at, one label per suspension for the dispatch to re-enter,
    /// and the label of the state that cannot happen.
    fn emit_machine_prologue(
        &mut self,
        completion: u16,
        code: &mut CodeBuilder,
    ) -> Option<(Label, Vec<Label>, Label)> {
        let machine = self.machine.clone()?;
        let internal = machine.internal.clone();
        let class = self.cw.class_ref(&internal);
        let label_field = self.cw.fieldref(&internal, "label", "I");
        let result_field = self.cw.fieldref(&internal, "result", "Ljava/lang/Object;");
        let fresh = code.new_label();
        let have = code.new_label();
        let body = code.new_label();
        let default = code.new_label();
        let resumes: Vec<Label> = (0..machine.plan.suspensions.len())
            .map(|_| code.new_label())
            .collect();

        // A continuation of our own type whose label carries the resume bit is this machine being
        // re-entered; anything else is a fresh call.
        code.aload(completion);
        code.instance_of(class);
        code.ifeq(fresh);
        code.aload(completion);
        code.checkcast(class);
        code.astore(machine.slots.continuation);
        code.aload(machine.slots.continuation);
        code.getfield(label_field, 1);
        code.push_int(i32::MIN, self.cw);
        code.iand();
        code.ifeq(fresh);
        code.aload(machine.slots.continuation);
        code.dup();
        code.getfield(label_field, 1);
        code.push_int(i32::MIN, self.cw);
        code.isub();
        code.putfield(label_field, 1);
        code.goto(have);

        code.bind(fresh);
        code.new_obj(class);
        code.dup();
        let constructor_descriptor = match &machine.receiver {
            Some(owner) => format!("(L{owner};Lkotlin/coroutines/Continuation;)V"),
            None => "(Lkotlin/coroutines/Continuation;)V".to_string(),
        };
        let mut words = 2;
        if machine.receiver.is_some() {
            code.aload(0);
            words += 1;
        }
        code.aload(completion);
        let constructor = self
            .cw
            .methodref(&internal, "<init>", &constructor_descriptor);
        code.invokespecial(constructor, words, 0);
        code.astore(machine.slots.continuation);

        code.bind(have);
        code.aload(machine.slots.continuation);
        code.getfield(result_field, 1);
        code.astore(machine.slots.result);
        let suspended = self.cw.methodref(
            "kotlin/coroutines/intrinsics/IntrinsicsKt",
            "getCOROUTINE_SUSPENDED",
            "()Ljava/lang/Object;",
        );
        code.invokestatic(suspended, 0, 1);
        code.astore(machine.slots.suspended);
        code.aload(machine.slots.continuation);
        code.getfield(label_field, 1);
        let mut targets = Vec::with_capacity(resumes.len() + 1);
        targets.push(body);
        targets.extend(resumes.iter().copied());
        code.tableswitch(0, targets.len() as i32 - 1, default, &targets);

        self.machine_entered = true;
        let throw_on_failure =
            self.cw
                .methodref("kotlin/ResultKt", "throwOnFailure", "(Ljava/lang/Object;)V");

        // A state's restores are emitted AFTER the body, in `emit_machine_states`: they must not
        // allocate or touch a slot before the body is laid out, because the plan being realized
        // here was read from a first emission that had none of this code in it.
        let joins: Vec<Label> = (0..machine.plan.suspensions.len())
            .map(|_| code.new_label())
            .collect();
        self.machine_joins = joins;
        let resume_points: Vec<Label> = (0..machine.plan.suspensions.len())
            .map(|_| code.new_label())
            .collect();
        self.machine_resumes = resume_points;

        code.bind(body);
        code.aload(machine.slots.result);
        code.invokestatic(throw_on_failure, 1, 0);
        Some((body, resumes, default))
    }

    /// The dispatch's states, emitted AFTER the body they re-enter.
    ///
    /// Each restores its own spills and jumps to the resume point inside the body, so nothing the
    /// machine adds sits between the `try` ranges the body declares and the locals they describe: a
    /// handler's frame claims the body's locals, and an edge from a restore that had not run yet
    /// cannot produce them. Placing the block after the body also keeps the slot allocation
    /// identical to the first emission, which is where the plan was read.
    ///
    /// A failed resumption is NOT rethrown here: the block is outside every `try` range the body
    /// declares, so a `catch` around the suspension would never see the callee's exception. The
    /// resume point inside the body does that (see [`Self::emit_machine_check`]).
    fn emit_machine_states(&mut self, states: &[Label], code: &mut CodeBuilder) {
        let Some(machine) = self.machine.clone() else {
            return;
        };
        for (ordinal, suspension) in machine.plan.suspensions.iter().enumerate() {
            let (Some(&state), Some(&resume)) =
                (states.get(ordinal), self.machine_resumes.get(ordinal))
            else {
                continue;
            };
            code.bind_external_target(state);
            code.set_stack_height(0);
            for (slot, ty, field, descriptor) in coroutine_machine::suspension_fields(suspension) {
                code.aload(machine.slots.continuation);
                let reference = self.cw.fieldref(&machine.internal, &field, descriptor);
                let jvm = ir_ty_to_jvm(&ty);
                code.getfield(reference, slot_words(jvm) as i32);
                // Only a reference spill widens on the way in — it is stored in an `Object` field,
                // so the read has to be narrowed back. A primitive field already has the slot's own
                // type, and casting an `int` is not something the verifier will accept.
                if descriptor == "Ljava/lang/Object;" {
                    if let Some(internal) = checkcast_internal(jvm) {
                        let class = self.cw.class_ref(&internal);
                        code.checkcast(class);
                    }
                }
                store(jvm, slot, code);
            }
            // A slot the verifier held as `null` at the suspension is a constant, not a field.
            for slot in coroutine_machine::constant_null_slots(suspension) {
                code.aconst_null();
                code.astore(slot);
            }
            code.goto(resume);
        }
    }

    /// The state a resume cannot legally be in: re-entering a machine that never suspended.
    fn emit_machine_default(&mut self, default: Label, code: &mut CodeBuilder) {
        code.bind(default);
        let class = self.cw.class_ref("java/lang/IllegalStateException");
        code.new_obj(class);
        code.dup();
        code.push_string("call to 'resume' before 'invoke' with coroutine", self.cw);
        let constructor = self.cw.methodref(
            "java/lang/IllegalStateException",
            "<init>",
            "(Ljava/lang/String;)V",
        );
        code.invokespecial(constructor, 2, 0);
        code.athrow();
    }

    /// Slot-indexed caller locals for `0..upto` (long/double take two slots; `Top` fills the gaps).
    ///
    /// Semantic locals and backend temporaries share this physical layout. Coroutine discovery uses
    /// it as the entry state for final-bytecode dataflow; ordinary method frames are computed later.
    fn verif_slots_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let semantic = self.assigned_semantic_slots();
        let temporaries = self.temporaries.live();
        let mut raw = backend_temporaries::frame_slots(upto, &semantic, &temporaries, &mut |ty| {
            self.verif_single(ty)
        });
        if self.this_uninitialized && !raw.is_empty() {
            raw[0] = VerifType::UninitializedThis;
        }
        raw
    }

    fn function_ref_class_and_captures(&self, expr: u32) -> Option<(crate::ir::ClassId, Vec<u32>)> {
        match self.ir.expr(expr) {
            IrExpr::New { internal, args, .. }
                if self
                    .ir
                    .class_id_by_name(*internal)
                    .is_some_and(|c| self.ir.classes[c as usize].func_ref.is_some()) =>
            {
                Some((self.ir.class_id_by_name(*internal).unwrap(), args.clone()))
            }
            IrExpr::StaticInstance { ty, .. }
                if self.ir.classes[*ty as usize].func_ref.is_some() =>
            {
                Some((*ty, Vec::new()))
            }
            _ => None,
        }
    }

    fn property_ref_class_and_captures(&self, expr: u32) -> Option<(crate::ir::ClassId, Vec<u32>)> {
        match self.ir.expr(expr) {
            IrExpr::New { internal, args, .. }
                if self
                    .ir
                    .class_id_by_name(*internal)
                    .is_some_and(|c| self.ir.classes[c as usize].prop_ref.is_some()) =>
            {
                Some((self.ir.class_id_by_name(*internal).unwrap(), args.clone()))
            }
            IrExpr::StaticInstance { ty, .. }
                if self.ir.classes[*ty as usize].prop_ref.is_some() =>
            {
                Some((*ty, Vec::new()))
            }
            _ => None,
        }
    }

    fn emit(&mut self, e: u32, code: &mut CodeBuilder) {
        self.with_copied_private_access(e, |emitter| emitter.emit_node(e, code));
    }

    fn emit_node(&mut self, e: u32, code: &mut CodeBuilder) {
        match self.ir.expr(e).clone() {
            IrExpr::Block { stmts, value } => self.emit_statement_block(e, stmts, value, code),
            IrExpr::Return(value) => self.emit_return_node(e, value, code),
            IrExpr::Variable {
                index,
                ty,
                init,
                named,
            } => self.emit_local_variable(e, index, ty, init, named, code),
            IrExpr::InlineFrameMarker => self.emit_inline_frame_marker(e, code),
            IrExpr::SetValue { var, value } => {
                let Some(&(_, jt)) = self.slots.get(&var) else {
                    panic!(
                        "malformed IR: assignment target {var} has no allocated JVM slot while emitting {}; known slots: {:?}",
                        self.owner,
                        self.slots.keys().collect::<Vec<_>>()
                    );
                };
                let deferred = self.inline_return_frame_result_type(var).is_some();
                match local_updates::iinc_delta(self.ir, e, var, value, jt) {
                    Some(delta) if !deferred => code.iinc(self.slots[&var].0, delta),
                    _ => {
                        // `visitSetValue` marks the assignment before the value, so a literal
                        // argument does not take the line the way a declaration initializer's does.
                        // The store writes the line again once `constructor-impl` has forgotten it.
                        let unsigned = self.completes_unsigned_bitwise_value(value);
                        let saved_assignment = self.unsigned_assignment_line;
                        if unsigned {
                            self.unsigned_assignment_line = true;
                            // The value's own mark follows at this same pc. When its right operand
                            // is itself unsigned, kotlinc keeps both.
                            if self.unsigned_bitwise_rhs_is_unsigned(value) {
                                debug_lines::mark_statement_retained(self.ir, e, code);
                            }
                        }
                        // An erased `FunctionN.invoke` stored into an `Object` slot stays `Object`.
                        // The consumer after the store narrows it; casting at the store would put
                        // the checkcast inside the protected range.
                        // `FunctionN.invoke` returns `Object` even when the function type says
                        // `Unit`. A `Unit` consumer pops that object; an `Object` slot stores it.
                        let keep_erased_invoke = jvm_is_erased_top(jt)
                            && matches!(
                                self.ir.expr(value),
                                IrExpr::InvokeFunction { ret, .. } if *ret != Ty::Nothing
                            );
                        if keep_erased_invoke {
                            self.erased_invocations.insert(value);
                        }
                        self.emit_value(value, code);
                        if keep_erased_invoke {
                            self.erased_invocations.remove(&value);
                        }
                        self.unsigned_assignment_line = saved_assignment;
                        // Coerced to the slot's type as the initializer is: a value of another
                        // class is cast to the declared one, which is what a join of the two
                        // stores reads back. The erased invoke already left `Object`.
                        if !keep_erased_invoke {
                            self.adapt_physical_operand_for(value, self.value_ty(value), jt, code);
                        }
                        // An inlined `return` stores on the return's line, after the value.
                        self.mark_expression_start(e, code);
                        let slot = self.activate_inline_return_frame_result(var, jt);
                        if unsigned {
                            code.forget_line();
                            debug_lines::mark_statement(self.ir, e, code);
                        }
                        store(jt, slot, code);
                    }
                }
                self.unassigned_values.remove(&var);
            }
            IrExpr::SetFrameResult { frame, value } => {
                self.emit_set_frame_result(&frame, value, code)
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => self.emit_set_field(e, receiver, class, index, value, code),
            IrExpr::SetStatic { index, value } => self.emit_set_static(e, index, value, code),
            IrExpr::Checked(_) => {
                unreachable!("checked operation passed jvm_can_emit without a JVM realization")
            }
            IrExpr::While { .. } => self.emit_while(e, code),
            IrExpr::Break { label } => self.emit_loop_transfer(e, &label, true, code),
            IrExpr::Continue { label } => self.emit_loop_transfer(e, &label, false, code),
            other => {
                self.emit_discarding_node(e, &other, code);
            }
        }
    }

    fn emit_value(&mut self, e: u32, code: &mut CodeBuilder) {
        self.with_copied_private_access(e, |emitter| emitter.emit_value_expression(e, code));
    }

    fn emit_value_expression(&mut self, e: u32, code: &mut CodeBuilder) {
        debug_lines::begin_expression(self.ir, e, code);
        self.mark_value_expression_start(e, code);
        // Mark a machine-owned suspension at its post-splice location.
        let suspension = self.machine_before(e, code);
        self.open_transformed_suspension(e, code);
        let node = self.ir.expr(e).clone();
        self.emit_value_node(e, &node, code);
        self.mark_after_inlined_call(e, code);
        self.probe_intrinsic_suspension(e, code);
        self.machine_after(suspension, code);
        self.close_declared_suspension(e, code);
        if !code.is_dead() {
            if let Some(line) = self.ir.evaluated_result_end_line(e) {
                self.mark_expression_line(e, line, code);
                code.nop();
            }
        }
    }

    /// Realize `IrExpr::PropertyRead` — the one place that decides what reading a Kotlin property
    /// COMPILES to. The owner's class file names the accessor or field (via `@Metadata`'s
    /// `JvmPropertySignature`, so a `@JvmName` or value-class-mangled spelling is honoured, never guessed)
    /// and says whether it takes a receiver. A class this compilation is still emitting has no class file
    /// to ask, so it falls back to the JVM convention kotlinc itself follows: `get<Name>()`.
    fn emit_property_read(&mut self, operation: PropertyOperation<'_>, code: &mut CodeBuilder) {
        use crate::jvm::inline::PropertyAccess;
        let receiver_ty = operation.receiver.map(|receiver| self.value_ty(receiver));
        let selected = self
            .ir
            .property_selected_accessors
            .get(&operation.expression);
        let stamped = self
            .ir
            .property_accessor_jvm_realizations
            .get(&operation.expression);
        // Resolution records a declaration type independently of where the owner was found. A JVM
        // realization stamp is more specific (notably for a value-class-mangled accessor); otherwise
        // the semantic declaration type supplies the descriptor and the node's logical type remains
        // the value consumed by the surrounding expression.
        let physical = stamped
            .map(|(_, physical)| physical)
            .or_else(|| selected.map(|(_, physical)| physical))
            .or_else(|| {
                self.ir
                    .property_declaration_types
                    .get(&operation.expression)
            });
        let declaration_ty = *physical.unwrap_or(operation.ty);
        if let Some(realization) = self.property_realizations.get(operation.expression) {
            let access = match realization {
                crate::jvm::property_realizations::PropertyRealization::Physical(access) => {
                    Some(access.clone())
                }
                crate::jvm::property_realizations::PropertyRealization::Local(target) => self
                    .local_property_read_access(
                        *target,
                        selected.map(|(name, _)| name.as_str()),
                        operation.interface,
                    ),
            };
            if let Some(mut access) = access {
                self.retarget_explicit_backing_read(&operation, &mut access);
                return self.emit_realized_property_read(&operation, access, code);
            }
        }
        crate::trace_compiler!(
            "emit",
            "property read owner={} name={} receiver={receiver_ty:?} declaration={:?}",
            operation.owner,
            operation.name,
            declaration_ty,
        );
        if let Some(access) = self
            .ir
            .property_external_accessors
            .get(&operation.expression)
            .and_then(|accessor| self.bodies.external_property_access(*accessor))
        {
            return self.emit_realized_property_read(&operation, access, code);
        }
        // A source declaration from this compilation supplies the invocation shape; a checker-selected
        // spelling refines only its otherwise-conventional accessor name.
        if let Some(mut access) = self.declared_property_read_access(
            operation.owner,
            operation.name,
            None,
            selected.map(|(name, _)| name.as_str()),
            operation.interface,
        ) {
            self.retarget_explicit_backing_read(&operation, &mut access);
            return self.emit_realized_property_read(&operation, access, code);
        }
        // A loaded classfile supplies the authoritative JVM realization, including static value-class
        // accessors. The selected spelling intentionally carries no duplicate invocation shape.
        if let Some(access) = self
            .bodies
            .property_read_access(operation.owner, operation.name)
        {
            return self.emit_realized_property_read(&operation, access, code);
        }
        let access = PropertyAccess::Accessor {
            owner: operation.owner,
            // A sibling source class has no classfile in `bodies`, so this is the only realization
            // that cannot read the exact JVM accessor spelling from a declaration. Exact selected
            // spellings returned above; an unstamped ordinary property keeps Kotlin's convention.
            name: stamped
                .map(|(name, _)| name.clone())
                .or_else(|| selected.map(|(name, _)| name.clone()))
                .unwrap_or_else(|| crate::names::property_getter_name(operation.name)),
            // Keep the logical type on the node; the call boundary uses the JVM realization when
            // present, otherwise the declaration type. That prevents calling generic `getA(): Object`
            // as `getA(): A`, or mangled `getId-…(): String` as `getId-…(): Id`.
            descriptor: method_descriptor(
                &[],
                ir_ty_to_jvm(&crate::jvm::annotation_kclass::annotation_member_read_type(
                    self.classifiers,
                    operation.owner,
                    stored_value_ty(*physical.unwrap_or(operation.ty)),
                )),
            ),
            is_static: false,
            // Resolution carries source-module shape because a sibling class is not in `bodies`.
            is_interface: operation.interface
                || self.bodies.owner_is_interface_name(operation.owner),
            static_receiver: None,
        };
        self.emit_realized_property_read(&operation, access, code)
    }

    /// Realize `IrExpr::PropertyWrite` — the write analogue of [`Self::emit_property_read`], and the same
    /// sources in the same order: a class this compilation declares, then the owner's class file, then the
    /// JVM naming convention (`set<Name>`).
    fn emit_property_write(
        &mut self,
        operation: PropertyOperation<'_>,
        value: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        use crate::jvm::inline::PropertyAccess;
        let stamped = self
            .ir
            .property_accessor_jvm_realizations
            .get(&operation.expression);
        let physical = stamped.map(|(_, physical)| physical).or_else(|| {
            self.ir
                .property_declaration_types
                .get(&operation.expression)
        });
        let access = self
            .property_realizations
            .get(operation.expression)
            .map(|realization| match realization {
                crate::jvm::property_realizations::PropertyRealization::Physical(access) => {
                    access.clone()
                }
                crate::jvm::property_realizations::PropertyRealization::Local(target) => self
                    .local_property_write_access(*target)
                    .expect("a realized local member property write has exact JVM access"),
            })
            .or_else(|| {
                self.ir
                    .property_external_accessors
                    .get(&operation.expression)
                    .and_then(|accessor| self.bodies.external_property_access(*accessor))
            })
            .or_else(|| self.declared_property_write_access(operation.owner, operation.name))
            .or_else(|| {
                self.bodies
                    .property_write_access(operation.owner, operation.name)
            })
            .unwrap_or_else(|| PropertyAccess::Accessor {
                owner: operation.owner,
                name: stamped
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| crate::names::property_setter_name(operation.name)),
                descriptor: method_descriptor(
                    &[ir_ty_to_jvm(physical.unwrap_or(operation.ty))],
                    Ty::Unit,
                ),
                is_static: false,
                is_interface: operation.interface
                    || self.bodies.owner_is_interface_name(operation.owner),
                static_receiver: None,
            });
        let access =
            access_bridges::protected_property_access(self.run, operation.expression, access);
        let Some(access) = self.checked_dispatched_accessor(operation.expression, access) else {
            return;
        };
        let access_owner = match &access {
            PropertyAccess::Field { owner, .. }
            | PropertyAccess::Accessor { owner, .. }
            | PropertyAccess::AccessBridge { owner, .. } => *owner,
        };
        let takes_receiver = accessor_takes_receiver(&access);
        let writes_static_field = matches!(
            &access,
            PropertyAccess::Field {
                is_static: true,
                ..
            }
        );
        // A value that cannot carry the operand stack (a handler, suspension, or loop transfer)
        // spills BOTH operands in source order (receiver, then value), then reloads them. Spilling
        // only the value would reverse observable side effects. An ordinary branchy value keeps the
        // receiver on the stack, as kotlinc does.
        let spilled = if takes_receiver && self.spills_operand_prefix(value) {
            Some(
                self.spill_to_temps(
                    &[
                        operation
                            .receiver
                            .expect("instance property writes have a dispatch receiver"),
                        value,
                    ],
                    code,
                ),
            )
        } else {
            None
        };
        if let Some(temps) = &spilled {
            let (slot, receiver_ty, _) = temps[0];
            load(receiver_ty, slot, code);
            self.narrow_on_stack(
                receiver_ty,
                accessor_receiver_ty(&access, access_owner),
                code,
            );
        } else if let Some(receiver) = operation.receiver {
            let receiver_ty = accessor_receiver_ty(&access, access_owner);
            if writes_static_field {
                if self.static_property_write_evaluates_receiver(operation.expression) {
                    // Unlike a read through a static `@JvmField` qualifier, a write evaluates its
                    // explicit receiver even when companion lookup retargeted storage to the outer.
                    self.emit_value(receiver, code);
                    code.pop();
                }
            } else {
                self.emit_property_receiver(
                    receiver,
                    operation.owner,
                    access_owner,
                    takes_receiver,
                    &receiver_ty,
                    property_access::PropertyReceiverUse::Write,
                    code,
                );
            }
        } else if takes_receiver {
            self.run.set_emit_error(format!(
                "receiver-less property realization requires an instance receiver: {}",
                access_owner.render()
            ));
            return;
        }
        // The assigned value is bridged to what the realization stores, the mirror of the read's bridge.
        let target = property_access::property_store_slot(&access, *operation.ty);
        if let Some(temps) = &spilled {
            let (slot, value_ty, _) = temps[1];
            load(value_ty, slot, code);
            self.release_operand_spills(temps);
        } else {
            self.emit_value(value, code);
        }
        let source = self.value_ty(value);
        if source.is_jvm_scalar() && !target.is_jvm_scalar() && target.is_reference() {
            // The property operation retains the substituted Kotlin type even when the selected
            // field/accessor descriptor is erased. Use it to choose the wrapper before the carrier
            // loses unsigned identity (`UInt` must enter an `Object` slot as `kotlin/UInt`).
            box_prim_free(
                self.cw,
                code,
                semantic_scalar_adapter(*operation.ty, source),
            );
        } else if !source.is_jvm_scalar() && target.is_jvm_scalar() {
            unbox_prim_from(
                self.cw,
                code,
                source,
                semantic_scalar_adapter(*operation.ty, target),
            );
        } else {
            self.coerce_reference_on_stack(source, target, code);
        }
        match access {
            PropertyAccess::Field {
                owner,
                name,
                descriptor,
                is_static,
            } => self.emit_realized_field_store(
                operation.expression,
                owner,
                &name,
                &descriptor,
                is_static,
                code,
            ),
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static,
                is_interface,
                static_receiver: _,
            } => {
                let owner = owner.render();
                let words = crate::jvm::names::parse_method_descriptor(&descriptor)
                    .map(|(params, _)| {
                        params
                            .iter()
                            .map(|parameter| {
                                crate::jvm::physical_type::field_slot(parameter).words()
                            })
                            .sum()
                    })
                    .unwrap_or_else(|| slot_words(target) as i32);
                // A property assignment is `Unit`. A Java bean setter may return the receiver or
                // any other value; that result is discarded, the same way a statement-position
                // call drops its value. Kotlin setters are `void`, so they leave nothing to pop.
                let ret_words = descriptor_ret_words(&descriptor);
                let m = if is_interface {
                    self.cw.interface_methodref(&owner, &name, &descriptor)
                } else {
                    self.cw.methodref(&owner, &name, &descriptor)
                };
                // The write through an ACCESSOR is a dispatch, so the assignment's own line returns
                // here, after the value expression has marked its.
                self.mark_dispatch_line(operation.expression, code);
                if is_static {
                    code.invokestatic(m, words, ret_words);
                } else if is_interface {
                    code.invokeinterface(m, words, ret_words);
                } else {
                    code.invokevirtual(m, words, ret_words);
                }
                if ret_words == 2 {
                    code.pop2();
                } else if ret_words == 1 {
                    code.pop();
                }
            }
            PropertyAccess::AccessBridge {
                owner,
                name,
                descriptor,
                ..
            } => {
                // The bridge's arguments are already on the stack.
                let (parameters, _) = crate::jvm::names::parse_method_descriptor(&descriptor)
                    .expect("a planned property access bridge has a valid JVM descriptor");
                let words = parameters
                    .iter()
                    .map(|parameter| crate::jvm::physical_type::field_slot(parameter).words())
                    .sum();
                let owner = owner.render();
                let m = self.cw.methodref(&owner, &name, &descriptor);
                self.mark_dispatch_line(operation.expression, code);
                code.invokestatic(m, words, 0);
            }
        }
    }

    /// Whether emitting `e` introduces non-linear JVM control flow anywhere in its subtree. Operand
    /// sequences use this physical fact to keep an earlier value off the stack while nested branches,
    /// handlers, or inline splices execute. Stack-map frames themselves are computed from the final body.
    fn emits_control_flow(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. } => true,
            // The multi-part `StringConcat` itself spills branchy parts internally, so as a whole it
            // leaves only its `String` result — but a parent operand sequence still must treat it as
            // branchy if any part is (it builds the StringBuilder mid-stack otherwise).
            IrExpr::StringConcat(parts) => parts.iter().any(|&p| self.emits_control_flow(p)),
            IrExpr::Equality { .. } | IrExpr::PrimitiveBinOp { .. } => {
                self.comparison_emits_control_flow(e)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => {
                matches!(
                    &**callee,
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::Assert { .. },
                        ..
                    }
                ) || self.splice_branches(callee, args)
                    || dispatch_receiver.is_some_and(|r| self.emits_control_flow(r))
                    || args.iter().any(|&a| self.emits_control_flow(a))
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                self.emits_control_flow(*receiver)
                    || args
                        .iter()
                        .any(|a| a.is_some_and(|x| self.emits_control_flow(x)))
            }
            IrExpr::InvokeFunction { func, args, .. } => {
                self.emits_control_flow(*func)
                    || args.iter().any(|&arg| self.emits_control_flow(arg))
            }
            IrExpr::New { args, .. } => {
                self.nullable_sam_wrapper_emits_control_flow(e)
                    || args.iter().any(|&a| self.emits_control_flow(a))
            }
            // A `lateinit` FIELD read carries its own uninitialized guard (`dup; ifnonnull L; ldc name;
            // invokestatic throwUninitializedPropertyAccessException; L:`), whose join requires the
            // surrounding operand baseline to agree — so an earlier operand is spilled first.
            IrExpr::EnclosingInstance { receiver, .. } => self.emits_control_flow(*receiver),
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                self.emits_control_flow(*receiver)
                    || self.ir.classes[*class as usize].fields[*index as usize].is_lateinit()
            }
            IrExpr::PropertyRead {
                receiver,
                owner,
                name,
                ..
            } => {
                receiver.is_some_and(|receiver| self.emits_control_flow(receiver))
                    || self.lateinit_read_guards_inline(*owner, name)
            }
            IrExpr::SetField {
                receiver, value, ..
            } => self.emits_control_flow(*receiver) || self.emits_control_flow(*value),
            IrExpr::PropertyWrite {
                receiver, value, ..
            } => {
                receiver.is_some_and(|receiver| self.emits_control_flow(receiver))
                    || self.emits_control_flow(*value)
            }
            IrExpr::SetValue { value, .. }
            | IrExpr::SetStatic { value, .. }
            | IrExpr::SetFrameResult { value, .. } => self.emits_control_flow(*value),
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => {
                self.emits_control_flow(*arg)
            }
            IrExpr::BottomValue { producer, .. } => self.emits_control_flow(*producer),
            IrExpr::NotNullAssert { operand, .. } => self.emits_control_flow(*operand),
            // A `lateinit` read emits an `ifnonnull` merge frame, so a parent must spill other operands
            // first (else the frame at the join would omit them).
            IrExpr::LateinitCheck { .. } => true,
            IrExpr::RefGet { holder, .. } => self.emits_control_flow(*holder),
            IrExpr::RefSet { holder, value, .. } => {
                self.emits_control_flow(*holder) || self.emits_control_flow(*value)
            }
            IrExpr::RefNew { init, .. } => init.is_some_and(|init| self.emits_control_flow(init)),
            IrExpr::Throw { operand } => self.emits_control_flow(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.emits_control_flow(a)),
            IrExpr::NewArray { size, .. } => self.emits_control_flow(*size),
            IrExpr::Return(v) => v.is_some_and(|x| self.emits_control_flow(x)),
            IrExpr::Variable { init, .. } => init.is_some_and(|i| self.emits_control_flow(i)),
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| self.emits_control_flow(s))
                    || value.is_some_and(|v| self.emits_control_flow(v))
            }
            other => self.static_expr_branches(e, other),
        }
    }

    fn emit_primitive_inc_dec_virtual(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        if !args.is_empty() || !matches!(name, "inc" | "dec") {
            return false;
        }
        let Some(owner_prim) = wrapper_owner_primitive(owner) else {
            return false;
        };
        let recv_ty = self.value_ty(recv);
        let source_prim = if recv_ty.is_jvm_scalar() {
            recv_ty
        } else {
            owner_prim
        };
        let ret = ty_from_descriptor_ret(descriptor);
        self.emit_value(recv, code);
        if !recv_ty.is_jvm_scalar() {
            unbox_prim_from(self.cw, code, recv_ty, owner_prim);
        }
        match owner_prim {
            Ty::Long => {
                code.push_long(1, self.cw);
                if name == "inc" {
                    code.ladd();
                } else {
                    code.lsub();
                }
            }
            Ty::Float => {
                code.push_float(1.0, self.cw);
                if name == "inc" {
                    code.fadd();
                } else {
                    code.fsub();
                }
            }
            Ty::Double => {
                code.push_double(1.0, self.cw);
                if name == "inc" {
                    code.dadd();
                } else {
                    code.dsub();
                }
            }
            _ => {
                code.push_int(1, self.cw);
                if name == "inc" {
                    code.iadd();
                } else {
                    code.isub();
                }
            }
        }
        let arithmetic_ty = owner_prim.int_arithmetic_repr();
        emit_num_conv(arithmetic_ty, source_prim, code);
        emit_num_conv(source_prim, ret, code);
        true
    }

    /// Realize the already-selected Kotlin assertion operation. The runtime guard is emitted before
    /// either child, so disabled assertions evaluate neither the condition nor the lazy message.
    /// Common IR carries only the semantic mode and operands; the JVM-specific class-status probe,
    /// function-interface invocation, and `AssertionError` constructors are chosen here.
    fn emit_assertion(
        &mut self,
        mode: crate::types::AssertionMode,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        use crate::types::AssertionMode;

        if mode == AssertionMode::AlwaysDisabled {
            debug_assert!(args.is_empty());
            return;
        }
        let ([condition] | [condition, _]) = args else {
            self.run
                .set_emit_error("checked assert has an invalid operand shape".to_string());
            return;
        };

        let entry_stack = code.stack_height().max(0) as u16;
        let end = code.new_label();
        if mode == AssertionMode::Runtime {
            code.ldc_class(&self.owner, self.cw);
            let enabled = self
                .cw
                .methodref("java/lang/Class", "desiredAssertionStatus", "()Z");
            code.invokevirtual(enabled, 0, 1);
            code.ifeq(end);
        }

        // A true condition exits the intrinsic. A constant true emits an unconditional jump and
        // makes the throwing fallthrough unreachable, so omit that dead instruction sequence.
        if !self.emit_cond_branch(*condition, end, true, code) {
            let assertion_error = self.cw.class_ref("java/lang/AssertionError");
            code.new_obj(assertion_error);
            code.dup();
            let descriptor = if let Some(message) = args.get(1) {
                self.emit_value(*message, code);
                let function = jvm_function_interface(0);
                let invoke = self.cw.interface_methodref(
                    &function,
                    "invoke",
                    &jvm_function_invoke_descriptor(0),
                );
                code.invokeinterface(invoke, 0, 1);
                "(Ljava/lang/Object;)V"
            } else {
                "()V"
            };
            let constructor = self
                .cw
                .methodref("java/lang/AssertionError", "<init>", descriptor);
            code.invokespecial(constructor, i32::from(args.len() == 2), 0);
            code.athrow();
        }
        self.bind(end, code);
        code.set_stack(entry_stack);
    }

    /// Whether emitting `e` as a value always transfers control away (returns/throws), so control
    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        self.ir
            .expr_diverges_by(e, &|_, value| matches!(value, IrExpr::BottomValue { .. }))
    }

    /// Whether statement-form emission of `e` transfers control. A generic call whose inferred
    /// result is `Nothing` is physically an erased value call and falls through after its result is
    /// discarded; only value-form emission synthesizes `KotlinNothingValueException`.
    fn discarding_diverges(&self, e: u32) -> bool {
        self.ir.expr_discarding_diverges_by(e, &|_, _| false)
    }

    /// The element `Ty` of an array-typed IR expression.
    fn array_elem(&self, e: u32) -> Ty {
        self.value_ty(e).array_elem().unwrap_or(Ty::Error)
    }

    fn record_label_assignment_state(&mut self, label: Label, code: &CodeBuilder) {
        // A switch marks the linear stream dead before its case frames are registered. Its targets
        // are seeded at dispatch time, so later case emission must not merge the preceding case's
        // state into the next one merely because both are emitted linearly.
        if code.is_dead() && self.label_unassigned_values.contains_key(&label) {
            return;
        }
        self.label_unassigned_values
            .entry(label)
            .and_modify(|unassigned| unassigned.extend(self.unassigned_values.iter().copied()))
            .or_insert_with(|| self.unassigned_values.clone());
    }

    fn bind(&mut self, label: Label, code: &mut CodeBuilder) {
        let resident = self.label_stack_resident_values.remove(&label);
        let joins_fallthrough = resident.is_some() && !code.is_dead();
        if joins_fallthrough {
            self.run.set_emit_error(
                "a label joins a stack-carried value with a fallthrough edge".to_string(),
            );
        }
        if !code.is_dead() {
            self.label_unassigned_values
                .entry(label)
                .and_modify(|unassigned| unassigned.extend(self.unassigned_values.iter().copied()))
                .or_insert_with(|| self.unassigned_values.clone());
        }
        if let Some(unassigned) = self.label_unassigned_values.get(&label) {
            self.unassigned_values.clone_from(unassigned);
        }
        code.bind(label);
        // The jump that reaches this label already pushed the result. The linear tracker still
        // shows the dead path in front of the label (an `athrow` leaves height 0), so the landing
        // use would load the slot again. A label that does not carry a result leaves whatever
        // value is already stack-resident: a safe-call receiver can cross an unrelated bind.
        if let Some(value) = resident {
            if joins_fallthrough {
                self.stack_resident_value = None;
            } else if let Some(words) = self
                .inline_return_frame_result_type(value)
                .map(slot_words)
                .filter(|words| *words > 0)
            {
                code.set_stack(words);
                self.stack_resident_value = Some(value);
            }
        }
    }

    /// The definitely-assigned semantic locals, as `(slot, type)`.
    fn assigned_semantic_slots(&self) -> Vec<(u16, Ty)> {
        let slots: Vec<(u32, u16, Ty)> = self
            .slots
            .iter()
            .filter(|(value, _)| !self.unassigned_values.contains(value))
            .map(|(value, (slot, ty))| (*value, *slot, *ty))
            .collect();
        slots
            .into_iter()
            .map(|(value, slot, ty)| (slot, self.semantic_local_frame_ty(value, ty)))
            .collect()
    }

    fn verif_single(&mut self, ty: Ty) -> VerifType {
        // Keep object types by name here. The final StackMapTable encoder interns only classes that
        // survive frame compression, avoiding constant-pool entries for omitted locals.
        match ty.kind() {
            _ if is_jvm_int_category(ty) => VerifType::Integer,
            TyKind::LONG => VerifType::Long,
            TyKind::DOUBLE => VerifType::Double,
            TyKind::FLOAT => VerifType::Float,
            TyKind::STRING => VerifType::ObjectName("java/lang/String".to_string()),
            // An array's verification type is an `Object` whose class name is its descriptor (`[I`).
            _ if ty.is_array() => VerifType::ObjectName(type_descriptor(ty)),
            TyKind::Obj(n, _) => {
                VerifType::ObjectName(crate::jvm::names::classfile_internal_name_of(n).to_string())
            }
            TyKind::Nullable(_) | TyKind::PlatformNullable(_) | TyKind::MutabilityFlexible(_) => {
                VerifType::ObjectName(crate::jvm::names::instanceof_internal_name(ty))
            }
            TyKind::Null => VerifType::Null,
            _ => VerifType::Top,
        }
    }

    fn value_ty(&self, e: u32) -> Ty {
        if let Some(result) = self.transformed_result(e) {
            return jvm_declared_ty(&result);
        }
        if matches!(self.ir.expr(e), IrExpr::When { .. }) {
            if let Some(result) = self.ir.whens.exhaustive.get(&e) {
                return ir_ty_to_jvm(result);
            }
        }
        match self.ir.expr(e) {
            IrExpr::StringConcat(_) => Ty::String,
            // A JVM class literal is a reference, so equality uses reference comparison.
            IrExpr::ClassConst { .. } => Ty::obj("java/lang/Class"),
            IrExpr::KClassLiteral { .. } => Ty::obj("kotlin/reflect/KClass"),
            IrExpr::Const(c) => match c {
                IrConst::Boolean(_) => Ty::Boolean,
                // The unsigned identity common IR retained. Answering `Int` here is what hid
                // the carrier question from every backend.
                IrConst::UByte(_) => Ty::UByte,
                IrConst::UShort(_) => Ty::UShort,
                IrConst::UInt(_) => Ty::UInt,
                IrConst::ULong(_) => Ty::ULong,
                IrConst::Int(_) => Ty::Int,
                IrConst::Long(_) => Ty::Long,
                IrConst::Double(_) => Ty::Double,
                IrConst::Float(_) => Ty::Float,
                IrConst::Char(_) => Ty::Char,
                IrConst::String(_) => Ty::String,
                IrConst::Short(_) => Ty::Short,
                IrConst::Byte(_) => Ty::Byte,
                IrConst::Null => Ty::Null,
            },
            IrExpr::GetValue(i) => self
                .unboxed_scalar_read(e, *i)
                .or_else(|| self.slots.get(i).map(|(_, t)| *t))
                .or_else(|| self.var_types.get(i).copied())
                .or_else(|| self.singleton_receiver_value(*i).map(Ty::obj_name))
                .unwrap_or(Ty::Error),
            IrExpr::EnclosingInstance { outer, .. } => instance_representation(self.ir, *outer),
            IrExpr::GetField { class, index, .. } => {
                // A field is a value slot. `Unit`, including a type parameter that erases to it,
                // is the `kotlin.Unit` reference the `getfield` just pushed — not `void`.
                jvm_value_ty(&self.ir.classes[*class as usize].fields[*index as usize].ty)
            }
            IrExpr::PropertyRead { ty, .. } => {
                // A read keeps its LOGICAL type in the IR; a value-class property's accessor returns
                // the carrier, which the value-class pass records beside it. The stack holds that.
                if let Some(physical) = self.ir.physical_types.get(&e) {
                    return jvm_value_ty(physical);
                }
                // A property read always yields a stored value. `Unit` therefore occupies the
                // `kotlin/Unit` reference slot; only a function's control-flow return uses `V`.
                jvm_value_ty(&stored_value_ty(*ty))
            }
            // A write is a statement: it leaves nothing on the stack, so nothing is discarded after it.
            IrExpr::PropertyWrite { .. } => Ty::Unit,
            IrExpr::GetStatic(i) => ir_ty_to_jvm(&self.ir.statics[*i as usize].ty),
            IrExpr::New { internal, .. } => Ty::obj_name(*internal),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                call_ret_ty(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call { callee, .. } => {
                if let Some(function) = callee.source_function() {
                    call_ret_ty(&self.ir.functions[function as usize].ret)
                } else {
                    match &**callee {
                        Callee::CrossFile { ret, .. }
                        | Callee::Module { ret, .. }
                        | Callee::Super { ret, .. }
                        | Callee::External { ret, .. }
                        | Callee::Intrinsic { ret, .. } => call_ret_ty(ret),
                        Callee::Static { owner, name, .. } if name == "box-impl" => {
                            Ty::nullable(Ty::obj_name(*owner))
                        }
                        Callee::Static { descriptor, .. } | Callee::Special { descriptor, .. } => {
                            // A kotlin `Nothing` return is a `java/lang/Void` JVM descriptor — report
                            // it as `Nothing` so a diverging (inlined `error(...)`) call is treated as
                            // never returning (no value, no dead epilogue after the spliced `athrow`).
                            if descriptor.ends_with(")Ljava/lang/Void;") {
                                Ty::Nothing
                            } else {
                                ty_from_descriptor_ret(descriptor)
                            }
                        }
                        // A user (sibling-file) method carries its return as a `Ty`; a classpath one
                        // via descriptor.
                        Callee::Virtual {
                            descriptor, params, ..
                        } => match params {
                            Some((_, ret)) => call_ret_ty(ret),
                            None if descriptor.ends_with(")Ljava/lang/Void;") => Ty::Nothing,
                            None => ty_from_descriptor_ret(descriptor),
                        },
                        _ => unreachable!("source-function callees were handled above"),
                    }
                }
            }
            IrExpr::Equality { .. } => Ty::Boolean,
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::RefEq
                | IrBinOp::RefNe
                | IrBinOp::And
                | IrBinOp::Or => Ty::Boolean,
                // Arithmetic leaves an unboxed primitive on the stack. Use the lhs carrier even when its
                // value is boxed (`it + 100` from `Map.get`); otherwise safe-call/elvis boxing may skip
                // `valueOf` and produce an `int`/`Integer` stackmap mismatch after spill removal.
                _ => {
                    let t = self.value_ty(*lhs);
                    let physical = match boxed_prim_of(t).unwrap_or(t) {
                        Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                        other => other,
                    };
                    self.ir
                        .logical_types
                        .get(&e)
                        .map(|semantic| ir_ty_to_jvm(&semantic.non_null()))
                        .unwrap_or(physical)
                }
            },
            IrExpr::PrimitiveNeg { ty, .. } => ir_ty_to_jvm(ty),
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::EnumEntry { classifier, .. } => Ty::obj_name(*classifier),
            IrExpr::EnumValueOf { classifier, .. } => Ty::obj_name(*classifier),
            IrExpr::StaticInstance { ty, .. } => {
                Ty::obj_name(self.ir.classes[*ty as usize].fq_name)
            }
            IrExpr::SingletonValue { classifier } => Ty::obj_name(*classifier),
            IrExpr::ExternalStaticInstance { ty, .. } => Ty::obj_name(*ty),
            // The static field's JVM type, from its descriptor.
            IrExpr::ExternalStaticField { descriptor, .. } => {
                super::physical_type::descriptor_operand_ty(descriptor)
            }
            IrExpr::RefNew { elem, .. } => Ty::obj(ref_class(elem).0),
            IrExpr::RefGet { elem, .. } => ir_ty_to_jvm(elem),
            IrExpr::RefSet { .. } => Ty::Unit,
            IrExpr::EnumValues { classifier } => Ty::array(Ty::obj_name(*classifier)),
            IrExpr::EnumSwitchOrdinal { .. } | IrExpr::EnumSwitchKey { .. } => Ty::Int,
            IrExpr::EnumEntries { classifier } => Ty::obj_args_name(
                crate::types::type_name("kotlin/enums/EnumEntries"),
                &[Ty::obj_name(*classifier)],
            ),
            IrExpr::ReifiedClassMarker { kclass, .. } => Ty::obj(if *kclass {
                "kotlin/reflect/KClass"
            } else {
                "java/lang/Class"
            }),
            IrExpr::ReifiedTypeOp { cast, erased, .. } => {
                if *cast {
                    Ty::obj_name(*erased)
                } else {
                    Ty::Boolean
                }
            }
            IrExpr::Block { value, .. } => {
                let Some(value) = *value else {
                    return Ty::Unit;
                };
                self.value_ty(value)
            }
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(&stored_value_ty(*type_operand)),
            },
            IrExpr::Lambda { arity, .. } => Ty::obj(&jvm_function_interface(*arity)),
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            IrExpr::NotNullAssert { operand, .. } => self.value_ty(*operand),
            IrExpr::LateinitCheck { operand, .. } => self.value_ty(*operand),
            IrExpr::Vararg { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::NewArray { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
            IrExpr::CurrentContinuation => Ty::obj("kotlin/coroutines/Continuation"),
            IrExpr::Try { result, .. } => ir_ty_to_jvm(&self.try_physical_result(e, *result)),
            other => diverging_value_type::diverging_value_ty(other).unwrap_or(Ty::Error),
        }
    }
}

/// The `LambdaMetafactory.metafactory` bootstrap-method descriptor (the standard non-altmetafactory form).
const LMF_METAFACTORY_DESC: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;\
Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;\
Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";

/// The boxed (wrapper) descriptor for a `Ty` — primitives map to their wrapper, references unchanged.
pub(super) fn boxed_descriptor(t: Ty) -> String {
    if t.non_null().is_unsigned() {
        let owner = t
            .non_null()
            .kotlin_class_internal()
            .expect("unsigned scalar must name its Kotlin classifier");
        return format!("L{};", owner.render());
    }
    match crate::jvm::jvm_class_map::wrapper_internal(t) {
        Some(w) => format!("L{w};"),
        None => type_descriptor(t),
    }
}

/// Whether one already-parsed JVM field descriptor occupies a reference slot.
///
/// Descriptor interpretation stays in the JVM emitter; common resolution and IR carry only the
/// semantic SAM and the provider-supplied method spelling. Arrays are references just like objects,
/// while primitive and `void` spellings are not. Keeping this tiny predicate shared by parameter and
/// return specialization prevents the two halves of a LambdaMetafactory boundary from drifting.
pub(super) fn descriptor_is_reference(descriptor: &str) -> bool {
    descriptor.starts_with('L') || descriptor.starts_with('[')
}

/// JVM internal name for a reference `Ty`, for `instanceof`/`checkcast`.
/// Convert the numeric primitive on top of the stack from `from` to `to` (JVM `i2l`/`i2d`/…).
/// Byte/Short/Char live in the `int` stack category; widening goes via that category, and a
/// Byte/Short/Char target is narrowed from `int` last.
/// Parse the return type of a JVM method descriptor (`(…)Lfoo/Bar;` → `Obj("foo/Bar")`) into a `Ty`.
fn ty_from_descriptor_ret(desc: &str) -> Ty {
    super::physical_type::method_return_operand_ty(desc)
        .expect("emitted invocation has a valid JVM method descriptor")
}

fn descriptor_ret_words(desc: &str) -> i32 {
    // A genuinely `void` (`)V`) method leaves nothing on the stack; `ty_from_descriptor_ret` maps `V` to
    // `Unit` (a 1-word value) for type flow elsewhere.
    if desc.ends_with(")V") {
        0
    } else {
        slot_words(ty_from_descriptor_ret(desc)) as i32
    }
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
/// If `t` is the boxed-reference form of a primitive (the element of a `Array<Int>` etc., carried as
/// `Obj("kotlin/Int")`), the underlying primitive `Ty`. Used to insert box/unbox at the boxed-array
/// element boundary (`a[i]` yields an unboxed `Int`; `a[i] = v` boxes the `Int`).
fn boxed_prim_of(t: Ty) -> Option<Ty> {
    t.unboxed_primitive()
}

/// `(opcode, value-words)` for an array element store (`Xastore`).
/// Push the zero value of `t` (the placeholder for an omitted `$default` argument; the stub overwrites
/// it when the mask bit is set).
fn push_zero(t: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    match t {
        Ty::Long => code.lconst_0(),
        Ty::Double => code.dconst_0(),
        Ty::Float => code.fconst_0(),
        t if is_jvm_int_category(t) => code.push_int(0, cw),
        _ => code.aconst_null(),
    }
}

fn is_jvm_int_category(t: Ty) -> bool {
    matches!(t, Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char)
}

/// True when a `RefEq`/`RefNe` between `lt` and `rt` must compare OBJECT REFERENCES (`if_acmp*`).
/// Only a pair of JVM SCALARS is a value comparison (Kotlin's `===` on two primitives is just `==`,
/// remapped to `Eq`/`Ne`); everything else rides in a reference slot. Two references compare as-is,
/// and a mixed reference/primitive pair boxes its primitive side first (kotlinc's "unstable because
/// of implicit boxing" case).
///
/// Phrased as "not both scalars" rather than "either is a reference" because `is_reference()` is a
/// LANGUAGE-level query and misses types that are nonetheless references on the JVM: `Ty::Unit` (whose
/// value is the `kotlin/Unit.INSTANCE` singleton) and `Ty::Null`. Testing for references directly left
/// `g() === g()` and `x === null` in the numeric tail, which is exactly the int-branch-on-a-reference
/// bug this predicate exists to prevent.
fn identity_compares_refs(lt: Ty, rt: Ty) -> bool {
    !(lt.is_jvm_scalar() && rt.is_jvm_scalar())
}

/// The int-vs-wide category of a numeric comparison's operands: `true` for the int-category primitives
/// that fuse to `if_icmp*`/compare-to-zero, `false` for `Long`/`ULong`/`Double`/`Float`, which compare
/// 3-way through `lcmp`/`dcmp*`/`fcmp*` first. `ULong` occupies a `long` slot; treating it as an int
/// emits `if_icmp*` against two longs.
///
/// Deriving this as a bare "not `Long`/`Double`/`Float`" swept every REFERENCE type into the int
/// category, so a mixed reference/primitive `===` that slipped past the identity path emitted an int
/// branch on an object ref — a class file that is written out fine and only fails at verification
/// (`VerifyError: Bad type on operand stack`). Reference operands must be handled by the identity /
/// null / `Intrinsics.areEqual` paths before the numeric tail, so reaching here with one is an emitter
/// bug: assert rather than silently emit unverifiable bytecode. `Unit`/`Nothing` are neither, and
/// likewise never reach a numeric comparison.
fn numeric_cmp_int_category(lt: Ty, rt: Ty) -> bool {
    assert!(
        lt.is_jvm_scalar() && rt.is_jvm_scalar(),
        "numeric comparison reached with a non-scalar operand ({lt:?} vs {rt:?}) — \
         reference shapes belong on the identity/null/areEqual paths"
    );
    !matches!(lt, Ty::Long | Ty::ULong | Ty::Double | Ty::Float)
}

/// Normalize a call's return JVM-type: a Kotlin `Nothing` is carried as an object whose JVM mapping is
/// `java/lang/Void` (the descriptor the front end emits for it). Collapse that to the `Ty::Nothing`
/// bottom variant so `diverges`/`value_ty_of_when` see the call never returns — a `Static`/`Virtual`
/// callee already gets this from its `)Ljava/lang/Void;` descriptor; a `Local`/`CrossFile`/method callee
/// reads the IR `ret` directly and needs the same normalization (else a `Nothing`-returning call's value
/// is wrongly merged/popped, e.g. an `exit()` branch of an `if` ⇒ inconsistent stackmap frames).
/// Whether an IR return type is the NON-nullable bottom type `Nothing` (so a call to it never returns and
/// must be terminated). A `Nothing?` return is NULLABLE — it can yield `null` (`fun f(): Nothing? { … return
/// null … }`) — and must NOT be treated as diverging; the JVM descriptor erases the `?` (both are `Void`),
/// so the nullability is checked on the IR type before it is erased by `ir_ty_to_jvm`.
fn ret_is_nothing(ret: &Ty) -> bool {
    !ret.is_nullable() && norm_nothing(ir_ty_to_jvm(ret)) == Ty::Nothing
}

/// Exact JVM stack effect of a call result described by a semantic IR return. `Nothing` has no
/// language value, but its method descriptor returns `java/lang/Void` and therefore contributes one
/// physical word until the enclosing bottom-completion wrapper discards it.
fn physical_call_result_words(ret: Ty) -> i32 {
    if ret_is_nothing(&ret) {
        1
    } else {
        slot_words(ret) as i32
    }
}

/// The JVM `Ty` a call to a function with IR return `ret` leaves on the stack: the `Ty::Nothing` bottom
/// for a NON-nullable `Nothing` return (no value — the call diverges), else the erased reference/value
/// type. A `Nothing?` return is a real nullable reference (`Void`, 1 slot) that yields `null`, so it must
/// NOT collapse to `Nothing` (that would mis-size discards and mis-flag it as diverging).
fn call_ret_ty(ret: &Ty) -> Ty {
    if ret_is_nothing(ret) {
        Ty::Nothing
    } else {
        ir_ty_to_jvm(ret)
    }
}

fn norm_nothing(t: Ty) -> Ty {
    match t.kind() {
        TyKind::Obj(n, _)
            if crate::jvm::jvm_class_map::type_name_maps_to_jvm_internal(n, "java/lang/Void") =>
        {
            Ty::Nothing
        }
        _ => t,
    }
}

pub use crate::jvm::physical_type::ir_ty_to_jvm;

/// The JVM element type of an array given its whole array type. `ir_ty_to_jvm` already maps
/// `kotlin/Array<Int>` → `[Ljava/lang/Integer;` (boxed) and `kotlin/IntArray` → `[I` (primitive), so the
/// boxed-vs-primitive distinction is carried by the type — no flag needed.
fn array_jvm_element(array_type: &Ty) -> Ty {
    ir_ty_to_jvm(array_type)
        .array_elem()
        .unwrap_or_else(|| Ty::obj("java/lang/Object"))
}

fn primitive_spread_builder(element: Ty) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match element {
        Ty::Boolean => ("kotlin/jvm/internal/BooleanSpreadBuilder", "(Z)V", "[Z"),
        Ty::Char => ("kotlin/jvm/internal/CharSpreadBuilder", "(C)V", "[C"),
        Ty::Byte => ("kotlin/jvm/internal/ByteSpreadBuilder", "(B)V", "[B"),
        Ty::Short => ("kotlin/jvm/internal/ShortSpreadBuilder", "(S)V", "[S"),
        Ty::Int | Ty::UInt => ("kotlin/jvm/internal/IntSpreadBuilder", "(I)V", "[I"),
        Ty::Long | Ty::ULong => ("kotlin/jvm/internal/LongSpreadBuilder", "(J)V", "[J"),
        Ty::Float => ("kotlin/jvm/internal/FloatSpreadBuilder", "(F)V", "[F"),
        Ty::Double => ("kotlin/jvm/internal/DoubleSpreadBuilder", "(D)V", "[D"),
        _ => return None,
    })
}

/// A single-operand compare-to-zero branch (`ifeq`/`ifne`/`iflt`/`ifle`/`ifgt`/`ifge`) to `target`,
/// taken when `(value <op> 0) == jt`. Used for `x <op> 0` and for the 3-way `lcmp`/`dcmp*`/`fcmp*`
/// result tested against 0, which is already -1/0/1.
fn cmp0_branch(op: IrBinOp, jt: bool, target: Label, code: &mut CodeBuilder) {
    use IrBinOp::*;
    match (op, jt) {
        (Lt, true) => code.iflt(target),
        (Lt, false) => code.ifge(target),
        (Le, true) => code.ifle(target),
        (Le, false) => code.ifgt(target),
        (Gt, true) => code.ifgt(target),
        (Gt, false) => code.ifle(target),
        (Ge, true) => code.ifge(target),
        (Ge, false) => code.iflt(target),
        (Eq, true) => code.ifeq(target),
        (Eq, false) => code.ifne(target),
        (Ne, true) => code.ifne(target),
        (Ne, false) => code.ifeq(target),
        _ => unreachable!(),
    }
}

/// A two-operand int-category comparison branch (`if_icmplt`/`if_icmpge`/…) to `target`, taken when
/// `(a <op> b) == jt`. The `jt = false` rows are the negated operator, which is how a value-position
/// comparison reaches its `false` arm.
fn icmp_branch(op: IrBinOp, jt: bool, target: Label, code: &mut CodeBuilder) {
    use IrBinOp::*;
    match (op, jt) {
        (Lt, true) => code.if_icmplt(target),
        (Lt, false) => code.if_icmpge(target),
        (Le, true) => code.if_icmple(target),
        (Le, false) => code.if_icmpgt(target),
        (Gt, true) => code.if_icmpgt(target),
        (Gt, false) => code.if_icmple(target),
        (Ge, true) => code.if_icmpge(target),
        (Ge, false) => code.if_icmplt(target),
        (Eq, true) => code.if_icmpeq(target),
        (Eq, false) => code.if_icmpne(target),
        (Ne, true) => code.if_icmpne(target),
        (Ne, false) => code.if_icmpeq(target),
        _ => unreachable!(),
    }
}

fn slot_words(t: Ty) -> u16 {
    match t {
        // `ULong` is a `long` on the JVM — two words, like `Long`/`Double` (`UInt` is one, like `Int`).
        Ty::Long | Ty::Double | Ty::ULong => 2,
        Ty::Unit | Ty::Nothing => 0,
        _ => 1,
    }
}

fn load(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lload(slot),
        Ty::Double => code.dload(slot),
        Ty::Float => code.fload(slot),
        t if is_jvm_int_category(t) => code.iload(slot),
        _ => code.aload(slot),
    }
}

fn store(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lstore(slot),
        Ty::Double => code.dstore(slot),
        Ty::Float => code.fstore(slot),
        t if is_jvm_int_category(t) => code.istore(slot),
        _ => code.astore(slot),
    }
}

fn emit_return(t: Ty, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lreturn(),
        Ty::Double => code.dreturn(),
        Ty::Float => code.freturn(),
        t if is_jvm_int_category(t) => code.ireturn(),
        Ty::Unit | Ty::Nothing => code.ret_void(),
        _ => code.areturn(),
    }
}

fn methodref_owner<'a>(body: &'a MethodCode, name: &str, descriptor: &str) -> Option<&'a str> {
    fn utf8(cp: &[C], idx: u16) -> Option<&str> {
        match cp.get(idx as usize)? {
            C::Utf8(s) => Some(s.as_str()),
            _ => None,
        }
    }
    fn class_name(cp: &[C], idx: u16) -> Option<&str> {
        match cp.get(idx as usize)? {
            C::Class(name_idx) => utf8(cp, *name_idx),
            _ => None,
        }
    }
    fn name_and_desc(cp: &[C], idx: u16) -> Option<(&str, &str)> {
        match cp.get(idx as usize)? {
            C::NameAndType(name_idx, desc_idx) => {
                Some((utf8(cp, *name_idx)?, utf8(cp, *desc_idx)?))
            }
            _ => None,
        }
    }

    body.source_cp.iter().find_map(|entry| {
        let C::Methodref(class_idx, nt_idx) = entry else {
            return None;
        };
        let (n, d) = name_and_desc(&body.source_cp, *nt_idx)?;
        (n == name && d == descriptor).then(|| class_name(&body.source_cp, *class_idx))?
    })
}

#[cfg(test)]
mod invariant_tests {
    use super::*;
    use crate::ir::{IrConst, IrExpr, IrFile, IrFunction};
    use crate::jvm::classreader::MethodCode;
    use crate::jvm::inline::MethodBodies;
    use crate::types::Ty;

    #[test]
    fn descriptor_ty_consumers_preserve_reference_array_elements() {
        let (params, result) =
            parse_physical_method_desc("([Lfixture/Token;[Lkotlin/UInt;)[Lkotlin/UInt;")
                .expect("valid physical method descriptor");
        assert_eq!(type_descriptor(params[0]), "[Lfixture/Token;");
        assert_eq!(type_descriptor(params[1]), "[Lkotlin/UInt;");
        assert_eq!(type_descriptor(result), "[Lkotlin/UInt;");
    }

    pub(super) struct NoBodies;
    impl MethodBodies for NoBodies {
        fn body(&self, _o: &str, _n: &str, _d: &str) -> Option<MethodCode> {
            None
        }
    }

    pub(super) fn emit_for_test(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        emit_for_test_with_facts(
            ir,
            facade,
            run,
            &NoBodies,
            &crate::jvm::suspend::EmitTimeMachines::default(),
            &crate::jvm::suspend::IntrinsicProbeContinuations::default(),
        )
    }

    /// [`emit_for_test`] with the emit-time coroutine machines a suspend pass would have recorded.
    pub(super) fn emit_for_test_with_machines(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
        emit_time_machines: &crate::jvm::suspend::EmitTimeMachines,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        emit_for_test_with_facts(
            ir,
            facade,
            run,
            &NoBodies,
            emit_time_machines,
            &crate::jvm::suspend::IntrinsicProbeContinuations::default(),
        )
    }

    pub(super) fn emit_for_test_with_probe_continuations(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
        intrinsic_probe_continuations: &crate::jvm::suspend::IntrinsicProbeContinuations,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        emit_for_test_with_facts(
            ir,
            facade,
            run,
            &NoBodies,
            &crate::jvm::suspend::EmitTimeMachines::default(),
            intrinsic_probe_continuations,
        )
    }

    /// [`emit_for_test`] with the inline bodies the classpath would supply.
    pub(super) fn emit_for_test_with_bodies(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
        bodies: &dyn MethodBodies,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        emit_for_test_with_facts(
            ir,
            facade,
            run,
            bodies,
            &crate::jvm::suspend::EmitTimeMachines::default(),
            &crate::jvm::suspend::IntrinsicProbeContinuations::default(),
        )
    }

    fn emit_for_test_with_facts(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
        bodies: &dyn MethodBodies,
        emit_time_machines: &crate::jvm::suspend::EmitTimeMachines,
        intrinsic_probe_continuations: &crate::jvm::suspend::IntrinsicProbeContinuations,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        let continuations = crate::jvm::suspend::ContinuationMetadataMap::default();
        let property_realizations =
            crate::jvm::property_realizations::PropertyRealizations::default();
        let property_reference_realizations =
            crate::jvm::property_references::PropertyReferenceRealizations::default();
        let default_call_operands =
            crate::jvm::default_call_operands::DefaultCallOperands::default();
        let sam_wrapper_realizations = crate::jvm::sam_wrappers::SamWrapperRealizations::default();
        let bridge_adaptations = crate::jvm::bridge_adaptations::BridgeAdaptations::default();
        let function_argument_arrays =
            crate::jvm::function_argument_arrays::FunctionArgumentArrays::default();
        let suspended_result_returns = crate::jvm::suspend::SuspendedResultReturns::default();
        let override_results = crate::jvm::override_results::OverrideResults::default();
        let collection_method_entry_barriers =
            crate::jvm::collection_barriers::MethodEntryBarriers::default();
        let collection_members =
            crate::jvm::read_only_collection_stubs::CollectionMemberPlans::default();
        let local_delegate_access = crate::jvm::local_delegate_accessors::HelperAccess::default();
        let dependency_callables = crate::backend::CheckedBackendCallables::default();
        emit_all_with_checked_classifiers(
            ir,
            (crate::types::type_name(facade), facade),
            bodies,
            CheckedEmitFacts {
                metadata: EmitMetadata {
                    facade: None,
                    continuations: &continuations,
                    bridge_adaptations: &bridge_adaptations,
                    function_argument_arrays: &function_argument_arrays,
                    override_results: &override_results,
                    collection_method_entry_barriers: &collection_method_entry_barriers,
                    collection_members: &collection_members,
                    emit_time_machines,
                    suspended_result_returns: &suspended_result_returns,
                    intrinsic_probe_continuations,
                },
                signature_symbols: &super::metadata_name_tests::DeclarationNames(ir),
                dependency_callables: &dependency_callables,
                property_realizations: &property_realizations,
                property_reference_realizations: &property_reference_realizations,
                default_call_operands: &default_call_operands,
                sam_wrapper_realizations: &sam_wrapper_realizations,
                local_delegate_access: &local_delegate_access,
            },
            &EmitOptions::default(),
            run,
        )
    }

    #[test]
    fn anonymous_enclosure_uses_the_bound_function_id_not_name_shape() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("demo/Owner");
        ir.add_fun(IrFunction {
            name: "build".into(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: vec![],
        });
        let selected = ir.add_fun(IrFunction {
            name: "build".into(),
            params: vec![Ty::String],
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: vec![],
        });
        let mut anonymous = crate::plugins::synthetic_class("demo/opaque_identity");
        anonymous.is_anonymous_object = true;
        anonymous.enclosure = Some(crate::ir::IrEnclosure::Function(selected));

        let (resolved_owner, method) = class_enclosure(
            &ir,
            &crate::jvm::override_results::OverrideResults::default(),
            &anonymous,
            "IgnoredFacade",
        )
        .expect("bound enclosure");
        assert_eq!(resolved_owner, "demo/Owner");
        assert_eq!(
            method,
            Some(("build".to_string(), "(Ljava/lang/String;)V".to_string()))
        );
    }

    #[test]
    fn nothing_has_one_declared_slot_but_remains_bottom_at_a_call() {
        assert_eq!(jvm_declared_ty(&Ty::Nothing), Ty::obj("java/lang/Void"));
        assert_eq!(
            jvm_declared_ty(&Ty::nullable(Ty::Nothing)),
            Ty::obj("java/lang/Void")
        );
        assert_eq!(slot_words(jvm_declared_ty(&Ty::Nothing)), 1);
        assert_eq!(call_ret_ty(&Ty::Nothing), Ty::Nothing);
        assert_eq!(
            call_ret_ty(&Ty::nullable(Ty::Nothing)),
            Ty::obj("kotlin/Any")
        );
    }

    // A `GetValue` of a value slot that was never allocated is malformed IR. It is not an
    // unsupported source construct and must not enter the recoverable emit-error path.
    #[test]
    #[should_panic(
        expected = "malformed IR: value 99 has no allocated JVM slot while emitting TestKt"
    )]
    fn getvalue_of_unallocated_slot_is_an_explicit_backend_invariant_violation() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::GetValue(99));
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        let _ = emit_for_test(&ir, "TestKt", &EmitRun::default());
    }

    #[test]
    #[should_panic(
        expected = "malformed IR: assignment target 99 has no allocated JVM slot while emitting TestKt"
    )]
    fn setvalue_of_unallocated_slot_is_an_explicit_backend_invariant_violation() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let body = ir.add_expr(IrExpr::SetValue { var: 99, value });
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        let _ = emit_for_test(&ir, "TestKt", &EmitRun::default());
    }

    // A `Unit` declaration owns a `kotlin/Unit` reference slot, so reading it pushes one operand. A
    // discarded block whose value is that read is popped only after the block's scope has closed,
    // when the type comes from the body-wide declaration table instead of the live slot map. Both
    // must name the slot the declaration owns; reading the table as `Unit` (no operand) left the
    // reference on the stack, and the `if` branch reached its join one operand higher than the
    // empty `else`.
    #[test]
    fn a_discarded_unit_temporary_read_is_popped_after_its_scope_closes() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 1,
            ty: Ty::Unit,
            init: Some(unit),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(1));
        let temporary = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration],
            value: Some(read),
        });
        let then_branch = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(temporary),
        });
        let else_branch = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let condition = ir.add_expr(IrExpr::GetValue(0));
        let branch = ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), then_branch), (None, else_branch)],
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![branch],
            value: None,
        });
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![Ty::Boolean],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        // Before the fix this panicked computing frames: `StackHeight` at the branch join.
        let classes = emit_for_test(&ir, "TestKt", &EmitRun::default()).expect("emitted class");
        let (_, bytes) = classes
            .iter()
            .find(|(name, _)| name == "TestKt")
            .expect("facade class");
        let code =
            crate::jvm::classreader::read_method_code(bytes, "box", "(Z)V").expect("box code");
        // The popped read is dead, and the optimizer then removes the unused temporary with it.
        assert_eq!(
            code.max_stack, 1,
            "only the condition reaches the stack; the temporary's read is popped"
        );
    }

    // A machine recorded for a function with no `$completion` parameter cannot resolve the
    // continuation it is re-entered on. Arming it anyway and letting the prologue decline emitted a
    // body full of coroutine markers (`impdep1`, reserved by JVMS §6.2) over a continuation slot
    // nothing assigned, and reached NO erasure pass: both are inside the branch that runs only when
    // the prologue succeeded. The decision belongs before the machine is armed.
    #[test]
    fn a_machine_that_cannot_resolve_its_continuation_declines_before_it_is_armed() {
        let mut ir = IrFile::default();
        let suspension = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
        let function = ir.add_fun(IrFunction {
            name: "box".into(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(suspension),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut machines = crate::jvm::suspend::EmitTimeMachines::default();
        machines.record(
            function,
            vec![crate::jvm::suspend::cps::SplicedSuspension {
                call: suspension,
                resumed_box: None,
            }],
        );
        // The EMITTING pass: a plan is already in hand, so the machine is what this emission builds.
        let run = EmitRun::default();
        run.record_machine_plan(
            function,
            coroutine_machine::MachinePlan {
                suspensions: vec![coroutine_machine::SuspensionPlan::default()],
                body_locals: 0,
            },
        );
        assert!(
            emit_for_test_with_machines(&ir, "TestKt", &run, &machines).is_none(),
            "a machine that cannot be built declines the compile instead of emitting half of one",
        );
        assert_eq!(
            run.inline_bail().as_deref(),
            Some("a coroutine machine with no continuation parameter"),
        );
    }
}
