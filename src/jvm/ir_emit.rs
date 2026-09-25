//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of backend-agnostic IR maps Kotlin
//! identities to JVM descriptors here; common IR never carries descriptors.

use std::collections::{HashMap, HashSet};

use crate::backend::BackendClassifierSource;
use crate::ir::{
    Callee, IrBinOp, IrClass, IrConst, IrDataClassMemberRole, IrExpr, IrField, IrFile, IrTypeOp,
};
use crate::jvm::array_representation::{array_load_op, array_store_op, prim_newarray_atype};
use crate::jvm::classfile::{
    ClassWriter, CodeBuilder, InnerClassResolver, Label, VerifType, MAJOR_JAVA8,
};
use crate::jvm::classreader::{MethodCode, C};
use crate::jvm::constructor_debug::property_line;
use crate::jvm::inline::MethodBodies;
use crate::jvm::names::{
    mapped_builtin_virtual_name, method_descriptor, property_getter_name, property_setter_name,
    reference_array_element, type_descriptor,
};
use crate::kt_string::KtStringBuf;
use crate::types::{stored_value_ty, Ty, TypeName, TypeVariance};

mod access_bridges;
mod annotation_impl;
mod backend_temporaries;
mod block_scope;
mod bottom_values;
mod bridge_emission;
mod bytecode_inline_call;
mod call_operands;
mod captured_storage;
mod checked_facts;
mod class_pool_seed;
mod condition_emission;
mod constructor_accessors;
mod constructor_defaults;
mod coroutine_machine;
mod data_class_pool_seed;
mod data_class_value_classes;
mod debug_lines;
mod declaration_types;
mod declared_nullability;
mod discarding;
mod enum_entry_subclass;
mod enum_metadata;
mod field_write;
mod frame_map;
mod function_debug;
mod function_reference_invoke;
mod implicit_reference_coercion;
mod in_place_arguments;
mod inline_body_emission;
mod inline_call;
mod interface_compatibility;
mod local_updates;
mod member_schedule;
mod metadata_policy;
mod non_null_operands;
mod object_static_initialization;
mod operand_representation;
mod operand_stack;
mod primary_constructor_parameters;
mod property_access;
mod property_reference_values;
mod return_emission;
mod safe_calls;
mod scalar_coercion;
mod transformed_suspensions;
mod try_emission;
use annotation_impl::emit_annotation_impl_class;
mod value_class_descriptors;
mod value_class_signatures;
use class_pool_seed::{
    seed_data_class_pool, seed_plain_class_pool, seed_plain_constructor_tail, PlainClassPoolSeed,
};
use primary_constructor_parameters::{
    primary_ctor_parameter_fields, primary_ctor_source_parameters,
};
use try_emission::FinallyRegion;
mod secondary_constructor;
mod static_fields;
mod type_operation_emission;
mod vararg;
mod when;

use super::method_parameters::OwnerConstructorPrefix;
pub(crate) use checked_facts::{CheckedEmitFacts, EmitMetadata};
pub(super) use declaration_types::function_descriptor;
pub(crate) use declaration_types::jvm_tys;
pub(super) use declaration_types::{class_ctor_jvm_tys, ir_method_desc};
use declaration_types::{field_jvm_tys, jvm_declared_ty};
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
    is_continuation_class, is_coroutine_state_machine, synthetic_class_xi, SYNTHETIC_LOCAL,
    SYNTHETIC_PROTECTED, SYNTHETIC_PUBLIC,
};
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
    c.is_object && !c.is_companion && !c.is_local_class && c.enum_entry_of.is_none()
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
/// layout): its instance lives in a `static final $$INSTANCE` on the companion itself, its
/// properties back `static` fields there, and the interface's `Companion` field merely aliases
/// `$$INSTANCE` in the interface `<clinit>` — nothing hoists onto the interface (whose fields
/// would be forced `public static final`).
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

/// Whether a static-storage object's `init_body` reads `this` (`GetValue(0)`) anywhere OTHER than
/// as the receiver of a store to its own (now static) field — those receivers are dropped by the
/// `putstatic` lowering, so only remaining reads force materializing INSTANCE into a local.
fn init_body_reads_this(ir: &IrFile, body: crate::ir::ExprId) -> bool {
    fn walk(ir: &IrFile, e: crate::ir::ExprId, skip_self: bool) -> bool {
        use crate::ir::IrExpr;
        match ir.expr(e) {
            IrExpr::GetValue(v) => *v == 0 && !skip_self,
            IrExpr::SetField {
                receiver, value, ..
            } => {
                let receiver_is_this = matches!(ir.expr(*receiver), IrExpr::GetValue(0));
                (!receiver_is_this && walk(ir, *receiver, false)) || walk(ir, *value, false)
            }
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| walk(ir, s, false))
                    || value.is_some_and(|v| walk(ir, v, false))
            }
            _ => {
                let mut found = false;
                crate::ir::for_each_child(&ir.exprs, e, &mut |child| {
                    if walk(ir, child, false) {
                        found = true;
                    }
                });
                found
            }
        }
    }
    walk(ir, body, false)
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
    /// The reason an inline splice failed during emission (a required stdlib-inline call the backend
    /// could not splice), else `None`.
    inline_bail: std::cell::RefCell<Option<String>>,
    /// Set when a `GetValue`/`SetValue` references a value slot that was never allocated (malformed IR
    /// from an unsupported lowering). The emitter never panics: it sets this and the file is dropped —
    /// a compiler must never crash on its own IR.
    emit_bail: std::cell::Cell<bool>,
    emit_error: std::cell::RefCell<Option<String>>,
    /// Lambda impl `FunId`s that got a REAL `invokedynamic` this pass. A lambda spliced by the inliner
    /// (a `require { … }` message, an inlined `flatMap { … }` body) never emits one, so its standalone
    /// `$lambda$N` method is dead — dropped on the re-emit (kotlinc emits neither it nor its facade).
    used_lambdas: std::cell::RefCell<std::collections::HashSet<u32>>,
    /// Lambda impls proven dead by the discovery emit. Unlike `inline_only_fns`, this is an
    /// emit-run decision: it includes class-owned helpers whose every use was spliced, and is cleared
    /// before the next independent emission of the same IR.
    dead_lambdas: std::cell::RefCell<std::collections::HashSet<u32>>,
    /// Subset of live closures actually realized with `invokedynamic`. A class-strategy closure still
    /// keeps its implementation method but does not impose the JVM-7 classfile requirement.
    used_indy_lambdas: std::cell::RefCell<std::collections::HashSet<u32>>,
    /// Lambda classes to synthesize for `-Xlambdas=class`, recorded as their `invokedynamic`
    /// replacement is emitted and drained by the class driver. They cannot be written inline: the
    /// emitter is mid-way through the ENCLOSING class's writer when it reaches a lambda.
    lambda_classes: std::cell::RefCell<Vec<LambdaClassPlan>>,
    /// Source/synthetic identity of every lambda class already written in the active emit pass,
    /// paired with its JVM owner. A source lambda lowered into multiple constructors still
    /// contributes one class. The discovery pass is discarded when dead implementations require a
    /// second emit, so this set is cleared with the pending plans at each pass boundary.
    lambda_classes_written:
        std::cell::RefCell<std::collections::HashSet<(String, LambdaClassIdentity)>>,
    /// Private instance members reached from another emitted JVM class. Kotlin permits this across
    /// lexical nesting, while Java 8 bytecode does not; the declaring class owns one synthetic
    /// static access bridge and every cross-owner call targets it.
    private_member_access_bridges: std::cell::RefCell<std::collections::HashSet<u32>>,
    /// Spill plans discovered for the suspend functions whose coroutine machine emission owns.
    /// Absent on the discovery pass and present on the one that builds the machine.
    machine_plans: std::cell::RefCell<coroutine_machine::MachinePlans>,
    /// Continuation classes synthesized for the machines this emission builds, drained with the
    /// facade they belong to.
    machine_classes: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    /// What kotlinc's coroutine transformer found, by continuation class.
    transformed_coroutines: transformed_suspensions::TransformedCoroutines,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LambdaClassIdentity {
    Source(u32),
    Synthetic(u32),
}

/// Realize backend-private implementation names from the semantic lambda origins produced by
/// common lowering. The common IR deliberately keeps its opaque temporary function name; only the
/// JVM boundary owns kotlinc's `$lambda$N` spelling.
pub(crate) fn realize_lambda_impl_names(ir: &mut IrFile) {
    let names = ir
        .lambda_origins
        .iter()
        .map(|(&function, origin)| {
            (
                function,
                super::debug_local_names::lambda_implementation_name(origin),
            )
        })
        .collect::<Vec<_>>();
    for (function, name) in names {
        ir.functions[function as usize].name = name;
    }
}

/// One synthetic lambda class to write under [`LambdaMode::Class`].
///
/// The lambda body stays where the indy strategy put it — a private static on the enclosing class —
/// and the synthesized `invoke` delegates to it. kotlinc instead moves the body into `invoke` and
/// emits no static, so the CLASS SET matches but the enclosing class keeps one extra private method.
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
    /// Source-level arity, passed to `kotlin.jvm.internal.Lambda`'s constructor.
    arity: u32,
    /// Only a Kotlin function type extends `kotlin/jvm/internal/Lambda`; a user SAM conversion
    /// extends `java/lang/Object` (kotlinc emits no `FunctionBase` for a non-`FunctionN` target).
    kotlin_function: bool,
    /// A fun-interface conversion of a callable reference implements Kotlin's FunctionAdapter
    /// equality contract by delegating to its single captured function value.
    function_adapter: bool,
    /// Whether the body lives on an INTERFACE: a static call to one needs an `InterfaceMethodref`
    /// constant, not a `Methodref` (`IncompatibleClassChangeError` otherwise).
    owner_is_interface: bool,
    identity: LambdaClassIdentity,
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
}

/// The emit environment threaded (by `&`) through the whole emit callgraph in place of the bare
/// `bodies` provider: the bytecode provider plus the mutable run accumulators, so the deep `Emitter`
/// records a used lambda / an emit-or-inline bail without an ambient thread-local. Replacing `bodies`
/// keeps every function's argument count unchanged.
pub(super) struct EmitEnv<'a> {
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
    continuation_metadata: &'a crate::jvm::suspend::ContinuationMetadataMap,
    emit_time_machines: &'a crate::jvm::suspend::EmitTimeMachines,
    unit_result_tail_forwards: &'a crate::jvm::suspend::UnitResultTailForwards,
    bridge_return_adaptations: &'a crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations,
    /// Semantic classifier declarations used only while translating Kotlin generic types into JVM
    /// `Signature` attributes. Declaration-site variance is a Kotlin fact; spelling it as JVM
    /// use-site wildcards is owned entirely by this emitter.
    signature_symbols: &'a dyn BackendClassifierSource,
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
    /// Per-call JVM placeholder/mask/marker plans produced during default-call realization.
    default_call_operands: &'a crate::jvm::default_call_operands::DefaultCallOperands,
    /// File-wide `InnerClasses` candidates prepared once from stable classifier identities.
    inner_classes: crate::jvm::inner_classes::InnerClasses,
    /// `-java-parameters`: name each declared parameter in a `MethodParameters` attribute.
    java_parameters: bool,
}

/// kotlinc's `IrClass.isLocal` for a declared classifier: a class declared in executable code (a
/// local class or an anonymous object) or nested, at any depth, in one. kotlinc's lambda and
/// callable-reference classes are local too; their writers here annotate nothing to begin with.
fn is_local_classifier(ir: &IrFile, class: &crate::ir::IrClass) -> bool {
    class.is_local_class
        || class.is_anonymous_object
        || class
            .fq_name_id()
            .existing_nested_owners()
            .into_iter()
            .find_map(|owner| ir.class_id_by_name(owner))
            .is_some_and(|owner| is_local_classifier(ir, &ir.classes[owner as usize]))
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

impl LambdaModes {
    fn for_sam(self, is_sam: bool) -> LambdaMode {
        if is_sam {
            self.sam_conversions
        } else {
            self.lambdas
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

/// Drop every `Intrinsics.checkNotNullParameter` guard the lowering recorded.
///
/// `-Xno-param-assertions` removes the parameter null checks kotlinc emits at the entry of every
/// function reachable from Java. Applied to the IR rather than at the emission site on purpose: the
/// guards are also what the `LineNumberTable` and `LocalVariableTable` start offsets are computed
/// from, so suppressing them at one site and not the other would emit debug tables pointing into the
/// middle of the method.
pub(crate) fn strip_param_assertions(ir: &mut IrFile) {
    for function in &mut ir.functions {
        function.param_checks.fill(None);
    }
    for class in &mut ir.classes {
        for parameter in &mut class.ctor_args {
            parameter.check = None;
        }
    }
}

/// Drop every `Intrinsics.checkNotNullExpressionValue` guard on a narrowed platform value.
///
/// `-Xno-call-assertions` removes the null checks kotlinc emits where a Java call's `T!` result is
/// committed to a declared non-null type. The guard is an expression wrapper, so it is removed by
/// rewriting the node into its operand's value rather than by clearing a record: `x!!`
/// ([`IrExpr::NotNullAssert`] with no message) is a SOURCE assertion the flag must leave alone.
pub(crate) fn strip_call_assertions(ir: &mut IrFile) {
    for expr in &mut ir.exprs {
        if let IrExpr::NotNullAssert {
            operand,
            message: Some(_),
        } = expr
        {
            *expr = IrExpr::Block {
                stmts: Vec::new(),
                value: Some(*operand),
            };
        }
    }
}

/// Per-file emission configuration passed explicitly down the emit callgraph and stamped onto every
/// `ClassWriter` (via [`new_writer`]) so synthetic serializer/companion/DefaultImpls classes inherit
/// it too. The `Default` is v52 with no `SourceFile`; every path that claims to emit the bytes krusty
/// SHIPS — the CLI, `survey`, the conformance corpus and the in-process test helpers — builds its
/// options through [`crate::jvm::backend::shipping_emit_options`] instead, which supplies the source
/// `.kt` name and the inner-class resolver (and, from the CLI, `-jvm-target`).
#[derive(Clone)]
pub struct EmitOptions {
    /// Class-file major version to emit (default v52; `-jvm-target 25` ⇒ v69).
    pub class_major: Option<u16>,
    /// Source-file simple name for the `SourceFile` attribute (e.g. `Foo.kt`); `None` ⇒ no attribute.
    pub source_file: Option<String>,
    /// `-module-name` value, recorded in each class's `@Metadata` (`classModuleName`). kotlinc omits it
    /// for the default module `main`; `None` here matches that.
    pub module_name: Option<String>,
    /// Emit a computed `@kotlin.Metadata` for supported class shapes ([`build_class_metadata`]).
    /// Byte-verified vs kotlinc for a plain `val`/`var`-property class and a `data class` (its IS_DATA
    /// flag + synthesized `componentN`/`copy`/`equals`/`hashCode`/`toString`); a shape that is not
    /// verified declines individually and emits no metadata, so this never writes an unverified
    /// payload (one did break kotlin-reflect on a box-corpus case). ON in this `Default`, and ON in
    /// [`crate::jvm::backend::shipping_emit_options`] — without it a krusty-compiled CLASS carries
    /// nothing a second krusty compilation can read (the facade metadata describes top-level
    /// declarations only). There is no `EmitOptions` value that means "the pre-class-metadata bytes"
    /// by default; a caller that wants those either sets `KRUSTY_NO_CLASS_METADATA` (which only the
    /// shipping constructor consults, for bisecting) or constructs `EmitOptions` explicitly with this
    /// field `false`.
    pub emit_class_metadata: bool,
    /// `-jvm-default`: the JVM shape of interface members with bodies. Not part of
    /// [`crate::jvm::backend::shipping_emit_options`]'s parameters because it is a per-INVOCATION
    /// compiler option rather than per-file configuration; the backend applies it with
    /// [`EmitOptions::with_jvm_default`].
    pub jvm_default: JvmDefaultMode,
    /// Emit the `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions` clears this).
    ///
    /// Most guards are recorded by lowering and removed from the IR before emission
    /// ([`strip_param_assertions`]), which keeps them consistent with the debug-table offsets
    /// measured past them. A PROPERTY SETTER's `<set-?>` guard has no IR record — it is derived here
    /// from the property's type — so those sites read this flag instead.
    pub param_assertions: bool,
    /// Independent `-Xlambdas` / `-Xsam-conversions` strategies for this invocation.
    pub lambda_modes: LambdaModes,
    pub inner_class_resolver: Option<InnerClassResolver>,
    /// The file's value classes, for the redundant-boxing pass; the emitter fills it per file.
    pub(crate) value_classes:
        std::rc::Rc<crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors>,
    /// `-java-parameters`: name each declared parameter in a `MethodParameters` attribute.
    pub java_parameters: bool,
}

impl EmitOptions {
    /// Select the `-jvm-default` mode, keeping every other field as configured.
    pub fn with_jvm_default(mut self, mode: JvmDefaultMode) -> Self {
        self.jvm_default = mode;
        self
    }

    /// Enable `-java-parameters`, keeping every other field as configured.
    pub fn with_java_parameters(mut self, enabled: bool) -> Self {
        self.java_parameters = enabled;
        self
    }

    /// Select the independently configured lambda strategies, keeping every other field unchanged.
    pub fn with_lambda_modes(mut self, modes: LambdaModes) -> Self {
        self.lambda_modes = modes;
        self
    }
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            class_major: None,
            source_file: None,
            module_name: None,
            emit_class_metadata: true,
            jvm_default: JvmDefaultMode::Enable,
            param_assertions: true,
            java_parameters: false,
            lambda_modes: LambdaModes::default(),
            inner_class_resolver: None,
            value_classes: std::rc::Rc::default(),
        }
    }
}

impl EmitOptions {
    /// `-Xno-param-assertions` passes `false`.
    pub fn with_param_assertions(mut self, enabled: bool) -> Self {
        self.param_assertions = enabled;
        self
    }
}

/// `Class.flags` (proto field 1) for any Kotlin class kind — ONE bitfield, not a per-kind constant.
/// Decoded from kotlinc 2.4.0 across every kind (plain 6, open 22, abstract 38, sealed 54, interface
/// 102, annotation 262, object 326, data 1030, value 8199, enum 32902):
///   bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3) | bits4-5 modality (FINAL0/OPEN1/ABSTRACT2/
///   SEALED3) | bits6-8 classKind (CLASS0/INTERFACE1/ENUM2/ENUM_ENTRY3/ANNOTATION4/OBJECT5/COMPANION6)
///   | bit10 isData | bit13 isValue | bit14 isFunInterface | bit15 hasEnumEntries.
/// The writer omits the field at [`DEFAULT_CLASS_FLAGS`] (a public final class).
/// Whether a realized property accessor consumes the receiver as an OPERAND. An instance accessor
/// always does. A STATIC one does not — a `@JvmStatic` object property's `setX(V)` takes the VALUE,
/// not a receiver — except on a `@JvmInline value class`, where every member is realized as a static
/// `-impl` whose FIRST parameter is the receiver's carrier (`kotlin/Result.isSuccess` is
/// `isSuccess-impl(Ljava/lang/Object;)Z`). Reading `!is_static` alone evaluated that receiver only for
/// effect and then invoked the static with an empty stack.
fn accessor_takes_receiver(access: &crate::jvm::inline::PropertyAccess) -> bool {
    use crate::jvm::inline::PropertyAccess;
    match access {
        PropertyAccess::Field { is_static, .. } => !is_static,
        PropertyAccess::Accessor {
            is_static,
            name,
            descriptor,
            ..
        } => {
            !is_static
                || crate::jvm::names::parse_method_descriptor(descriptor).is_some_and(
                    |(params, ret)| is_value_class_impl_accessor(name, params.len(), ret != "V"),
                )
        }
        // The receiver is the bridge's first ARGUMENT, so it is pushed like an ordinary receiver.
        PropertyAccess::AccessBridge { .. } => true,
    }
}

/// kotlinc's spelling for a `@JvmInline value class` member realized as a static over the carrier: the
/// Kotlin name with an `-impl` suffix (`isSuccess-impl`, `getLabel-impl`). It is the only static
/// accessor shape whose leading parameter is a receiver rather than a value.
///
/// `is_read` distinguishes the two sites, because the parameter COUNT is what separates a carrier from
/// a value: such a getter takes exactly the carrier, and such a setter the carrier AND the new value. A
/// `@JvmStatic` property whose name merely ends in `-impl` (reachable through `@JvmName`) therefore
/// cannot be mistaken for one — its static setter takes a single VALUE parameter.
fn is_value_class_impl_accessor(name: &str, params: usize, is_read: bool) -> bool {
    name.ends_with("-impl") && params == if is_read { 1 } else { 2 }
}

/// The type the receiver must hold ON THE STACK for `access`, given the property's `owner`.
///
/// Normally the owner itself. On a value class's static `-impl` accessor it is the accessor's first
/// DECLARED parameter — the carrier (`isSuccess-impl(Ljava/lang/Object;)Z` consumes the erased
/// underlying, never a `kotlin/Result` box). Narrowing an erased operand to the owner there emits a
/// `checkcast` no unboxed carrier can pass.
fn accessor_receiver_ty(access: &crate::jvm::inline::PropertyAccess, owner: &str) -> Ty {
    use crate::jvm::inline::PropertyAccess;
    if let PropertyAccess::Accessor {
        is_static: true,
        name,
        descriptor,
        ..
    } = access
    {
        if let Some((params, ret)) = crate::jvm::names::parse_method_descriptor(descriptor) {
            if is_value_class_impl_accessor(name, params.len(), ret != "V") {
                if let Some(carrier) = params.first() {
                    return crate::jvm::jvm_libraries::desc_to_ty(carrier);
                }
            }
        }
    }
    Ty::obj(owner)
}

fn class_metadata_flags(ir: &IrFile, c: &crate::ir::IrClass) -> u64 {
    // Visibility bits: INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3 — an `internal class` must
    // record explicit 0 so a consumer enforces the module boundary; synthesized classes without a
    // recorded visibility stay public.
    let visibility: u64 = match ir.class_visibilities.get(&c.fq_name_id()) {
        Some(crate::types::Visibility::Internal) => 0,
        Some(crate::types::Visibility::Private) => 1,
        Some(crate::types::Visibility::Protected) => 2,
        _ => 3,
    };
    let modality: u64 = if c.is_sealed {
        3
    } else if c.is_abstract || c.is_interface {
        2
    } else if c.is_open {
        1
    } else {
        0
    };
    let kind: u64 = if c.is_annotation {
        4
    } else if c.is_interface {
        1
    } else if !c.enum_entries.is_empty() {
        2
    } else if c.enum_entry_of.is_some() {
        3
    } else if c.is_companion {
        6
    } else if c.is_object {
        5
    } else {
        0
    };
    // A value class carries `@JvmInline`, which sets `hasAnnotations`.
    let has_annotations = u64::from(c.is_value || !c.applied_annotations.is_empty());
    has_annotations
        | (visibility << 1)
        | (modality << 4)
        | (kind << 6)
        // `IS_INNER` (bit 9): an `inner class` — the record is how a consumer knows construction
        // takes the enclosing instance (kotlinc: `inner class Item` flags 518).
        | (u64::from(c.is_inner_class) << 9)
        | (u64::from(c.is_data) << 10)
        | (u64::from(c.is_value) << 13)
        | (u64::from(c.is_fun_interface) << 14)
        | (u64::from(!c.enum_entries.is_empty()) << 15)
}

/// `Function.flags` (proto field 9) — ONE bitfield like [`class_metadata_flags`], not a per-shape
/// constant. Decoded from kotlinc 2.4.0 (copy 198, componentN 454, hashCode/toString 65750, equals
/// 66006): bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3, PRIVATE=1) | bits4-5 modality
/// (FINAL=0, OPEN=1, ABSTRACT=2) | bits6-7 memberKind (DECLARATION=0, SYNTHESIZED=3) | bit8
/// isOperator | bit9 isInfix.
/// Used for a class's REAL declared members; the data/value-class synthesized sets keep their own
/// (already kotlinc-verified) constants.
fn function_flags(ir: &IrFile, fid: u32, f: &crate::ir::IrFunction) -> u64 {
    let visibility: u64 = if ir.private_methods.contains(&fid) {
        1
    } else if ir.internal_methods.contains(&fid) {
        0 // INTERNAL — only metadata carries the module boundary
    } else {
        3
    };
    let modality: u64 = if f.body.is_none() {
        2 // abstract (an interface method or an `abstract fun`)
    } else if ir.open_methods.contains(&fid) {
        1
    } else {
        0
    };
    // `isOperator` (bit 8) — only `@Metadata` carries it; without it a consumer rejects the
    // conventional call form (`recv(args)`, `a[i]`) with "expression is not callable", and
    // convention resolution (`getValue`/`provideDelegate`/`invoke`) cannot filter on it.
    let operator = u64::from(ir.operator_fns.contains(&fid)) << 8;
    // `isInfix` (bit 9) — same metadata-only channel as `isOperator`: without it a consumer
    // rejects the `a f b` call form.
    let infix = u64::from(ir.infix_fns.contains(&fid)) << 9;
    // `isInline` (bit 10) is a Kotlin declaration capability, not a bytecode access flag. It must
    // survive class metadata so downstream frontends can select and splice member inline bodies.
    let inline = u64::from(ir.inline_fns.contains(&fid)) << 10;
    let return_value_status = ir.fn_return_value_statuses.get(&fid).map_or(0, |status| {
        status.metadata_value() << crate::metadata::function_flags::RETURN_VALUE_STATUS_SHIFT
    });
    (visibility << 1) | (modality << 4) | operator | infix | inline | return_value_status
}

/// The primary constructor's parameter descriptors. Only the LEADING `ctor_param_count` fields are
/// constructor parameters — a BODY property (`val y: Int = 2`) is a field but not a ctor argument.
/// The primary constructor's JVM descriptor: every constructor argument, property-backed or plain.
fn primary_ctor_descriptor(c: &IrClass) -> String {
    format!("({})V", primary_ctor_parameter_descs(c))
}

fn primary_ctor_parameter_descs(c: &IrClass) -> String {
    class_ctor_jvm_tys(c)
        .into_iter()
        .map(crate::jvm::names::type_descriptor)
        .collect()
}

fn ctor_field_descs(c: &IrClass) -> String {
    c.fields
        .iter()
        .take(c.ctor_param_count as usize)
        .map(|f| crate::jvm::names::type_descriptor(f.ty))
        .collect()
}

/// The TYPE PARAMETER a field is declared as (`class Pair<A, B>(val a: A)` → `a` is `A`), or `None` when
/// the field has a type of its own. `field_signatures` already tracks these — it is what drives their
/// `Signature` attribute. Callers ask so they can consult that parameter's BOUND: the erased descriptor
/// says nothing about whether the field can hold null.
fn type_parameter_field_name<'a>(ir: &'a IrFile, fq_name: &str, field: &str) -> Option<&'a str> {
    ir.field_signatures(fq_name)
        .and_then(|signatures| {
            signatures
                .iter()
                .find(|(name, _)| name == field)
                .map(|(_, parameter)| parameter.as_str())
        })
        .or_else(|| {
            let class = ir.class_id_by_name(crate::types::type_name(fq_name))?;
            ir.classes[class as usize]
                .fields
                .iter()
                .find(|candidate| candidate.name == field)
                .and_then(|candidate| candidate.type_param.as_deref())
        })
}

/// kotlinc's nullability classification for a class field / primary-constructor parameter: `0` = no
/// annotation (a primitive, or a type parameter that admits null), `1` = a non-null reference
/// (`@NotNull`, plus an `Intrinsics.checkNotNullParameter` guard wherever one applies), `2` = a nullable
/// reference (`@Nullable`, never guarded).
///
/// A field declared as a TYPE PARAMETER answers from that parameter's BOUND, not from the erased
/// descriptor: `<T : Cargo>`/`<T : Any>` cannot hold null and is `@NotNull`, while an unbounded `<T>`
/// (= `Any?`) or a `<T : Cargo?>` is left UNANNOTATED — kotlinc does not mark it `@Nullable`.
///
/// One predicate for the pool seeder, the field/accessor/parameter annotations, the `var` setter guard,
/// and the constructor's `LineNumberTable` start pc, because those must agree: classify a field as
/// guarded in one and unguarded in another and the line entry lands at the wrong offset.
fn field_nullability_kind(ir: &IrFile, fq_name: &str, name: &str, t: Ty) -> u8 {
    let d = crate::jvm::names::type_descriptor(t);
    if !(d.starts_with('L') || d.starts_with('[')) {
        return 0;
    }
    if matches!(t, Ty::PlatformNullable(_)) {
        0
    } else if let Some(parameter) = type_parameter_field_name(ir, fq_name, name) {
        u8::from(!ir.class_type_param_admits_null(fq_name, parameter))
    } else if matches!(t, Ty::Nullable(_)) {
        2
    } else {
        1
    }
}

/// Whether a field/constructor parameter is a NON-NULL reference — [`field_nullability_kind`] `== 1`.
fn is_nonnull_reference_field(ir: &IrFile, fq_name: &str, name: &str, t: Ty) -> bool {
    field_nullability_kind(ir, fq_name, name, t) == 1
}

/// Field indices a class `init_body` assigns a compile-time literal — a BODY property such as
/// `val y: Int = 2`. kotlinc sets `Property.hasConstant` for exactly these.
fn init_body_constant_fields(ir: &IrFile, c: &IrClass) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    let Some(body) = c.init_body else { return out };
    let IrExpr::Block { stmts, .. } = ir.expr(body) else {
        return out;
    };
    for &s in stmts {
        if let IrExpr::SetField { index, value, .. } = ir.expr(s) {
            if matches!(ir.expr(*value), IrExpr::Const(_)) {
                out.insert(*index);
            }
        }
    }
    out
}

/// Does `data` on this class synthesize the `componentN`/`copy` family? A `data object` is a SINGLETON:
/// kotlinc gives it `equals`/`hashCode`/`toString` ONLY — there is nothing to copy from and no
/// primary-constructor property to destructure. Both the constant-pool seeder and the `@Metadata`
/// builder ask this, so a data object cannot end up describing a `copy` its class file does not have.
fn synthesizes_data_class_members(c: &crate::ir::IrClass) -> bool {
    c.is_data && !c.is_singleton()
}

struct DataClassMemberRealization {
    jvm_name: Option<String>,
    descriptor: Option<String>,
}

/// Physical JVM realization of one exact common data-class declaration. The role was bound to its
/// [`crate::ir::FunId`] before backend renaming, so this reads the final function directly rather
/// than recovering it from a source/physical name pair.
fn data_class_member_realization(
    ir: &IrFile,
    owner: TypeName,
    role: IrDataClassMemberRole,
    source_name: &str,
    declared_params: &[Ty],
    declared_ret: Ty,
) -> Option<DataClassMemberRealization> {
    let function = &ir.functions[ir.data_class_member(owner, role)? as usize];
    let physical = ir_method_desc(&function.params, &function.ret);
    let declared = method_descriptor(declared_params, declared_ret);
    let has_boxed_primitive = std::iter::once(declared_ret)
        .chain(declared_params.iter().copied())
        .any(|ty| ty.is_nullable() && ty.non_null().is_jvm_scalar());
    Some(DataClassMemberRealization {
        jvm_name: (function.name != source_name).then(|| function.name.clone()),
        descriptor: (has_boxed_primitive || physical != declared).then_some(physical),
    })
}

/// The synthesized `copy` declaration's exact common-IR identity, if this class owns one.
fn data_copy_fid(ir: &IrFile, c: &crate::ir::IrClass) -> Option<u32> {
    ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::Copy)
}

/// `Function.flags` for the synthesized `copy`: [`COPY_FN_FLAGS`] (public final SYNTHESIZED member)
/// with the visibility bits swapped to the copy's actual visibility — the primary constructor's
/// under `DataClassCopyRespectsConstructorVisibility` (kotlinc 2.4.10: private ctor → 0xC2).
fn data_copy_fn_flags(ir: &IrFile, c: &crate::ir::IrClass) -> u64 {
    use crate::metadata::class_builder::COPY_FN_FLAGS;
    let Some(fid) = data_copy_fid(ir, c) else {
        return COPY_FN_FLAGS;
    };
    // INTERNAL=0, PRIVATE=1, PUBLIC=3 in metadata's visibility enum, held in bits 1-3.
    let visibility: u64 = if ir.private_methods.contains(&fid) {
        1
    } else if ir.internal_methods.contains(&fid) {
        0
    } else {
        3
    };
    (COPY_FN_FLAGS & !crate::metadata::property_flags::VISIBILITY_MASK) | (visibility << 1)
}

/// Compute a class's `@kotlin.Metadata` from its IR — WIRING [`crate::metadata::class_builder::build_class`]
/// into emission. Covers a class with a primary constructor of `val`/`var` properties plus real declared
/// members (emitted with derived [`function_flags`]), and the data/value-class synthesized sets. Returns
/// `None` for still-unsupported shapes (companion/annotation/enum-entry/secondary-ctors/…), so those
/// classes emit no `@Metadata` (unchanged). Broader shapes follow as `build_class` grows.
fn build_class_metadata(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
) -> Option<KotlinMetadata> {
    use crate::metadata::class_builder::{
        build_class, ClassMemberOrder, ClassTail, FnMeta, PropMeta, COMPONENT_FN_FLAGS,
        EQUALS_FN_FLAGS, FN_IS_SUSPEND, HASHCODE_TOSTRING_FN_FLAGS, OBJECT_CTOR_FLAGS,
        SEALED_CTOR_FLAGS,
    };
    if is_coroutine_state_machine(c) {
        return Some(KotlinMetadata {
            k: 3,
            mv: vec![2, 4, 0],
            xi: synthetic_class_xi(if is_continuation_class(c) {
                SYNTHETIC_PROTECTED
            } else {
                SYNTHETIC_LOCAL
            }),
            d1: vec![],
            d2: vec![],
        });
    }
    if !class_metadata_common_shape_admitted(ir, c) {
        return None;
    }
    // A `data class` also carries kotlinc's synthesized `componentN`/`copy`/`equals`/`hashCode`/
    // `toString` — derivable from the primary-ctor properties alone, so allowed alongside accessors.
    if c.is_value && !value_class_metadata_shape_admitted(ir, c) {
        return None;
    }
    // A value class's compiler-synthesized members (the static `-impl` family + their instance
    // delegators); allowed alongside the property accessor without disqualifying the shape.
    let value_method_names: std::collections::HashSet<String> = if c.is_value {
        [
            "equals",
            "hashCode",
            "toString",
            "equals-impl",
            "equals-impl0",
            "hashCode-impl",
            "toString-impl",
            "box-impl",
            "unbox-impl",
            "constructor-impl",
        ]
        .map(String::from)
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };
    let synthesizes_copy = synthesizes_data_class_members(c);
    // `data` synthesizes over the PRIMARY-CONSTRUCTOR properties only — `c.fields` also holds the
    // backing fields of body properties (`data class P(val x: Int) { val y = 1 }` has two fields but
    // one component). Counting all of them advertised a `component2` the class file does not define,
    // and a `copy(II)` where only `copy(I)` exists; real kotlinc reading that record accepts
    // `val (a, b) = p` and binds a method that is not there.
    let data_component_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
    let data_component_properties = if c.is_data {
        (0..data_component_fields.len())
            .map(|index| {
                c.properties
                    .iter()
                    .find(|property| property.backing_field == Some(index as u32))
            })
            .collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };
    // The only methods allowed in this bounded shape are the properties' own accessors (`getX`/`setX`)
    // plus a data class's synthesized set; any other real method is a shape not computed yet.
    // Accessor spellings are matched by name AND shape below, so the getter and setter names stay
    // in separate sets: a declared `operator fun getValue(thisRef, prop)` shares the JVM getter
    // name of a property called `value` but takes parameters no getter has — swallowing it by name
    // alone dropped its Function record from `@Metadata`, and a consumer could then not resolve
    // the delegate operator.
    // Accessor spellings map to the PROPERTY TYPE's descriptor: a declared function that merely
    // shares the getter name but has a different return (`val x: String` beside
    // `fun getX(): Int`) is a real Function record, not the accessor — the JVM holds both.
    let mut getter_names: std::collections::HashMap<String, String> = Default::default();
    let mut setter_names: std::collections::HashMap<String, String> = Default::default();
    for (name, ty) in c
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty))
        // A HOISTED companion property has no companion field, but its delegating accessors are
        // ordinary IR methods — they realize the Property record, never a Function one.
        .chain(
            c.properties
                .iter()
                .enumerate()
                .filter(|(property, p)| {
                    p.backing_field.is_none() && hoisted_static_for(ir, c, *property).is_some()
                })
                .map(|(_, p)| (p.name.as_str(), p.ty)),
        )
        // An INTERFACE property's accessor is a real default-method `IrFunction` (there is no
        // backing field to derive its name from), but it realizes the Property record — kotlinc
        // emits no Function entry for `getX` of `val x: Int get() = 1`, and a kotlinc consumer
        // reading both reports "inherited platform declarations clash" on every implementer.
        .chain(
            c.is_interface
                .then(|| c.properties.iter().map(|p| (p.name.as_str(), p.ty)))
                .into_iter()
                .flatten(),
        )
    {
        let (getter, setter) = accessor_jvm_names(c, name);
        let descriptor = crate::jvm::names::type_descriptor(jvm_declared_ty(&ty));
        getter_names.insert(getter, descriptor.clone());
        setter_names.insert(setter, descriptor);
    }
    // Member-extension-PROPERTY accessors are described as `Property` records (below), never as
    // functions — kotlinc emits no `Function` record for `getDoubled` of `val Int.doubled`.
    let ext_prop_accessor_fids: std::collections::HashSet<u32> = ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .flat_map(|prop| std::iter::once(prop.getter).chain(prop.setter))
        .collect();
    let property_accessor_fids: std::collections::HashSet<u32> = c
        .properties
        .iter()
        .flat_map(|property| property.getter.into_iter().chain(property.setter))
        .collect();
    let source_callable_fids = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    // Metadata Function records come only from exact source-callable realizations. Accessors have
    // Property records, while generated declarations use the explicit publication contract below.
    let mut declared_fids: Vec<u32> = c
        .methods
        .iter()
        .copied()
        .filter(|&fid| {
            // A physical class method is not necessarily a Kotlin declaration. Lifted local
            // functions and interface-delegation forwarders are implementation methods and have
            // no Function entry in class metadata. Common lowering publishes the exact source
            // callable -> function edge, so metadata consumes that identity instead of inferring
            // declaration status from a name, descriptor, or parameter spelling.
            if !source_callable_fids.contains(&fid) {
                return false;
            }
            if ir.lambda_own_params_from.contains_key(&fid) || ir.synthetic_methods.contains(&fid) {
                return false;
            }
            if ext_prop_accessor_fids.contains(&fid) {
                return false;
            }
            if property_accessor_fids.contains(&fid) {
                return false;
            }
            let function = &ir.functions[fid as usize];
            let n = &function.name;
            let accessor_shaped = (function.params.is_empty()
                && getter_names.get(n).is_some_and(|descriptor| {
                    crate::jvm::names::type_descriptor(jvm_declared_ty(&function.ret))
                        == *descriptor
                }))
                || (function.params.len() == 1
                    && matches!(function.ret, Ty::Unit)
                    && setter_names.get(n).is_some_and(|descriptor| {
                        crate::jvm::names::type_descriptor(jvm_declared_ty(&function.params[0]))
                            == *descriptor
                    }));
            !accessor_shaped
                && !ir.is_data_class_member(c.fq_name_id(), fid)
                && !value_method_names.contains(n)
        })
        .collect();
    declared_fids.sort_by_key(|fid| ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX));
    let generated_publication = ir.generated_member_publication(c.fq_name_id());
    // A VALUE-CLASS-INVOLVED MEMBER is now DESCRIBED. The writer could always produce kotlinc's exact
    // payload for one (the byte-identity tests proved it); what was missing was the READ half, and the
    // classpath value-class RETURN model supplies it — `MetadataCallFacts::value_class_ret` reports
    // that the physical method already hands back the ERASED underlying, so a caller that learns the
    // Kotlin return `K` from `@Metadata` no longer also emits kotlinc's boxed sequence (`invokevirtual
    // I.f-XLNMDGE()Ljava/lang/String; checkcast K; K.unbox-impl()`) over a `String` that IS the
    // carrier. Round-tripped by `krusty_roundtrip_class_metadata_e2e`'s value-class cases (each RUNS
    // `box()`) and pinned by the box corpus's `compileKotlinAgainstKotlin/inlineClasses/*` MODULE
    // chains.
    //
    // An `Object`-erased value-class member is therefore described too. The metadata-selected
    // dependency declaration carries semantic and physical parameter/result shapes independently,
    // so a generic-underlying value class (`kotlin/Result`) no longer requires guessing from its
    // `Object` descriptor. General class/value-class shape admission is centralized below in
    // `class_metadata_common_shape_admitted` / `value_class_metadata_shape_admitted`.
    //
    // …and a class cannot be described in terms of a value class a downstream compilation cannot READ
    // as one (`value_class_is_readable`): it would see an ordinary class, cast the carrier to the box
    // and bind an instance accessor where kotlinc emits the static `-impl` — a ClassCastException.
    // Describing `Holder.make(): A` is only sound once `A` itself is described.
    let mentions_undescribed_value_class = |t: &Ty| {
        t.non_null().obj_internal().is_some_and(|fq_name| {
            // Same-file and classpath declarations are in the unified lookup. A sibling source
            // declaration is deliberately not materialized into this file's IR, so the module-origin
            // subset is also positive identity for that one case; it is not a second underlying map.
            (ir.is_value_class_name(fq_name) || ir.module_source_value_classes.contains(&fq_name))
                && !value_class_is_readable(ir, fq_name)
        })
    };
    if declared_fids
        .iter()
        .copied()
        .chain(
            generated_publication
                .into_iter()
                .flat_map(|publication| publication.functions.iter())
                .filter(|member| member.metadata.is_some())
                .map(|member| member.function),
        )
        .any(|fid| {
            ir.vc_declared_sigs
                .get(&fid)
                .is_some_and(|(_, params, ret)| {
                    params
                        .iter()
                        .chain(std::iter::once(ret))
                        .any(mentions_undescribed_value_class)
                })
        })
        || c.properties
            .iter()
            .any(|p| p.getter_jvm_name.is_some() && mentions_undescribed_value_class(&p.ty))
    {
        return None;
    }
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let const_fields = init_body_constant_fields(ir, c);
    // Metadata describes Kotlin PROPERTY declarations, never physical fields. Synthetic storage such
    // as `x$delegate`, `this$0`, and interface-delegation fields has no source declaration and must not
    // leak into the metadata name/type namespace. A property's optional backing field supplies only
    // its JVM realization (descriptor, constant, and accessor descriptor).
    let mut declared_props: Vec<(u32, PropMeta)> = c
        .properties
        .iter()
        .enumerate()
        .map(|(property_index, property)| {
            crate::trace_compiler!(
                "metadata",
                "emit class metadata property owner={:?} name={} ty={:?} context={:?}",
                c.fq_name,
                property.name,
                property.ty,
                property.context_params,
            );
            let visibility = property.visibility;
            let backing = property
                .backing_field
                .and_then(|index| c.fields.get(index as usize).map(|field| (index, field)));
            let (default_getter, default_setter) = accessor_jvm_names(c, &property.name);
            let ordinary_getter = property
                .getter
                .and_then(|fid| ir.functions.get(fid as usize))
                .map(|function| {
                    (
                        function.name.clone(),
                        ir_method_desc(&function.params, &function.ret),
                    )
                })
                .or_else(|| {
                    c.methods
                        .iter()
                        .map(|fid| &ir.functions[*fid as usize])
                        .find(|function| function.name == default_getter)
                        .map(|function| {
                            (
                                function.name.clone(),
                                ir_method_desc(&function.params, &function.ret),
                            )
                        })
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        // `@JvmField` suppresses the accessor pair entirely, so there is no
                        // synthesized getter to derive from the backing field — kotlinc records the
                        // field alone.
                        (!visibility.is_private() && !is_jvm_field(c, &property.name))
                            .then(|| (default_getter, format!("(){}", desc(field.ty))))
                    })
                });
            let getter = if c.is_annotation {
                Some((property.name.clone(), format!("(){}", desc(property.ty))))
            } else {
                ordinary_getter
            };
            let setter = property
                .setter
                .and_then(|fid| ir.functions.get(fid as usize))
                .map(|function| {
                    (
                        function.name.clone(),
                        ir_method_desc(&function.params, &function.ret),
                    )
                })
                .or_else(|| {
                    c.methods
                        .iter()
                        .map(|fid| &ir.functions[*fid as usize])
                        .find(|function| function.name == default_setter)
                        .map(|function| {
                            (
                                function.name.clone(),
                                ir_method_desc(&function.params, &function.ret),
                            )
                        })
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        (!visibility.is_private()
                            && property.is_var
                            && !is_jvm_field(c, &property.name))
                        .then(|| (default_setter, format!("({})V", desc(field.ty))))
                    })
                });
            // A delegated property's JVM field is its `x$delegate` storage, which metadata names.
            let delegate = property
                .delegate_field
                .and_then(|i| c.fields.get(i as usize));
            (
                property.source_order,
                PropMeta {
                    return_value_status: property.return_value_status,
                    spellings: ir
                        .prop_declared_spellings
                        .get(&(c.fq_name_id(), property.name.clone()))
                        .cloned()
                        .unwrap_or_default(),
                    name: property.name.clone(),
                    ty: property.ty,
                    context_params: property.context_params.clone(),
                    is_var: property.is_var,
                    visibility,
                    // A HOISTED companion property still records a (derived) backing field — the field
                    // exists, on the outer class — and a literal-initialized `val` keeps kotlinc's
                    // HAS_CONSTANT flag exactly like an instance-field one.
                    has_constant: backing.is_some_and(|(index, field)| {
                        field.is_final()
                            && index >= c.ctor_param_count
                            && const_fields.contains(&index)
                    }) || hoisted_static_for(ir, c, property_index).is_some_and(
                        |s| !s.is_var && static_fields::const_value_idx_peek(ir, s.init),
                    ),
                    is_const: false,
                    modifiers: property.modifiers,
                    setter_is_private: property.setter_is_private,
                    has_backing_field: !c.is_annotation
                        && (backing.is_some()
                            || delegate.is_some()
                            || hoisted_static_for(ir, c, property_index).is_some())
                        && !c.is_interface,
                    tparam: match property.ty {
                        Ty::TyParam(name, _) => Some(name),
                        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => match *inner {
                            Ty::TyParam(name, _) => Some(name),
                            _ => None,
                        },
                        _ => None,
                    }
                    .and_then(|parameter| {
                        let parameter = crate::types::type_parameter_source_name(parameter);
                        c.type_params
                            .iter()
                            .position(|candidate| candidate == parameter)
                    })
                    .or_else(|| {
                        ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                            signatures
                                .iter()
                                .find(|(field, _)| field == &property.name)
                                .and_then(|(_, parameter)| {
                                    c.type_params
                                        .iter()
                                        .position(|candidate| candidate == parameter)
                                })
                        })
                    })
                    .map(|index| index as u32),
                    receiver: None,
                    type_params: Vec::new(),
                    getter,
                    setter,
                    setter_parameter_name: super::parameter_names::explicit_setter(
                        ir,
                        property.setter,
                    ),
                    field_desc: backing
                        .map(|(_, field)| field)
                        .or(delegate)
                        .filter(|field| property.ty != field.ty)
                        .map(|field| desc(field.ty)),
                    // The PHYSICAL field name when the JVM realization mangles it — an instance
                    // property beside a same-named hoisted companion static (`result` → `result$1`).
                    field_name: backing
                        .map(|(_, field)| field)
                        .or(delegate)
                        .map(|field| instance_field_jvm_name(ir, c, field))
                        .filter(|physical| *physical != property.name),
                    // A property-targeted annotation lives on its synthetic marker method; the
                    // record here is what connects the property to it (and the marker's FINAL name,
                    // which the value-class pass may have mangled with the getter's).
                    annotations: property_marker_annotations(ir, c, &property.name),
                    field_annotations: property_backing_field_annotations(c, &property.name),
                    synthetic_method: property_marker_signature(ir, c, &property.name),
                    // kotlinc marks an interface companion's `@JvmField` property record: the
                    // backing field was MOVED onto the interface itself.
                    moved_from_interface_companion: companion_of_interface(ir, c)
                        && jvm_field_static_for(ir, c, property_index),
                },
            )
        })
        .collect();
    // A class's own `const val`s (`declared_class_statics`) enter here — BEFORE the extension
    // properties — and the whole set then sorts by each declaration's exact source offset (kotlinc's
    // metadata property order). The key stays attached to the declaration across JVM storage moves.
    for &static_id in ir
        .declared_class_statics
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
    {
        let prop = &ir.statics[static_id as usize];
        declared_props.push((
            prop.source_order,
            PropMeta {
                return_value_status: Default::default(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: prop.name.clone(),
                ty: prop.ty,
                context_params: Vec::new(),
                is_var: prop.is_var,
                visibility: prop.visibility,
                has_constant: true,
                is_const: true,
                modifiers: Default::default(),
                setter_is_private: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                type_params: Vec::new(),
                getter: None,
                setter: None,
                setter_parameter_name: None,
                field_desc: None,
                field_name: None,
                annotations: property_marker_annotations(ir, c, &prop.name),
                field_annotations: property_backing_field_annotations(c, &prop.name),
                synthetic_method: property_marker_signature(ir, c, &prop.name),
                moved_from_interface_companion: false,
            },
        ));
    }
    declared_props.sort_by_key(|(line, _)| *line);
    let mut prop_source_orders: Vec<u32> = declared_props.iter().map(|(order, _)| *order).collect();
    let mut props: Vec<PropMeta> = declared_props
        .into_iter()
        .map(|(_, property)| property)
        .collect();
    // Member EXTENSION properties: a `Property` record with `receiver_type` and the accessor
    // signatures — the declaration the accessor methods (excluded from `declared_fids`) realize.
    for ext in ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
    {
        let accessor_sig = |fid: u32| {
            ir.functions.get(fid as usize).map(|function| {
                (
                    function.name.clone(),
                    ir_method_desc(&function.params, &function.ret),
                )
            })
        };
        let ext_delegate = ext.delegate_field.and_then(|i| c.fields.get(i as usize));
        props.push(PropMeta {
            return_value_status: Default::default(),
            spellings: ir
                .prop_declared_spellings
                .get(&(c.fq_name_id(), ext.name.clone()))
                .cloned()
                .unwrap_or_default(),
            name: ext.name.clone(),
            ty: ext.ty,
            context_params: Vec::new(),
            is_var: ext.is_var,
            visibility: ext.visibility,
            has_constant: false,
            is_const: false,
            modifiers: ext.modifiers,
            setter_is_private: false,
            has_backing_field: ext_delegate.is_some(),
            tparam: None,
            receiver: Some(ext.receiver),
            type_params: ext.type_params.clone(),
            getter: accessor_sig(ext.getter),
            setter: ext.setter.and_then(accessor_sig),
            setter_parameter_name: super::parameter_names::explicit_setter(ir, ext.setter),
            field_desc: ext_delegate
                .filter(|field| field.ty != ext.ty)
                .map(|field| desc(field.ty)),
            field_name: ext_delegate.map(|field| instance_field_jvm_name(ir, c, field)),
            annotations: property_marker_annotations(ir, c, &ext.name),
            field_annotations: Vec::new(),
            synthetic_method: property_marker_signature(ir, c, &ext.name),
            moved_from_interface_companion: false,
        });
        prop_source_orders.push(
            ir.fn_source_order
                .get(&ext.getter)
                .copied()
                .unwrap_or(u32::MAX),
        );
    }
    let named_ctor_args: Vec<(String, Ty, bool, Option<u32>)> = c
        .ctor_args
        .iter()
        .filter_map(|arg| {
            arg.name.as_ref().map(|name| {
                (
                    name.clone(),
                    arg.declared_ty.unwrap_or(arg.ty),
                    arg.has_default,
                    arg.type_param,
                )
            })
        })
        .collect();
    // Parallel to `named_ctor_args` (hence the SAME unnamed-parameter filter): the class's
    // `ctor_param_annotations` covers every `ctor_args` entry, including the synthetic unnamed ones a
    // metadata constructor record never lists.
    let named_ctor_param_annotations: Vec<Vec<crate::ir::AppliedAnnotation>> = c
        .ctor_args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.name.is_some())
        .map(|(i, _)| {
            c.ctor_param_annotations
                .get(i)
                .map(|anns| anns.iter().map(|a| a.annotation.clone()).collect())
                .unwrap_or_default()
        })
        .collect();
    let ctor_params_with_defaults = if c.ctor_args.is_empty() {
        c.fields
            .iter()
            .take(c.ctor_param_count as usize)
            .map(|field| {
                let type_param = ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                    signatures
                        .iter()
                        .find(|(name, _)| name == &field.name)
                        .and_then(|(_, type_param)| {
                            c.type_params
                                .iter()
                                .position(|candidate| candidate == type_param)
                        })
                        .map(|index| index as u32)
                });
                (
                    field.name.clone(),
                    field.ty,
                    field.has_default(),
                    type_param,
                )
            })
            .collect()
    } else {
        // `ctor_args` may contain only the unnamed enclosing-instance parameter of an `inner`
        // class. That parameter belongs in the JVM descriptor below, never in Kotlin metadata's
        // source value-parameter list; an empty `named_ctor_args` is therefore authoritative.
        named_ctor_args
    };
    // Position of a `vararg` primary-ctor parameter within the NAMED parameter list (the same
    // filtered order `ctor_params` uses) — `None` when unnamed-args fallback is in effect.
    let ctor_vararg_index = c
        .ctor_args
        .iter()
        .filter(|arg| arg.name.is_some())
        .position(|arg| arg.is_vararg);
    let ctor_params: Vec<(String, Ty)> = ctor_params_with_defaults
        .iter()
        .map(|(name, ty, _, _)| (name.clone(), *ty))
        .collect();
    let ctor_param_defaults: Vec<bool> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, has_default, _)| *has_default)
        .collect();
    let ctor_param_tparams: Vec<Option<u32>> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, _, type_param)| *type_param)
        .collect();
    // An `enum class`'s JVM constructor takes the two synthetic `Enum` parameters first, so its
    // recorded `JvmMethodSignature` is `(Ljava/lang/String;I…)V` — the metadata names the REAL
    // descriptor even though those parameters are not Kotlin-visible.
    let ctor_desc = format!(
        "({}{}{})V",
        if c.enum_entries.is_empty() {
            ""
        } else {
            "Ljava/lang/String;I"
        },
        // The physical `<init>` leads with the UNNAMED lowering-added parameters — an inner class's
        // enclosing instance (`Llib/Outer;`) — which `ctor_params` (source parameters) never carry.
        // kotlinc's record spells them (`(Llib/Outer;Ljava/lang/String;I)V`); without them a
        // consumer's constructor call is one slot short.
        c.ctor_args
            .iter()
            .filter(|arg| arg.name.is_none())
            .map(|arg| desc(arg.ty))
            .collect::<String>(),
        ctor_params
            .iter()
            .map(|(_, t)| desc(*t))
            .collect::<String>()
    );
    // A value-class-parametered primary ctor: the record names the DECLARED types (`id: ItemId` —
    // the erase pass rewrote `ctor_args` to the underlying), and its physical handle is the PUBLIC
    // synthetic marker ctor (`(…;Lkotlin/jvm/internal/DefaultConstructorMarker;)V`) — the private
    // erased `<init>` is not callable cross-class. Both exactly as kotlinc records them.
    let (ctor_params, ctor_desc) = match ir.vc_ctor_declared_params(c.fq_name_id()) {
        Some(declared) if c.enum_entries.is_empty() => {
            let named_declared: Vec<Ty> = c
                .ctor_args
                .iter()
                .zip(declared)
                .filter(|(arg, _)| arg.name.is_some())
                .map(|(_, ty)| *ty)
                .collect();
            let params = if named_declared.len() == ctor_params.len() {
                ctor_params
                    .iter()
                    .zip(&named_declared)
                    .map(|((name, _), ty)| (name.clone(), *ty))
                    .collect()
            } else {
                ctor_params
            };
            // The physical marker ctor spells EVERY parameter — an inner class's leading outer
            // instance included (`ctor_params` above holds only the NAMED source parameters).
            let desc = format!(
                "({}Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                c.ctor_args
                    .iter()
                    .map(|arg| desc(arg.ty))
                    .collect::<String>()
            );
            (params, desc)
        }
        _ => (ctor_params, ctor_desc),
    };
    // kotlinc's synthesized data-class methods, in declaration order: componentN, copy, equals,
    // hashCode, toString. Their shapes come entirely from the primary-ctor properties.
    let declared_methods = || {
        declared_fids
            .iter()
            .filter_map(|&fid| {
                let f = ir.functions.get(fid as usize)?;
                // Real parameter identities — metadata is reflection-visible, so a placeholder
                // would be an observable lie. A missing semantic name is rejected below.
                let parameter_identities = ir.function_parameter_identities(fid);
                // A function is described as SOURCE declared it: its own name, parameters and return
                // type. Two lowerings hide that — CPS gives a `suspend fun` a trailing `Continuation`
                // and an `Object` return, and the value-class pass mangles the name and erases the
                // value classes away. Prefer the value-class record when both applied: it ran first,
                // so it holds the fully declared form. What the JVM method actually looks like rides
                // along as a `JvmMethodSignature` (name only when mangling changed it).
                let is_suspend = ir.suspend_declared_sigs.contains_key(&fid);
                let vc = ir.vc_declared_sigs.get(&fid);
                let declared = vc
                    .map(|(n, p, r)| (n.as_str(), p.as_slice(), *r))
                    .or_else(|| {
                        ir.suspend_declared_sigs
                            .get(&fid)
                            .map(|(p, r)| (f.name.as_str(), p.as_slice(), *r))
                    });
                let (_, params, ret) =
                    declared.unwrap_or((f.name.as_str(), f.params.as_slice(), f.ret));
                let name = ir
                    .fn_source_names
                    .get(&fid)
                    .map(String::as_str)
                    .expect("a source metadata function retains its declaration name");
                let semantic_signature = ir.signatures.get(&fid);
                // A member mentioning an ENCLOSING-CLASS type parameter records its semantic shape
                // separately (`member_semantic_sigs`) — the erased params would publish `Any`.
                let member_semantic = ir
                    .member_semantic_sigs
                    .get(&fid)
                    .filter(|_| !ir.extension_receiver_fns.contains(&fid));
                let metadata_params = semantic_signature
                    .map(|signature| signature.params.as_slice())
                    .or(member_semantic.map(|(params, _)| params.as_slice()))
                    .unwrap_or(params);
                let metadata_ret = semantic_signature
                    .and_then(|signature| signature.ret)
                    .or(member_semantic.map(|(_, ret)| *ret))
                    .unwrap_or(ret);
                // A parameter's declared `?` lives in a side-table, not in `params` (which stays
                // non-null so the value-class mangle is undisturbed) — re-apply it for `@Metadata`.
                let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
                let function_type_params = ir
                    .signatures
                    .get(&fid)
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let semantic_function_type_params = semantic_signature
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| parameter.semantic_name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let function_type_param_bounds = semantic_signature
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| {
                                parameter.bounds.iter().map(|(bound, _)| *bound).collect()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // Restore an extension receiver to `Function.receiver_type` so metadata keeps only
                // the declaration's logical value parameters. A value-class member's static
                // `-impl` has one backend-generated carrier before that complete declaration list;
                // the exact representation marker supplies that offset. Source-owned side tables
                // remain indexed by the declaration list and never acquire the carrier slot.
                let member_context_count = ir.fn_context_counts.get(&fid).copied().unwrap_or(0);
                let is_ext = ir.extension_receiver_fns.contains(&fid)
                    && member_context_count < metadata_params.len();
                let receiver_index = is_ext.then_some(member_context_count);
                let backend_parameter_prefix =
                    usize::from(ir.jvm_value_class_receiver_impls.contains(&fid));
                let parameter_identities = parameter_identities
                    .expect("a metadata function retains exact parameter identities");
                assert!(
                    backend_parameter_prefix + metadata_params.len() <= parameter_identities.len(),
                    "metadata declaration parameters retain their exact physical identities"
                );
                let apply_nullable = |source_index: usize, t: crate::types::Ty| {
                    if declared_nullable
                        .and_then(|v| v.get(source_index))
                        .copied()
                        .unwrap_or(false)
                    {
                        crate::types::Ty::nullable(t)
                    } else {
                        t
                    }
                };
                let receiver =
                    receiver_index.map(|index| apply_nullable(index, metadata_params[index]));
                let logical_param_indices = (0..metadata_params.len())
                    .filter_map(|index| {
                        if receiver_index == Some(index) {
                            None
                        } else {
                            Some((index, backend_parameter_prefix + index))
                        }
                    })
                    .collect::<Vec<_>>();
                let logical_params: Vec<(String, crate::types::Ty)> = logical_param_indices
                    .iter()
                    .map(|&(metadata_index, physical_index)| {
                        let identity = parameter_identities
                            .get(physical_index)
                            .expect("a metadata parameter carries an exact identity");
                        let n = if matches!(
                            identity.role,
                            crate::ir::IrParameterRole::ContextReceiver { .. }
                        ) {
                            String::new()
                        } else {
                            crate::jvm::parameter_names::metadata(identity)
                                .map(str::to_owned)
                                .expect("a metadata value parameter carries a semantic name")
                        };
                        (
                            n,
                            apply_nullable(metadata_index, metadata_params[metadata_index]),
                        )
                    })
                    .collect();
                let context_parameter_kinds = parameter_identities
                    .iter()
                    .skip(backend_parameter_prefix)
                    .take(member_context_count)
                    .map(crate::jvm::parameter_names::metadata_context_kind)
                    .collect();
                // Per-parameter DECLARES_DEFAULT_VALUE — recorded so a cross-module caller may
                // OMIT a defaulted member argument (the `$default` synthetic realizes the call).
                let param_defaults: Vec<bool> = ir
                    .param_defaults(fid)
                    .map(|ds| {
                        ds.iter()
                            .enumerate()
                            .filter(|(i, _)| receiver_index != Some(*i))
                            .map(|(_, d)| d.is_some())
                            .collect()
                    })
                    .unwrap_or_default();
                Some(FnMeta {
                    // How SOURCE spelled this member's declared types — carried on the IR because
                    // class metadata is built without the AST (see `IrFile::fn_declared_spellings`).
                    spellings: ir
                        .fn_declared_spellings
                        .get(&fid)
                        .cloned()
                        .unwrap_or_default(),
                    name: name.to_string(),
                    params: logical_params,
                    ret: metadata_ret,
                    receiver,
                    type_params: function_type_params,
                    semantic_type_params: semantic_function_type_params,
                    type_param_bounds: function_type_param_bounds,
                    flags: function_flags(ir, fid, f) | if is_suspend { FN_IS_SUSPEND } else { 0 },
                    params_have_defaults: false,
                    param_defaults,
                    vararg_index: ir.fn_vararg_index.get(&fid).copied(),
                    context_count: member_context_count,
                    context_parameter_kinds,
                    // The physical descriptor rides along whenever a reader could not derive it from
                    // the proto types: a VC/suspend-rewritten member (`declared`), a signature
                    // mentioning a TYPE PARAMETER (`vararg parts: T` erases to `[Ljava/lang/Object;`
                    // — nothing in the record names that), a vararg (kotlinc records it there too),
                    // or a `kotlin/Array` anywhere in the signature, which a name-keyed table cannot
                    // map because the descriptor depends on the type ARGUMENT. That last one is the
                    // same rule the facade path applies, and the same predicate states it.
                    // Derivable signatures omit it, kotlinc's usual shape. A value-class rewrite
                    // that only MANGLED the name still has a derivable descriptor when no erasure
                    // happened (`f(): V?` stays `()LI$V;` — nullable value classes box), so kotlinc
                    // records just the name there; an erased shape (`h(): V` → `()I`) is not
                    // derivable and keeps the descriptor.
                    jvm_sig: ((declared.is_some()
                        && (is_suspend
                            || !vc.is_some_and(|(_, p, r)| {
                                crate::jvm::names::method_descriptor(p, *r)
                                    == crate::jvm::names::method_descriptor(&f.params, f.ret)
                            })))
                        || ir.fn_vararg_index.contains_key(&fid)
                        || matches!(metadata_ret, crate::types::Ty::TyParam(..))
                        || crate::metadata::descriptor_needs_recording(metadata_ret)
                        || metadata_params.iter().any(|parameter| {
                            matches!(parameter, crate::types::Ty::TyParam(..))
                                || crate::metadata::descriptor_needs_recording(*parameter)
                        }))
                    .then(|| crate::jvm::names::method_descriptor(&f.params, f.ret)),
                    jvm_sig_name: (name != f.name).then(|| f.name.clone()),
                    // The declaration's own annotations, mirrored into `@Metadata`. Retention split
                    // the two class-file attributes apart (`RuntimeVisible`/`RuntimeInvisible`); the
                    // metadata record keeps ONE list, so they are rejoined here — SOURCE-retained
                    // annotations were dropped during lowering and never reach either.
                    annotations: ir
                        .function_annotations
                        .get(&fid)
                        .map(|annotations| annotations.applications().cloned().collect())
                        .unwrap_or_default(),
                    // `metadata_params` is the DECLARED parameter list (a suspend fn's synthesized
                    // `Continuation` is already dropped), so the side table lines up with it.
                    param_annotations: logical_param_indices
                        .iter()
                        .map(|&(metadata_index, _)| {
                            ir.fn_param_annotations
                                .get(&fid)
                                .and_then(|table| table.get(metadata_index))
                                .map(|annotations| {
                                    annotations
                                        .iter()
                                        .map(|annotation| annotation.annotation.clone())
                                        .collect()
                                })
                                .unwrap_or_default()
                        })
                        .collect(),
                    no_infer_params: logical_param_indices
                        .iter()
                        .map(|&(metadata_index, _)| {
                            ir.fn_param_no_infer
                                .get(&fid)
                                .and_then(|flags| flags.get(metadata_index))
                                .copied()
                                .unwrap_or(false)
                        })
                        .collect(),
                })
            })
            .collect::<Vec<_>>()
    };
    let class_ty = Ty::obj(&c.fq_name());
    let inferred_methods: Vec<FnMeta> = if c.is_data {
        let mut m = Vec::new();
        for (i, property) in data_component_properties.iter().enumerate() {
            let name = format!("component{}", i + 1);
            let realization = data_class_member_realization(
                ir,
                c.fq_name_id(),
                IrDataClassMemberRole::Component(i as u32),
                &name,
                &[],
                property.ty,
            )?;
            m.push(FnMeta {
                jvm_sig_name: realization.jvm_name,
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name,
                params: vec![],
                ret: property.ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: realization.descriptor,
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if synthesizes_copy {
            let declared = data_component_properties
                .iter()
                .map(|property| property.ty)
                .collect::<Vec<_>>();
            let realization = data_class_member_realization(
                ir,
                c.fq_name_id(),
                IrDataClassMemberRole::Copy,
                "copy",
                &declared,
                class_ty,
            )?;
            m.push(FnMeta {
                jvm_sig_name: realization.jvm_name,
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "copy".into(),
                params: data_component_properties
                    .iter()
                    .map(|property| (property.name.clone(), property.ty))
                    .collect(),
                ret: class_ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: data_copy_fn_flags(ir, c),
                params_have_defaults: true,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: realization.descriptor,
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::Equals)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "equals".into(),
                params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
                ret: Ty::Boolean,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::HashCode)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::ToString)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        m.extend(declared_methods());
        m
    } else if c.is_value {
        // A value class's Kotlin-visible overrides. Each dispatches to a differently-named static
        // `-impl` taking the erased underlying, so each records a `JvmMethodSignature` (name + desc).
        let u = desc(c.fields[0].ty);
        let mut methods = vec![
            FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "equals".into(),
                params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
                ret: Ty::Boolean,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: Some(format!("({u}Ljava/lang/Object;)Z")),
                jvm_sig_name: Some("equals-impl".into()),
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            },
            FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: Some(format!("({u})I")),
                jvm_sig_name: Some("hashCode-impl".into()),
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            },
            FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: Some(format!("({u})Ljava/lang/String;")),
                jvm_sig_name: Some("toString-impl".into()),
                annotations: Vec::new(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            },
        ];
        methods.extend(declared_methods());
        methods
    } else {
        declared_methods()
    };
    let mut methods = match generated_publication.map(|publication| publication.metadata_scope) {
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Exclusive) => Vec::new(),
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Additive) | None => inferred_methods,
    };
    let inferred_method_count = methods.len();
    if let Some(publication) = generated_publication {
        methods.extend(super::generated_member_metadata::functions(ir, publication));
    }
    let type_aliases = ir
        .class_type_aliases
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .map(|alias| crate::metadata::builder::TypeAliasMeta {
            name: alias.name.clone(),
            formals: alias.formals.clone(),
            expansion: alias.expansion,
            visibility: alias.visibility,
            expansion_spelling: alias.expansion_spelling.clone(),
            decl_order: alias.source_order as usize,
        })
        .collect::<Vec<_>>();
    let member_order = if c.is_data || c.is_value {
        Vec::new()
    } else {
        let mut ordered = Vec::with_capacity(props.len() + methods.len() + type_aliases.len());
        ordered.extend(
            prop_source_orders
                .iter()
                .copied()
                .enumerate()
                .map(|(index, order)| (order, ClassMemberOrder::Property(index))),
        );
        if !matches!(
            generated_publication.map(|publication| publication.metadata_scope),
            Some(crate::ir::IrGeneratedFunctionMetadataScope::Exclusive)
        ) {
            ordered.extend(
                declared_fids
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, fid)| {
                        (
                            ir.fn_source_order.get(&fid).copied().unwrap_or(u32::MAX),
                            ClassMemberOrder::Function(index),
                        )
                    }),
            );
        }
        let generated_order_base = if matches!(
            generated_publication.map(|publication| publication.metadata_scope),
            Some(crate::ir::IrGeneratedFunctionMetadataScope::Exclusive)
        ) {
            0
        } else {
            ordered
                .iter()
                .map(|(order, _)| *order)
                .chain(
                    type_aliases
                        .iter()
                        .map(|alias| u32::try_from(alias.decl_order).unwrap_or(u32::MAX)),
                )
                .filter(|order| *order != u32::MAX)
                .max()
                .map_or(0, |order| order.saturating_add(1))
        };
        if let Some(publication) = generated_publication {
            ordered.extend(
                publication
                    .functions
                    .iter()
                    .filter(|member| member.metadata.is_some())
                    .enumerate()
                    .map(|(index, _)| {
                        (
                            generated_order_base.saturating_add(index as u32),
                            ClassMemberOrder::Function(inferred_method_count + index),
                        )
                    }),
            );
        }
        ordered.extend(type_aliases.iter().enumerate().map(|(index, alias)| {
            (
                u32::try_from(alias.decl_order).unwrap_or(u32::MAX),
                ClassMemberOrder::TypeAlias(index),
            )
        }));
        ordered.sort_by_key(|(order, _)| *order);
        ordered.into_iter().map(|(_, member)| member).collect()
    };
    // A value class's primary constructor is realized as the static `constructor-impl` returning the
    // erased underlying, not `<init>`; its `@Metadata` signature records that.
    let vc_ctor_desc = c
        .is_value
        .then(|| format!("({0}){0}", desc(c.fields[0].ty)));
    let enum_entry_meta = enum_metadata::entries(c);
    // Metadata keeps nested declarations ordered and sealed subclasses sorted.
    // Every DECLARED direct nested classifier joins `Class.nestedClassName` (f7) — kotlinc records
    // them all, not only sealed subtypes. Declaration origin and the exact identity-tree relation
    // keep synthesized classes out without interpreting their backend spellings.
    // Common IR carries the stable declaration order selected by the frontend. The IR arena and
    // debug lines are representation facts and do not define this metadata order.
    let mut source_nested: Vec<(u32, String)> = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.is_source_declared
                && !candidate.is_local_class
                && candidate.fq_name.nested_owner() == Some(c.fq_name)
        })
        .map(|(index, candidate)| {
            (
                ir.class_source_order(index as crate::ir::ClassId)
                    .expect("a source classifier carries its stable declaration order"),
                candidate.fq_name.nested_segment_ref().to_string(),
            )
        })
        .collect();
    source_nested.sort_by_key(|(source_order, _)| *source_order);
    let mut nested_names: Vec<String> = source_nested
        .into_iter()
        .map(|(_, segment)| segment)
        .collect();
    // A producer can generate a classifier that Kotlin code names (`Foo.$serializer`) and publish
    // that fact on the owning class. Other synthesized implementation classes stay out on their
    // `is_source_declared` record alone. Generated names precede the COMPANION, which kotlinc lists
    // last of that set (`$serializer` then `Companion` for a `@Serializable` class).
    let generated_nested = c.published_nested_classifiers.iter().cloned();
    // kotlinc lists the companion under `nestedClassName` (f7) TOO, alongside its own
    // `companionObjectName` (f4) record — both reference the same interned string.
    let companion_segment = c
        .companion_class
        .as_ref()
        .map(|companion| companion.nested_segment_ref().to_string());
    let at = companion_segment
        .as_ref()
        .and_then(|segment| nested_names.iter().position(|name| name == segment))
        .unwrap_or(nested_names.len());
    nested_names.splice(at..at, generated_nested);
    if let Some(segment) = companion_segment {
        if !nested_names.contains(&segment) {
            nested_names.push(segment);
        }
    }
    // `Class.sealedSubclassFqName` (f16) belongs only to a SEALED classifier — the IR records
    // subtype relationships for every class, but kotlinc writes the field for sealed ones alone
    // (a plain interface with implementors carries none).
    let sealed_sorted = if c.is_sealed {
        sorted_sealed_subclass_ids(c)
    } else {
        Vec::new()
    };
    let nested_refs: Vec<&str> = nested_names.iter().map(String::as_str).collect();
    let class_type_parameters = ir
        .class_signature(&c.fq_name())
        .map(|signature| signature.type_params.as_slice())
        .unwrap_or_default();
    // `c.fields` is the JVM storage realization by this point: value-class lowering may replace a
    // generic underlying parameter with the carrier of its bound. Kotlin metadata instead describes
    // the source property. Keep those two facts separate, especially for `V<T : U<Int>>(val value: T)`:
    // the physical field may use `U`'s carrier, but Class.inlineClassUnderlyingType must still name
    // this class's own `T` identity.
    let inline_underlying = c.is_value.then(|| {
        let property = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(0))
            .expect("a value class must retain its semantic underlying property");
        (
            property.name.as_str(),
            (!property.visibility.is_public_api()).then_some(property.ty),
        )
    });
    // Metadata lists the declared superclass before interfaces.
    let super_internal = c.superclass.render();
    let mut supertypes = ir
        .class_signature(&c.fq_name())
        .filter(|signature| !signature.supers.is_empty())
        .map(|signature| signature.supers.clone())
        .unwrap_or_default();
    // A generic class's recorded supers ALWAYS materialize the superclass position — holding
    // `kotlin/Any` when none was declared — because a JVM class `Signature` must name a superclass.
    // `@Metadata` records only the supertypes source DECLARED, so drop that implicit `Any`; leaving
    // it in shows every consumer a supertype the declaration never wrote. Both shapes then agree:
    // a superclass slot exists exactly when one was declared.
    if super_internal == "kotlin/Any"
        && supertypes
            .first()
            .is_some_and(|first| matches!(first, Ty::Obj(n, _) if n.matches("kotlin/Any")))
    {
        supertypes.remove(0);
    }
    if supertypes.is_empty() {
        if super_internal != "kotlin/Any" {
            supertypes.push(Ty::obj(&super_internal));
        }
        supertypes.extend(c.interfaces.iter_ids().map(Ty::obj_name));
    }
    // The header's spellings have to follow the same shape, or every abbreviation lands on the
    // neighbouring supertype.
    let has_declared_superclass = super_internal != "kotlin/Any";
    let class_spellings = ir
        .class_declared_spellings
        .get(&c.fq_name_id())
        .cloned()
        .unwrap_or_default();
    let supertype_spellings = class_spellings.supertype_spellings(has_declared_superclass);
    let secondary_ctor_shapes = super::constructor_metadata::secondary_constructor_shapes(ir, c);
    let secondary_ctor_metas: Vec<crate::metadata::class_builder::CtorMeta> = secondary_ctor_shapes
        .iter()
        .map(|shape| crate::metadata::class_builder::CtorMeta {
            params: &shape.params,
            param_defaults: &shape.param_defaults,
            desc: &shape.descriptor,
            sig_name: shape.signature_name,
            vararg_index: shape.vararg_index,
            flags: shape.flags,
            annotations: &shape.annotations,
        })
        .collect();
    let metadata_annotations: Vec<crate::ir::AppliedAnnotation> = c
        .applied_annotations
        .iter()
        .map(|retained| retained.annotation.clone())
        .collect();
    let (d1_bytes, d2) = build_class(
        c.fq_name_id(),
        &ctor_params,
        vc_ctor_desc.as_deref().unwrap_or(&ctor_desc),
        &props,
        &methods,
        &enum_entry_meta,
        &ClassTail {
            spellings: class_spellings,
            supertype_spellings: &supertype_spellings,
            type_params: &c.type_params,
            type_param_bounds: class_type_parameters,
            captured_type_params: &c.captured_type_params,
            ctor_param_tparams: &ctor_param_tparams,
            ctor_param_annotations: &named_ctor_param_annotations,
            flags: class_metadata_flags(ir, c),
            // An `enum class`'s primary ctor is private too — entries are the only instances.
            // A DECLARED constructor visibility (`class C protected constructor(…)`) takes
            // precedence: the consumer must reject constructions the declaration forbids.
            primary_ctor_flags: match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => SEALED_CTOR_FLAGS,
                Some(crate::types::Visibility::Private) => OBJECT_CTOR_FLAGS,
                _ if c.is_sealed => SEALED_CTOR_FLAGS,
                _ if c.is_singleton() || !c.enum_entries.is_empty() => OBJECT_CTOR_FLAGS,
                _ => 0,
            },
            primary_ctor_jvm_signature: !c.is_annotation,
            module_name: opts.module_name.as_deref(),
            ctor_param_defaults: &ctor_param_defaults,
            inline_underlying,
            ctor_sig_name: c.is_value.then_some("constructor-impl"),
            // An interface has no constructor at all, whatever the IR records.
            // An interface has no constructor; a class with ONLY secondary constructors emits no
            // primary record either (its `Class.constructor` entries are the secondaries below).
            // Every other class keeps its (possibly implicit) primary record — an `enum class`
            // without a declared constructor still records the implicit private `(String, I)` one.
            emit_primary_ctor: !c.is_interface
                && (c.has_primary_ctor || c.secondary_ctors.is_empty()),
            // `jvmClassFlags` describes the interface SHAPE this compilation produced, so it tracks
            // `-jvm-default` exactly: a consumer reads it to know whether method bodies live on the
            // interface and whether a `$DefaultImpls` compatibility copy exists.
            jvm_class_flags: c
                .is_interface
                .then(|| opts.jvm_default.interface_jvm_class_flags())
                .flatten(),
            // Kotlin 1.4 introduced JVM default methods without compatibility holders. Older
            // consumers must reject this metadata instead of assuming the legacy `$DefaultImpls`
            // realization, so kotlinc attaches a compiler-version requirement to every interface.
            compiler_version_requirement: (c.is_interface
                && opts.jvm_default == JvmDefaultMode::NoCompatibility)
                .then_some((1, 4, 0)),
            // A class with a companion records its simple name (`Class.companionObjectName`, f4) —
            // the consumer resolves `C.member` through it.
            companion: c
                .companion_class
                .as_ref()
                .map(|companion| companion.nested_segment_ref()),
            secondary_ctors: &secondary_ctor_metas,
            ctor_vararg_index,
            nested: &nested_refs,
            member_order: &member_order,
            type_aliases: &type_aliases,
            sealed_subclasses: &sealed_sorted,
            supertypes: &supertypes,
            annotations: &metadata_annotations,
            primary_ctor_annotations: &primary_ctor_annotations(c),
        },
    );
    // d1 is the protobuf payload as one `char` per byte (the constant pool writes it as modified-UTF-8).
    let d1 = vec![d1_bytes.iter().map(|&b| b as char).collect()];
    Some(KotlinMetadata {
        k: 1,
        mv: vec![2, 4, 0],
        xi: 48,
        d1,
        d2,
    })
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
        return value_class_metadata_shape_admitted(ir, declared);
    }
    !ir.module_source_value_classes.contains(&fq_name)
        || ir.module_readable_value_classes.contains(&fq_name)
}

/// Common class-shape admission shared by the writer and transitive value-class readability. Keeping
/// these kind/constructor bails in one predicate is correctness-critical: if the writer withholds a
/// value class but the transitive check independently admits it, a mentioning class publishes a type
/// a downstream compiler reads as an ordinary box.
fn class_metadata_common_shape_admitted(_ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    // Local/anonymous classifiers cannot be named by another compilation unit. Their lexical type
    // parameters are not declarations of the generated class, so publishing a class metadata record
    // would require falsely redeclaring them; omit the non-observable record instead.
    !(c.is_local_class
        || c.enum_entry_of.is_some()
        || c.prop_ref.is_some()
        || c.func_ref.is_some()
        // A published secondary constructor is described from its recorded semantic parameter
        // identities. A malformed publication contract would advertise the wrong parameter list,
        // so the class declines instead. Unpublished target realizations carry no record.
        || c.secondary_ctors
            .iter()
            .any(|sc| sc.metadata_visibility.is_some() && sc.named_params.len() != sc.params.len())
        || (!c.has_primary_ctor
            && c.secondary_ctors.is_empty()
            && !c.is_interface
            && c.enum_entries.is_empty())
        || (c.fields.len() as u32) < c.ctor_param_count)
}

/// The single admission predicate for a VALUE class's own metadata record. Both
/// [`build_class_metadata`] and [`value_class_is_readable`] call it, so adding a new write-side bail
/// cannot silently let a different class describe the withheld value class downstream.
fn value_class_metadata_shape_admitted(ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    c.is_value
        && class_metadata_common_shape_admitted(ir, c)
        // A value class's ctors realize as mangled static `constructor-impl` overloads, which the
        // secondary-ctor record path does not model — keep declining that combination.
        && c.secondary_ctors.is_empty()
        && c.fields.len() == 1
        && c.fields[0].is_final()
        && !ir.has_value_param_ctor(&c.fq_name())
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

/// Physical JVM name of an instance backing field. Kotlin permits an instance property and a
/// companion property with the same source name, but the JVM field signature does not include the
/// STATIC flag. kotlinc therefore keeps the companion static's source name and suffixes the instance
/// backing field (`result` -> `result$1`). This is solely a JVM realization decision: IR properties,
/// metadata, accessors, and resolver identities retain the source name.
fn instance_field_jvm_name(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    field: &crate::ir::IrField,
) -> String {
    let field_index = class
        .fields
        .iter()
        .position(|candidate| std::ptr::eq(candidate, field))
        .expect("an instance field name must belong to its class");
    if let Some(capture) = super::method_parameters::capture_field_name(class, field_index) {
        return capture;
    }
    let owner = class.fq_name();
    let descriptor = type_descriptor(jvm_declared_ty(&field.ty));
    let conflicts_with_static = ir.statics.iter().any(|static_field| {
        static_field.owner_matches(&owner)
            && static_field.name == field.name
            && type_descriptor(jvm_declared_ty(&static_field.ty)) == descriptor
    });
    if !conflicts_with_static {
        return field.name.clone();
    }
    for suffix in 1usize.. {
        let candidate = format!("{}${suffix}", field.name);
        let occupied_by_instance = class.fields.iter().any(|other| {
            other.name == candidate && type_descriptor(jvm_declared_ty(&other.ty)) == descriptor
        });
        let occupied_by_static = ir.statics.iter().any(|static_field| {
            static_field.owner_matches(&owner)
                && static_field.name == candidate
                && type_descriptor(jvm_declared_ty(&static_field.ty)) == descriptor
        });
        if !occupied_by_instance && !occupied_by_static {
            return candidate;
        }
    }
    unreachable!("an unused JVM backing-field suffix always exists")
}

/// The primary constructor's declared annotations in class-file order — RUNTIME-visible first, then
/// the BINARY-retained invisible ones. The pool seeder and the `@Metadata` mirror both read this
/// single sequence, so the two halves cannot drift apart.
fn primary_ctor_annotations(c: &crate::ir::IrClass) -> Vec<crate::ir::AppliedAnnotation> {
    let (visible, invisible) =
        crate::jvm::classfile::split_declaration_annotations(&c.primary_ctor_annotations);
    visible.into_iter().chain(invisible).collect()
}

/// JVM dispatch owner for a data-class field's reference `hashCode` call. Common IR carries only
/// the declared Kotlin type; interface dispatch and boxed scalar ownership are representation facts
/// derived here by the backend. `None` means the classfile seeder can use its primitive/array rule.
fn data_class_hashcode_owner(ir: &IrFile, bodies: &dyn MethodBodies, ty: Ty) -> Option<String> {
    if ty.is_array() || (ty.non_null().is_jvm_scalar() && !ty.is_nullable()) {
        return None;
    }
    if let Some(owner) = ty.non_null().obj_internal() {
        if ir.value_class_underlying_name(owner).is_some() {
            return Some(owner.render());
        }
    }
    let mut owner = if ty.is_nullable() && ty.non_null().is_jvm_scalar() {
        "java/lang/Object".to_owned()
    } else {
        crate::jvm::names::instanceof_internal_name(ty.non_null())
    };
    if ty
        .non_null()
        .obj_internal()
        .and_then(|name| ir.class_id_by_name(name))
        .is_some_and(|class| ir.classes[class as usize].is_interface)
        || bodies.owner_is_interface(&owner)
    {
        owner = "java/lang/Object".to_owned();
    }
    Some(owner)
}

/// One synthesized value-class member's JVM name, descriptor, and local-variable table entries.
type VcDebugMethod = (String, String, Vec<(String, String, u16)>);

fn attach_declared_method_debug(ir: &IrFile, c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let owner = c.fq_name();
    for &fid in &c.methods {
        function_debug::attach_declared_function_debug(ir, fid, &owner, cw);
    }
}

/// The HOISTED outer-class static backing a companion property of `c` (a companion class), if any:
/// the companion has no field for it, so field-driven attribute passes need this lookup instead.
fn hoisted_static_for<'a>(
    ir: &'a IrFile,
    c: &crate::ir::IrClass,
    property: usize,
) -> Option<&'a crate::ir::IrStatic> {
    if !c.is_companion {
        return None;
    }
    let static_id = ir.jvm_companion_property_static(c.fq_name_id(), property as u32)?;
    ir.statics.get(static_id as usize)
}

/// Whether this companion property's hoisted static is the `@JvmField` realization — a public owner
/// field with NO companion accessors, so accessor-shaped emission must skip it entirely.
fn jvm_field_static_for(ir: &IrFile, c: &crate::ir::IrClass, property: usize) -> bool {
    c.is_companion
        && ir
            .jvm_companion_property_static(c.fq_name_id(), property as u32)
            .is_some_and(|static_id| ir.is_jvm_field_static(static_id))
}

fn attach_synth_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    param_assertions: bool,
    // The primary constructor method emission actually produced and the source-mapped body offset
    // it reached after any parameter guards. Neither the physical descriptor nor the bytecode
    // position may be reconstructed later from fields or semantic constructor arguments.
    primary_ctor_debug: Option<(&str, u16)>,
    // Extra ctor LineNumberTable entries (body-property initializers + the trailing `return`), with
    // their real pcs captured during emission. Empty ⇒ the ctor gets kotlinc's single entry.
    ctor_lines: &[(u16, u32)],
) {
    let line = c.decl_line;
    if line == 0 {
        return;
    }
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let slot_size = |t: Ty| -> u16 {
        match desc(t).as_str() {
            "J" | "D" => 2,
            _ => 1,
        }
    };
    // `aload <slot>` byte length: 1 (aload_0..3), 2 (aload u1), or 4 (wide aload u2). Synthesized
    // setter debug still uses this until those accessors carry their own emission provenance too.
    let aload_len = |slot: u16| -> u16 {
        if slot <= 3 {
            1
        } else if slot <= 255 {
            2
        } else {
            4
        }
    };
    let this_desc = format!("L{};", c.fq_name());
    // A data class's `copy` parameters are exactly its property-backed constructor parameters.
    // This is not the primary constructor's physical descriptor (plain parameters may also exist
    // there), so keep the two identities deliberately separate.
    let data_copy_desc = format!("({})V", ctor_field_descs(c));
    // Primary constructor: `this` + one local per ctor parameter (a property-backed param). An
    // `enum class`'s ctor is `(String name, int ordinal, …declared params)`: kotlinc prepends the two
    // synthetic `Enum` parameters and names them `$enum$name` / `$enum$ordinal` in the LVT.
    let is_enum = !c.enum_entries.is_empty();
    let mut ctor_locals = vec![("this".to_string(), this_desc.clone(), 0u16)];
    let mut slot = 1u16;
    if is_enum {
        ctor_locals.push((
            "$enum$name".to_string(),
            "Ljava/lang/String;".to_string(),
            slot,
        ));
        ctor_locals.push(("$enum$ordinal".to_string(), "I".to_string(), slot + 1));
        slot += 2;
    }
    // Before Kotlin 2.4.20 an anonymous context parameter has no LVT row; since then its generated
    // reflection/assertion label names the physical constructor local too.
    let constructor_locals = crate::jvm::parameter_names::constructor_local_variables(&c.ctor_args);
    for (argument, name) in c.ctor_args.iter().zip(constructor_locals) {
        if let Some(name) = name {
            ctor_locals.push((name, desc(argument.ty), slot));
        }
        slot += slot_size(argument.ty);
    }
    let this_only = [("this".to_string(), this_desc.clone(), 0u16)];
    // kotlinc maps the `super()` call to where the DECLARATION starts — annotations included — and
    // the ctor's trailing `return` (pushed into `ctor_lines` by the emitter) back to the class
    // HEADER line. The two coincide unless an annotation sits on its own line above the header.
    let ctor_start_line = if c.decl_start_line == 0 {
        line
    } else {
        c.decl_start_line
    };
    if let Some((ctor_desc, ctor_pc)) = primary_ctor_debug {
        cw.set_method_debug(
            "<init>",
            ctor_desc,
            Some((ctor_pc, ctor_start_line)),
            &ctor_locals,
        );
        if !ctor_lines.is_empty() {
            let mut entries = vec![(ctor_pc, ctor_start_line)];
            entries.extend_from_slice(ctor_lines);
            // kotlinc never emits two consecutive entries for the same line — a run of stores on the
            // class-declaration line (a single-line `class C(val a: Int)`) collapses to one entry.
            entries.dedup_by_key(|(_, l)| *l);
            cw.set_method_lines("<init>", ctor_desc, &entries);
        }
    }
    // A marker accessor gets the same locals as the primary constructor plus its synthetic marker.
    if has_ctor_marker_accessor(ir, c) {
        const MARKER: &str = "Lkotlin/jvm/internal/DefaultConstructorMarker;";
        let mut acc_locals = ctor_locals.clone();
        let marker_slot = c
            .fields
            .iter()
            .take(c.ctor_param_count as usize)
            .map(|f| slot_size(f.ty))
            .sum::<u16>()
            + 1;
        acc_locals.push((
            "$constructor_marker".to_string(),
            MARKER.to_string(),
            marker_slot,
        ));
        let acc_desc = format!("({}{MARKER})V", ctor_field_descs(c));
        cw.set_method_debug("<init>", &acc_desc, None, &acc_locals);
    }
    // Synthesized property setters use the declaration's recorded nullability policy. This is
    // independent of the constructor's exact `IrCtorArg.check` facts above: a value class may omit
    // its private-constructor guard while its public mutable-property setter still requires one.
    let is_nonnull_ref =
        |name: &str, ty: Ty| -> bool { is_nonnull_reference_field(ir, &c.fq_name(), name, ty) };
    // Property accessors: getter has only `this`; a `var` setter also has its value parameter (named
    // `<set-?>` by kotlinc), guarded when the property type is a non-null reference.
    for (field_index, f) in c.fields.iter().enumerate() {
        // An accessor represented as a real function carries its own debug contract. Decide the
        // getter and setter independently: a `var` can declare one and retain the synthesized other.
        // The plugin-generated `descriptor` getter intentionally has NO line table, which this
        // class-level synthesis would otherwise overwrite with the declaration line.
        let declared_property = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(field_index as u32));
        // A CTOR-parameter property's accessors sit on the class-declaration line; a BODY property's
        // sit on its own `val`/`var` line.
        let pline = ir
            .prop_decl_lines
            .get(&(c.fq_name_id(), f.name.clone()))
            .copied()
            .filter(|&l| l != 0)
            .unwrap_or(line);
        let (g, s) = accessor_jvm_names(c, &f.name);
        if declared_property.is_none_or(|property| property.getter.is_none()) {
            cw.set_method_debug(
                &g,
                &format!("(){}", desc(f.ty)),
                Some((0, pline)),
                &this_only,
            );
        }
        if !f.is_final() && declared_property.is_none_or(|property| property.setter.is_none()) {
            let pd = desc(f.ty);
            // The setter's value param is always slot 1 (`this`=0): guard = `aload_1`(1) + the
            // `<set-?>` String's real ldc width + invokestatic(3).
            let set_pc = if param_assertions && is_nonnull_ref(&f.name, f.ty) {
                aload_len(1) + cw.string_ldc_len("<set-?>").unwrap_or(2) + 3
            } else {
                0
            };
            cw.set_method_debug(
                &s,
                &format!("({pd})V"),
                Some((set_pc, pline)),
                &[
                    ("this".to_string(), this_desc.clone(), 0),
                    ("<set-?>".to_string(), pd, 1),
                ],
            );
        }
    }
    // HOISTED companion properties: no companion field, but the delegating accessors get the same
    // debug shape kotlinc gives ordinary accessors (getter: `this` only; a `var` setter also has
    // its `<set-?>` value parameter, guarded when the property type is a non-null reference).
    // A `@JvmField` property has NO accessors — nothing to describe.
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some() || jvm_field_static_for(ir, c, property_index) {
            continue;
        }
        let Some(hoisted) = hoisted_static_for(ir, c, property_index) else {
            continue;
        };
        let pline = if property.decl_line != 0 {
            property.decl_line
        } else {
            line
        };
        let pd = crate::jvm::names::type_descriptor(jvm_declared_ty(&hoisted.ty));
        let (g, s) = accessor_jvm_names(c, &property.name);
        cw.set_method_debug(&g, &format!("(){pd}"), Some((0, pline)), &this_only);
        if hoisted.is_var {
            let set_pc = if param_assertions && is_nonnull_ref(&property.name, hoisted.ty) {
                aload_len(1) + cw.string_ldc_len("<set-?>").unwrap_or(2) + 3
            } else {
                0
            };
            cw.set_method_debug(
                &s,
                &format!("({pd})V"),
                Some((set_pc, pline)),
                &[
                    ("this".to_string(), this_desc.clone(), 0),
                    ("<set-?>".to_string(), pd.clone(), 1),
                ],
            );
        }
    }
    // A companion OUTER's `access$…$cp` bridges: kotlinc maps each to the CLASS declaration line
    // (getter bridges carry only the LineNumberTable; the setter bridge also names its `<set-?>`
    // value parameter).
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| {
            ir.is_jvm_companion_hoisted_static(*index as u32)
                && !ir.is_jvm_field_static(*index as u32)
                && s.owner_matches(&c.fq_name())
        })
        .map(|(_, s)| s)
    {
        let pd = crate::jvm::names::type_descriptor(jvm_declared_ty(&s.ty));
        let getter_bridge = format!("access${}$cp", crate::names::property_getter_name(&s.name));
        cw.set_method_debug(&getter_bridge, &format!("(){pd}"), Some((0, line)), &[]);
        if s.is_var {
            let setter_bridge =
                format!("access${}$cp", crate::names::property_setter_name(&s.name));
            cw.set_method_debug(
                &setter_bridge,
                &format!("({pd})V"),
                Some((0, line)),
                &[("<set-?>".to_string(), pd.clone(), 0)],
            );
        }
    }
    // A `@JvmInline value class`'s synthesized members: the static `-impl` family (taking the erased
    // underlying) and their instance delegators. kotlinc gives each a LocalVariableTable but no
    // LineNumberTable; the static impls name their parameter positionally (`arg0`/`v`/`p1`/`p2`) except
    // `constructor-impl`, which keeps the property name.
    if c.is_value {
        if let Some(f0) = c.fields.first() {
            let u = desc(f0.ty);
            let obj = "Ljava/lang/Object;".to_string();
            let w = slot_size(f0.ty);
            let one = |n: &str, d: &String, slot: u16| vec![(n.to_string(), d.clone(), slot)];
            let vc_methods: Vec<VcDebugMethod> = vec![
                (
                    "toString-impl".into(),
                    format!("({u})Ljava/lang/String;"),
                    one("arg0", &u, 0),
                ),
                (
                    "toString".into(),
                    "()Ljava/lang/String;".into(),
                    one("this", &this_desc, 0),
                ),
                (
                    "hashCode-impl".into(),
                    format!("({u})I"),
                    one("arg0", &u, 0),
                ),
                ("hashCode".into(), "()I".into(), one("this", &this_desc, 0)),
                (
                    "equals-impl".into(),
                    format!("({u}Ljava/lang/Object;)Z"),
                    vec![
                        ("arg0".to_string(), u.clone(), 0),
                        ("other".to_string(), obj.clone(), w),
                    ],
                ),
                (
                    "equals".into(),
                    "(Ljava/lang/Object;)Z".into(),
                    vec![
                        ("this".to_string(), this_desc.clone(), 0),
                        ("other".to_string(), obj.clone(), 1),
                    ],
                ),
                (
                    "constructor-impl".into(),
                    format!("({u}){u}"),
                    one(&f0.name, &u, 0),
                ),
                (
                    "box-impl".into(),
                    format!("({u}){this_desc}"),
                    one("v", &u, 0),
                ),
                (
                    "unbox-impl".into(),
                    format!("(){u}"),
                    one("this", &this_desc, 0),
                ),
                (
                    "equals-impl0".into(),
                    format!("({u}{u})Z"),
                    vec![
                        (
                            crate::jvm::parameter_names::value_class_equals_operand(1).to_string(),
                            u.clone(),
                            0,
                        ),
                        (
                            crate::jvm::parameter_names::value_class_equals_operand(2).to_string(),
                            u.clone(),
                            w,
                        ),
                    ],
                ),
            ];
            for (name, d, locals) in &vc_methods {
                cw.set_method_debug(name, d, None, locals);
            }
        }
    }
    // A `data class`'s synthesized methods carry a LocalVariableTable (this + params) but NO
    // LineNumberTable (kotlinc gives them none). componentN/hashCode/toString/equals have only `this`
    // (equals also `other`); `copy` has the ctor parameters.
    if c.is_data {
        let self_ref = format!("L{};", c.fq_name());
        let data_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
        for (i, f) in data_fields.iter().enumerate() {
            cw.set_method_debug(
                &format!("component{}", i + 1),
                &format!("(){}", desc(f.ty)),
                None,
                &this_only,
            );
        }
        // A `data object` synthesizes no `copy` (see the metadata assembly), so it has no table either.
        if !data_fields.is_empty() {
            let mut copy_locals = vec![("this".to_string(), this_desc.clone(), 0u16)];
            let mut slot = 1u16;
            for f in data_fields {
                copy_locals.push((f.name.clone(), desc(f.ty), slot));
                slot += slot_size(f.ty);
            }
            cw.set_method_debug(
                "copy",
                &format!(
                    "{copy_desc_no_v}{self_ref}",
                    copy_desc_no_v = &data_copy_desc[..data_copy_desc.len() - 1]
                ),
                None,
                &copy_locals,
            );
        }
        cw.set_method_debug(
            "equals",
            "(Ljava/lang/Object;)Z",
            None,
            &[
                ("this".to_string(), this_desc.clone(), 0),
                ("other".to_string(), "Ljava/lang/Object;".to_string(), 1),
            ],
        );
        // hashCode: a ≥2-field data class folds into a `result` accumulator local — kotlinc lists it
        // (partial live-range) before `this`. A single-field hashCode is a bare `return h(f0)` (this only).
        if c.fields.len() >= 2 {
            cw.set_hashcode_result_debug(&this_desc);
        } else {
            cw.set_method_debug("hashCode", "()I", None, &this_only);
        }
        cw.set_method_debug("toString", "()Ljava/lang/String;", None, &this_only);
    }
}

/// Attach kotlinc's `@org.jetbrains.annotations.NotNull` / `@Nullable` to a plain property class's
/// synthesized members: each non-null reference-typed return/parameter gets `@NotNull`, each nullable
/// reference gets `@Nullable` (primitives get nothing). Covers the ctor's reference params, each
/// getter's reference return, and each `var` setter's reference param — the shape kotlinc emits for a
/// class with reference-typed properties. Call after `attach_synth_debug_tables`.
fn attach_synth_nullability(ir: &IrFile, c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    // A reference type (descriptor `L…;`/`[…`) gets `@NotNull` unless it is `Ty::Nullable`, then
    // `@Nullable`; a primitive gets no annotation.
    let ann = |name: &str, t: Ty| -> Option<&'static str> {
        match field_nullability_kind(ir, &c.fq_name(), name, t) {
            1 => Some("Lorg/jetbrains/annotations/NotNull;"),
            2 => Some("Lorg/jetbrains/annotations/Nullable;"),
            _ => None,
        }
    };
    // Interfaces have accessors but no backing fields. The annotation targets the PHYSICAL field
    // (`result$1` when mangled away from a same-named hoisted companion static). The constructor
    // prefix's fields (the outer instance, lexical captures) are the compiler's own: kotlinc
    // annotates no synthetic declaration.
    let prefix = c.constructor_prefix_count as usize;
    if !c.is_interface {
        for f in c.fields.iter().skip(prefix) {
            if let Some(a) = ann(&f.name, f.ty) {
                cw.set_field_nullability(&instance_field_jvm_name(ir, c, f), a);
            }
        }
    }
    // The `Companion` instance field is a never-null reference (`@NotNull`), and each REFERENCE
    // hoisted companion static is annotated like any other backing field.
    if let Some(companion) = &c.companion_class {
        cw.set_field_nullability(
            companion.nested_segment_ref(),
            "Lorg/jetbrains/annotations/NotNull;",
        );
        for s in ir
            .statics
            .iter()
            .enumerate()
            .filter(|(index, s)| {
                ir.is_jvm_companion_hoisted_static(*index as u32) && s.owner_matches(&c.fq_name())
            })
            .map(|(_, s)| s)
        {
            if let Some(a) = ann(&s.name, jvm_declared_ty(&s.ty)) {
                cw.set_field_nullability(&s.name, a);
            }
        }
    }
    // Primary constructor: one parameter annotation slot per SOURCE parameter, plain or
    // property-backed. The prefix takes no slot: kotlinc sizes the table by the source parameters,
    // so an inner class's first declared parameter is annotation parameter 0, as in javac's output.
    let source_parameters = primary_ctor_source_parameters(ir, c);
    let ctor_params: Vec<Option<&str>> = source_parameters
        .iter()
        .map(|parameter| match parameter.nullability {
            1 => Some("Lorg/jetbrains/annotations/NotNull;"),
            2 => Some("Lorg/jetbrains/annotations/Nullable;"),
            _ => None,
        })
        .collect();
    let ctor_desc = primary_ctor_descriptor(c);
    if ctor_params.iter().any(|p| p.is_some()) {
        cw.set_method_nullability("<init>", &ctor_desc, None, &ctor_params);
    }
    // HOISTED companion properties: the delegating accessors annotate like ordinary accessors
    // (reference getter return; a `var` reference setter's parameter). `@JvmField` emits no
    // accessors, so there is nothing to annotate (the FIELD's annotations ride the owner class).
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some() || jvm_field_static_for(ir, c, property_index) {
            continue;
        }
        let Some(hoisted) = hoisted_static_for(ir, c, property_index) else {
            continue;
        };
        let Some(a) = ann(&property.name, hoisted.ty) else {
            continue;
        };
        let (getter, setter) = accessor_jvm_names(c, &property.name);
        let pd = desc(hoisted.ty);
        cw.set_method_nullability(&getter, &format!("(){pd}"), Some(a), &[]);
        if hoisted.is_var {
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
    for f in &c.fields {
        let Some(a) = ann(&f.name, f.ty) else {
            continue;
        };
        let (getter, setter) = accessor_jvm_names(c, &f.name);
        cw.set_method_nullability(&getter, &format!("(){}", desc(f.ty)), Some(a), &[]);
        if !f.is_final() {
            cw.set_method_nullability(&setter, &format!("({})V", desc(f.ty)), None, &[Some(a)]);
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
        let copy_is_private = copy.is_some_and(|fid| ir.private_methods.contains(&fid));
        if !data_fields.is_empty() && !copy_is_private {
            let fid = copy.expect("a non-singleton data class records its generated copy identity");
            let function = &ir.functions[fid as usize];
            // `copy`'s parameters mirror the primary-constructor properties, so each reference param
            // takes the SAME `@NotNull`/`@Nullable` annotation kotlinc puts on the constructor's.
            let copy_params: Vec<Option<&str>> =
                data_fields.iter().map(|f| ann(&f.name, f.ty)).collect();
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
        for (i, f) in data_fields.iter().enumerate() {
            if let Some(a) = ann(&f.name, f.ty) {
                let fid = ir
                    .data_class_member(c.fq_name_id(), IrDataClassMemberRole::Component(i as u32))
                    .expect("a data-class component records its generated identity");
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
    // A value class's `constructor-impl` returns the erased underlying; kotlinc annotates that return
    // exactly like the property's (a non-null reference underlying → `@NotNull`).
    if c.is_value {
        if let Some(f0) = c.fields.first() {
            if let Some(a) = ann(&f0.name, f0.ty) {
                let u = desc(f0.ty);
                cw.set_method_nullability(
                    "constructor-impl",
                    &format!("({u}){u}"),
                    Some(a),
                    &[Some(a)],
                );
            }
        }
    }
}

fn add_companion_field(cw: &mut ClassWriter, class: &IrClass) {
    let Some(companion) = class.companion_class else {
        return;
    };
    cw.add_field(
        0x0019,
        companion.nested_segment_ref(),
        &format!("L{};", companion.render()),
    );
}

fn emit_companion_init(cw: &mut ClassWriter, code: &mut CodeBuilder, owner: &str, class: &IrClass) {
    let Some(companion) = class.companion_class else {
        return;
    };
    let companion_name = companion.render();
    let descriptor = format!("L{companion_name};");
    // An INTERFACE's companion self-hosts its singleton (`static final $$INSTANCE`, built in the
    // companion's own `<clinit>`); the interface's `Companion` field merely aliases it.
    if is_jvm_interface(class) {
        let instance = cw.fieldref(&companion_name, "$$INSTANCE", &descriptor);
        code.getstatic(instance, 1);
        let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
        code.putstatic(field, 1);
        return;
    }
    let classifier = cw.class_ref(&companion_name);
    code.new_obj(classifier);
    code.dup();
    code.aconst_null();
    let constructor = cw.methodref(
        &companion_name,
        "<init>",
        "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
    );
    code.invokespecial(constructor, 1, 0);
    let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
    code.putstatic(field, 1);
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

    // Ordinary companion constants/values precede the Companion field. A hoisted `@JvmField`
    // property is visited later because kotlinc places it after Companion.
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| s.owner_matches(&fq_name) && !ir.is_jvm_field_static(*index as u32))
        .map(|(_, s)| s)
    {
        let descriptor = ir_type_desc(&s.ty);
        if let Some(value) = static_fields::const_value_idx(ir, s.init, cw) {
            cw.add_field_const(0x0019, &s.name, &descriptor, value);
        } else {
            cw.add_field(0x0019, &s.name, &descriptor);
        }
    }

    let clinit_statics: Vec<&crate::ir::IrStatic> = ir
        .statics
        .iter()
        .filter(|s| {
            s.owner_matches(&fq_name)
                && !(s.is_const && static_fields::const_value_idx_peek(ir, s.init))
        })
        .collect();
    if c.companion_class.is_some() || !clinit_statics.is_empty() {
        cw.reserve_method_name("<clinit>");
        cw.seed_utf8("()V");
        let mut emitter = Emitter::new(
            ir,
            cw,
            env,
            &fq_name,
            facade,
            Ty::Unit,
            clinit_statics.iter().map(|property| property.init),
        );
        let mut clinit = CodeBuilder::new(0);
        emit_companion_init(emitter.cw, &mut clinit, &fq_name, c);
        let mut clinit_lines = Vec::new();
        for s in &clinit_statics {
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
            emitter.emit_static_initializer_store(&fq_name, s, &mut clinit);
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

fn add_singleton_instance_field(cw: &mut ClassWriter, class: &str) {
    cw.add_field(0x0019, "INSTANCE", &format!("L{class};"));
}

fn emit_singleton_instance_clinit(cw: &mut ClassWriter, class: &str) {
    // The method header interns before its code, as a writer visiting the method first does.
    cw.seed_utf8("<clinit>");
    cw.seed_utf8("()V");
    let descriptor = format!("L{class};");
    let classifier = cw.class_ref(class);
    let constructor = cw.methodref(class, "<init>", "()V");
    let field = cw.fieldref(class, "INSTANCE", &descriptor);
    let mut code = CodeBuilder::new(0);
    code.new_obj(classifier);
    code.dup();
    code.invokespecial(constructor, 0, 0);
    code.putstatic(field, 1);
    code.ret_void();
    finish_code::<0x0008>(cw, "<clinit>", "()V", &mut code, 0);
}

fn sorted_sealed_subclass_ids(c: &IrClass) -> Vec<TypeName> {
    let mut subclasses: Vec<TypeName> = c.sealed_subclasses.iter_ids().collect();
    subclasses.sort_by(|a, b| {
        a.nested_segment_ref()
            .cmp(b.nested_segment_ref())
            .then_with(|| a.path_cmp(*b))
    });
    subclasses
}

fn register_sealed_subtypes(cw: &mut ClassWriter, ir: &IrFile, c: &IrClass, emit_permitted: bool) {
    use crate::jvm::classfile::InnerClassSpec;
    // The IR records subtype relationships for EVERY class; only a SEALED classifier turns them
    // into PermittedSubclasses + eager nest entries (a plain interface with an anonymous
    // implementor was seeding that implementor's class constant into its own pool).
    if !c.is_sealed {
        return;
    }
    let self_identity = c.fq_name_id();
    let subs = sorted_sealed_subclass_ids(c);
    if subs.is_empty() {
        return;
    }
    for &sub in &subs {
        if sub != self_identity && sub.same_or_nested_within(self_identity) {
            let rendered = sub.render();
            cw.seed_class(&rendered);
            cw.add_inner_class(InnerClassSpec {
                inner: rendered,
                outer: Some(self_identity.render()),
                name: Some(
                    sub.nested_segment_within(self_identity)
                        .expect("a nested sealed subtype must have a relative segment")
                        .to_string(),
                ),
                access: ir
                    .classes
                    .iter()
                    .find(|candidate| candidate.fq_name_id() == sub)
                    .map_or(0x0019, |candidate| {
                        crate::jvm::inner_classes::class_access(ir, candidate)
                    }),
            });
        }
    }
    if emit_permitted {
        let rendered: Vec<String> = subs.iter().map(|subclass| subclass.render()).collect();
        for sub in &rendered {
            cw.seed_class(sub);
        }
        cw.set_permitted_subclasses(rendered);
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
    let recorded = ir.class_signature_name(c.fq_name);
    let signature = if !c.enum_entries.is_empty() {
        jvm_enum_class_signature(&formatter, c, recorded)
    } else {
        recorded.and_then(|signature| jvm_class_signature(&formatter, signature))
    };
    let internal = c.fq_name();
    // kotlinc (ASM) visits `(name, signature, superName)`, so the signature VALUE interns between
    // the two class names — it must reach the writer's constructor, not only `set_signature`.
    let mut cw = new_writer_generic(&internal, signature.as_deref(), super_internal, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    cw.set_nullability_annotations(!is_local_classifier(ir, c));
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
            signature.push_str(&formatter.ty_at(supertype, Wildcards::Suppressed)?);
        }
    } else {
        for interface in class.interfaces.iter_ids() {
            signature.push_str(&formatter.ty_at(&Ty::obj_name(interface), Wildcards::Suppressed)?);
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
/// Mark the lambda-argument impls of a MUST-INLINE call (`require`/`check`/`error` — a non-public
/// `@InlineOnly` callee the backend always splices, never invokes) as `inline_only`, so the standalone
/// `$lambda$N` method is NOT emitted. It is dead: the message lambda is spliced at the call site, so a
/// leftover impl would only force a spurious facade class (`OrganizationIdKt` holding a dead
/// `$lambda$0`) that kotlinc never emits. Safe because a `MustInline` callee is guaranteed spliced (or
/// the whole file is skipped — then nothing is emitted anyway).
/// Reparent lambda impl methods into the CLASS whose code emits their `invokedynamic`. An impl is
/// PRIVATE (kotlinc's placement: same class as the call site), so a cross-class method handle would
/// throw `IllegalAccessError`. Lowering attaches impls per `cur_class`; this pass covers the code
/// that reaches a class only later: enum-entry constructor arguments (lowered class-less, emitted in
/// the enum's `<clinit>`) and suspend-lambda state-machine bodies (moved into the machine class).
/// Transitive: an impl reparented into a class drags the impls of its own nested lambdas along.
pub fn reparent_lambda_impls(ir: &mut IrFile) {
    let mut owned: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    // Impls whose `invokedynamic` (also) emits from FACADE code — facade-owned function bodies and
    // static initializers — must STAY on the facade: a suspend-lambda state machine SHARES its body
    // exprs with facade code, so a class walk alone would move an impl the facade still references.
    let facade_reachable: std::collections::HashSet<u32> = {
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for (i, f) in ir.functions.iter().enumerate() {
            // A lambda IMPL's body emits wherever the impl itself lands (facade or a class), so it
            // is NOT a facade root — its nested lambdas are marked transitively below only when the
            // impl is genuinely reachable from real facade code.
            if !owned.contains(&(i as u32))
                && f.dispatch_receiver.is_none()
                && !ir.lambda_own_params_from.contains_key(&(i as u32))
            {
                if let Some(b) = f.body {
                    roots.push(b);
                }
            }
        }
        for st in &ir.statics {
            roots.push(st.init);
        }
        let mut out = std::collections::HashSet::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[cur as usize] {
                if out.insert(*impl_fn) {
                    // Its nested lambdas emit wherever it does — keep the whole chain facade-side.
                    if let Some(b) = ir.functions.get(*impl_fn as usize).and_then(|f| f.body) {
                        stack.push(b);
                    }
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
        out
    };
    for cid in 0..ir.classes.len() {
        // Class-context roots whose code emits inside this class: member/method bodies (covers a
        // suspend machine's `invokeSuspend`), the instance initializer, super/delegate arguments,
        // and enum-entry constructor arguments (emitted in `<clinit>`).
        let c = &ir.classes[cid];
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for &fid in &c.methods {
            if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                roots.push(b);
            }
        }
        roots.extend(c.init_body);
        roots.extend(c.super_arg_prelude.iter().copied());
        roots.extend(c.super_args.iter().copied());
        for sc in &c.secondary_ctors {
            roots.extend(sc.body);
            roots.extend(sc.defaults.iter().flatten().copied());
            roots.extend(sc.delegate_prelude.iter().copied());
            roots.extend(sc.delegate_args.iter().copied());
        }
        for en in &c.enum_entries {
            roots.extend(en.args.iter().copied());
        }
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[cur as usize] {
                let fid = *impl_fn;
                // Only a free (facade-owned) standalone impl moves; one already owned by a class —
                // including THIS one — stays. A spliced (inline-only) impl never emits a method.
                if !owned.contains(&fid)
                    && !facade_reachable.contains(&fid)
                    && !ir.inline_only_fns.contains(&fid)
                    && ir
                        .functions
                        .get(fid as usize)
                        .is_some_and(|f| f.dispatch_receiver.is_none())
                {
                    owned.insert(fid);
                    ir.classes[cid].methods.push(fid);
                    // The impl's own body now emits in this class too — walk it for nested lambdas.
                    if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                        stack.push(b);
                    }
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
    }
}

/// Lambda impl methods that a lambda's own `inline_body` CALLS. An ANONYMOUS FUNCTION cannot be
/// spliced verbatim — its `return` is LOCAL, so a copied body would return from the enclosing method —
/// and the lowerer therefore gives it an `inline_body` that is an `invokestatic` to its impl. Such an
/// impl is LIVE even though no `invokedynamic` ever references it, and must survive both the
/// must-inline dead-marking and the facade dead-lambda sweep.
fn splice_called_impls(ir: &IrFile) -> std::collections::HashSet<u32> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                inline_body: Some(body),
                ..
            } => matches!(
                &ir.exprs[*body as usize],
                IrExpr::Call { callee: Callee::Local(f), .. } if f == impl_fn
            )
            .then_some(*impl_fn),
            _ => None,
        })
        .collect()
}

pub fn mark_must_inline_lambdas(ir: &mut IrFile) {
    let spliced_as_a_call = splice_called_impls(ir);
    let mut dead: Vec<u32> = Vec::new();
    for i in 0..ir.exprs.len() {
        let args = match &ir.exprs[i] {
            IrExpr::Call {
                callee:
                    Callee::Static {
                        inline: crate::libraries::InlineKind::MustInline,
                        ..
                    },
                args,
                ..
            } => args.clone(),
            IrExpr::Call { args, .. }
                if ir
                    .module_inline_calls
                    .contains(&(u32::try_from(i).expect("IR expression index exceeds ExprId"))) =>
            {
                args.clone()
            }
            _ => continue,
        };
        for a in args {
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[a as usize] {
                if !spliced_as_a_call.contains(impl_fn) {
                    dead.push(*impl_fn);
                }
            }
        }
    }
    for fid in dead {
        ir.inline_only_fns.insert(fid);
        ir.must_inline_lambdas.insert(fid);
    }
}

pub(crate) fn emit_all_with_checked_classifiers(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    facts: CheckedEmitFacts<'_>,
    opts: &EmitOptions,
    run: &EmitRun,
) -> Option<Vec<(String, Vec<u8>)>> {
    let env = EmitEnv {
        bodies,
        run,
        continuation_metadata: facts.metadata.continuations,
        emit_time_machines: facts.metadata.emit_time_machines,
        unit_result_tail_forwards: facts.metadata.unit_result_tail_forwards,
        bridge_return_adaptations: facts.metadata.bridge_returns,
        signature_symbols: facts.signature_symbols,
        jvm_default: opts.jvm_default,
        lambda_modes: opts.lambda_modes,
        java_parameters: opts.java_parameters,
        property_realizations: facts.property_realizations,
        property_reference_realizations: facts.property_reference_realizations,
        default_call_operands: facts.default_call_operands,
        inner_classes: crate::jvm::inner_classes::InnerClasses::new(ir),
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
    let empty = std::collections::HashSet::new();
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
    let rescued: std::collections::HashSet<u32> = used
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
    let function_reference_targets: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .filter_map(|class| class.func_ref.as_ref())
        .filter_map(|reference| function_reference_target(ir, reference))
        .collect();
    let dead: std::collections::HashSet<u32> = ir
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
                && !ir.suspend_lambda_sm.iter().any(|(f2, _, _)| *f2 == fid)
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
    dead: &'a std::collections::HashSet<u32>,
    rescued: &'a std::collections::HashSet<u32>,
}

/// Find private instance calls whose caller and declaration are different JVM classes.
///
/// FIR/common IR retain Kotlin ownership and the selected member identity only. The Java-8 access
/// bridge is a physical realization, so this whole-file reachability walk belongs at the backend
/// boundary and runs once per emission pass, never once per method candidate.
fn cross_owner_private_member_calls(
    ir: &IrFile,
    facade: &str,
    class_member_fids: &std::collections::HashSet<u32>,
    private_interface_bodies_are_members: bool,
) -> std::collections::HashSet<u32> {
    let mut result = std::collections::HashSet::new();
    let mut scan = |owner: &str, roots: Vec<crate::ir::ExprId>| {
        let mut seen = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(expression) = stack.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::MethodCall { class, index, .. } = ir.expr(expression) {
                let target_class = &ir.classes[*class as usize];
                let target = target_class.methods[*index as usize];
                if target_class.fq_name() != owner
                    && (private_interface_bodies_are_members || !target_class.is_interface)
                    && ir.private_methods.contains(&target)
                {
                    result.insert(target);
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| stack.push(child));
        }
    };

    let facade_roots = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, function)| {
            !class_member_fids.contains(&(*fid as u32)) && function.dispatch_receiver.is_none()
        })
        .filter_map(|(_, function)| function.body)
        .chain(
            ir.statics
                .iter()
                .filter(|property| property.owner.is_none())
                .map(|property| property.init),
        )
        .collect();
    scan(facade, facade_roots);

    for class in &ir.classes {
        let owner = class.fq_name();
        let mut roots = class
            .methods
            .iter()
            .filter_map(|fid| {
                ir.functions
                    .get(*fid as usize)
                    .and_then(|function| function.body)
            })
            .collect::<Vec<_>>();
        for fid in &class.methods {
            if let Some(defaults) = ir
                .fn_params
                .get(fid)
                .and_then(|parameters| parameters.defaults.as_ref())
            {
                roots.extend(defaults.iter().flatten().copied());
            }
        }
        roots.extend(class.init_body);
        roots.extend(class.super_arg_prelude.iter().copied());
        roots.extend(class.super_args.iter().copied());
        roots.extend(
            class
                .properties
                .iter()
                .filter_map(|property| property.initializer),
        );
        for constructor in &class.secondary_ctors {
            roots.extend(constructor.body);
            roots.extend(constructor.defaults.iter().flatten().copied());
            roots.extend(constructor.delegate_prelude.iter().copied());
            roots.extend(constructor.delegate_args.iter().copied());
        }
        for entry in &class.enum_entries {
            roots.extend(entry.args.iter().copied());
        }
        roots.extend(
            ir.statics
                .iter()
                .filter(|property| property.owner_matches(&owner))
                .map(|property| property.init),
        );
        scan(&owner, roots);
    }
    result
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
    if !jvm_can_emit(ir) {
        crate::trace_compiler!(
            "lower",
            "JVM emission declined residual IR nodes: {:?}",
            ir.exprs
                .iter()
                .enumerate()
                .filter_map(|(expression, value)| match value {
                    IrExpr::Checked(_)
                    | IrExpr::PluginPlaceholder { .. }
                    | IrExpr::Call {
                        callee: Callee::Module { .. } | Callee::External { .. },
                        ..
                    } => Some((expression, value)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
        return None;
    }
    *env.run.inline_bail.borrow_mut() = None;
    env.run.emit_bail.set(false);
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver). A function that BELONGS
    // to a class — including a `static` member like the serialization plugin's `serializer()` accessor,
    // which has no dispatch receiver — is emitted on its class (below), NOT here; otherwise two classes'
    // same-signature statics (`C.serializer()`/`D.serializer()`) would collide on the facade.
    let class_member_fids: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    env.run
        .private_member_access_bridges
        .borrow_mut()
        .clone_from(&cross_owner_private_member_calls(
            ir,
            facade,
            &class_member_fids,
            opts.jvm_default != JvmDefaultMode::Disable,
        ));
    let mut cw = new_writer(facade, "java/lang/Object", opts);
    // The facade constructs the file's local classes, and a class that references one as a class
    // constant must list it in `InnerClasses` — reflection cross-checks the two sides and throws
    // `IncompatibleClassChangeError` when only one carries the entry. kotlinc emits it here too.
    env.inner_classes.register(&mut cw);
    // PRIVATE facade functions a CLASS body calls (`Callee::Local` from a lambda impl, a
    // continuation class, or any class member): a cross-class private invokestatic is illegal, so
    // kotlinc emits a `public static final synthetic access$<name>` forwarding bridge on the facade
    // and the class calls that (the `Callee::Local` emit arm does the routing).
    let facade_access_bridges: std::collections::HashSet<u32> = {
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                    roots.push(b);
                }
            }
            roots.extend(c.init_body);
            roots.extend(c.super_arg_prelude.iter().copied());
            roots.extend(c.super_args.iter().copied());
            for sc in &c.secondary_ctors {
                roots.extend(sc.body);
                roots.extend(sc.defaults.iter().flatten().copied());
                roots.extend(sc.delegate_prelude.iter().copied());
                roots.extend(sc.delegate_args.iter().copied());
            }
            for en in &c.enum_entries {
                roots.extend(en.args.iter().copied());
            }
        }
        let mut out = std::collections::HashSet::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let crate::ir::IrExpr::Call {
                callee: Callee::Local(fid),
                ..
            } = &ir.exprs[cur as usize]
            {
                if ir.private_methods.contains(fid) && !class_member_fids.contains(fid) {
                    out.insert(*fid);
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
        // A function-reference class dispatching to a PRIVATE facade function (its `invoke` is
        // synthesized bytecode, not IR) needs the same bridge.
        for c in &ir.classes {
            if let Some(fr) = &c.func_ref {
                if fr.call_owner_is_facade() {
                    if let Some(target) = function_reference_target(ir, fr).filter(|target| {
                        ir.private_methods.contains(target) && !class_member_fids.contains(target)
                    }) {
                        out.insert(target);
                    }
                }
            }
        }
        out
    };
    let mut facade_has_method = false;
    let mut deferred_access_bridges = Vec::new();
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
            member_schedule::FacadeMember::PropertyAccessors(static_index) => {
                static_fields::emit_static_accessors(
                    ir,
                    facade,
                    &mut cw,
                    env,
                    opts.param_assertions,
                    static_index,
                );
                continue;
            }
        };
        let f = &ir.functions[i];
        let rescued = lambdas.rescued.contains(&(i as u32));
        emit_method_maybe_rescued(ir, i as u32, facade, facade, &mut cw, false, env, rescued);
        // A facade has no class declaration to close on.
        function_debug::attach_declared_function_debug(ir, i as u32, facade, &mut cw);
        facade_has_method = true;
        // A PARAMETERLESS `fun main()` is not a JVM entry point on its own: the launcher looks for
        // `main([Ljava/lang/String;)V`, and the no-arg form is only recognized by JEP 445 (Java 21+
        // preview, final in 25). kotlinc therefore emits a synthetic bridge that calls it, so the
        // output runs on any JVM. A declared `fun main(args: Array<String>)` IS the entry point and
        // gets no bridge; a `suspend fun main` has a continuation parameter after lowering and so is
        // not parameterless here (its bridge needs the coroutine runner and is not emitted yet).
        if f.name == "main" && f.params.is_empty() && matches!(f.ret, Ty::Unit) {
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
        if facade_access_bridges.contains(&(i as u32)) {
            deferred_access_bridges.push(i as u32);
        }
        // A top-level function (or extension) with SIMPLE parameter defaults gets kotlinc's
        // `foo$default(params…, int mask, Object marker)` synthetic (dispatches to the real method,
        // filling the masked slots from the defaults), so an omitted-argument caller — same-file or
        // cross-module — resolves against the same ABI kotlinc emits. A value-class-mangled function or a
        // complex default (lambda / construction / spilled temp) is skipped (`toplevel_default_stub_safe`).
        if crate::ir::toplevel_default_stub_safe(ir, i as u32) {
            let defaults = ir.param_defaults(i as u32).unwrap();
            // A top-level function's `$default` marker is a plain `Object` (kotlinc's function ABI).
            emit_facade_default_stub(
                ir,
                i as u32,
                facade,
                &mut cw,
                defaults,
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
    // kotlinc's SyntheticAccessorLowering appends each `access$<name>` bridge to the facade after
    // every declared and lifted member.
    for function in deferred_access_bridges {
        access_bridges::emit_facade_function_access_bridge(ir, function, facade, &mut cw);
    }
    static_fields::emit_statics(ir, facade, &mut cw, env);
    // kotlinc emits the `<File>Kt` facade class ONLY when the file has top-level callables/properties
    // (or a facade `@Metadata` payload). A file of only classes/objects gets no facade — emitting an
    // empty one is an ABI divergence (spurious extra class). A facade static is owner-less.
    let facade_has_static = ir.statics.iter().any(|s| s.is_facade_owned());
    let facade_needed = facade_has_method || facade_has_static || metadata.is_some();
    if facade_needed {
        if let Some(m) = metadata {
            cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
        }
        let (bytes, coroutines) = cw.finish_with_coroutines();
        env.run.record_transformed_coroutines(coroutines);
        out.push((facade.to_string(), bytes));
        out.extend(drain_lambda_classes(env, opts));
        out.extend(env.run.machine_classes.borrow_mut().drain(..));
    }
    // Each class — with its optional `@Metadata` (the provider returns `None` for the default emit).
    for c in &ir.classes {
        let fq_name = c.fq_name();
        // A function whose suspension points all turned out to be tail calls has no machine.
        if let Some(crate::jvm::classfile::CoroutineOutcome::TailCalls) =
            env.run.transformed_coroutine(&fq_name)
        {
            continue;
        }
        let cm = class_meta(&fq_name);
        let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
        out.push((
            fq_name,
            emit_class(ir, c, facade, env, opts, cm.as_ref(), &mut extra),
        ));
        // An interface's `$DefaultImpls` holder (its `name$default` synthetics), when any exist.
        out.extend(extra);
        out.extend(drain_lambda_classes(env, opts));
        // A member's coroutine machine builds its continuation class while the method is emitted,
        // which is after the facade drained the ones its own top-level functions produced.
        out.extend(env.run.machine_classes.borrow_mut().drain(..));
    }
    out.extend(drain_lambda_classes(env, opts));
    out.extend(env.run.machine_classes.borrow_mut().drain(..));
    if env.run.inline_bail.borrow().is_some() {
        return None;
    }
    if env.run.emit_bail.get() {
        return None; // a value slot was never allocated (malformed IR) — skip, never miscompile
    }
    Some(out)
}

/// Write one synthetic lambda class for [`LambdaMode::Class`].
///
/// Shape, as kotlinc emits it: `final class Outer$fn$N extends kotlin/jvm/internal/Lambda implements
/// FunctionN`, whose constructor passes the source arity to `Lambda.<init>(I)V`. A non-capturing
/// lambda additionally gets a `static final INSTANCE` initialized in `<clinit>`; a capturing one
/// stores each captured value in a field and reads it back in `invoke`.
///
/// `invoke` is emitted at the interface's ERASED descriptor only — that is the slot the JVM
/// dispatches through, so the specialized overload kotlinc also emits is not required for
/// correctness. Arguments are unboxed into the implementation's physical parameter types and the
/// result reboxed, exactly as the metafactory's adapter would have done under `indy`.
fn build_lambda_class(plan: &LambdaClassPlan, opts: &EmitOptions) -> (String, Vec<u8>) {
    let super_name = if plan.kotlin_function {
        "kotlin/jvm/internal/Lambda"
    } else {
        "java/lang/Object"
    };
    let mut cw = new_writer(&plan.internal, super_name, opts);
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
    if plan.kotlin_function {
        ctor.push_int(plan.arity as i32, &mut cw);
        let sup = cw.methodref(super_name, "<init>", "(I)V");
        ctor.invokespecial(sup, 1, 0);
    } else {
        let sup = cw.methodref(super_name, "<init>", "()V");
        ctor.invokespecial(sup, 0, 0);
    }
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
    let high_arity = plan.kotlin_function && is_high_arity_function(plan.arity as u8);
    if high_arity {
        // FunctionN's single erased parameter is `Object[]`; unpack each semantic argument in order
        // and adapt it to the existing implementation method's physical slot.
        for (index, want) in own_params.iter().copied().enumerate() {
            invoke.aload(arg_slots[0]);
            invoke.push_int(index as i32, &mut cw);
            invoke.array_load(0x32, 1); // aaload
            let wanted = type_descriptor(want);
            if !matches!(want, Ty::Unit) && !descriptor_is_reference(&wanted) {
                unbox_prim_from(&mut cw, &mut invoke, Ty::obj("java/lang/Object"), want);
            } else if wanted != "Ljava/lang/Object;" {
                let internal = wanted
                    .strip_prefix('L')
                    .and_then(|rest| rest.strip_suffix(';'))
                    .map(str::to_owned)
                    .unwrap_or(wanted);
                let target = cw.class_ref(&internal);
                invoke.checkcast(target);
            }
        }
    } else {
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
                    // `Function1.invoke` declares `Object`; the body declares the real type. Under
                    // `indy` the metafactory's adapter inserted this cast — nothing else does here,
                    // and without it the class fails verification on a specific reference parameter.
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

    if plan.function_adapter {
        assert_eq!(
            plan.captures.len(),
            1,
            "a FunctionAdapter SAM captures exactly its callable reference"
        );
        let delegate_field = cw.fieldref(&plan.internal, "$captured$0", &field_descs[0]);

        let mut delegate = CodeBuilder::new(1);
        delegate.aload(0);
        delegate.getfield(delegate_field, 1);
        delegate.areturn();
        cw.add_method(
            0x0011,
            "getFunctionDelegate",
            "()Lkotlin/Function;",
            &delegate,
        );

        let mut equals = CodeBuilder::new(2);
        let not_same = equals.new_label();
        let adapter = equals.new_label();
        equals.aload(0);
        equals.aload(1);
        equals.if_acmpne(not_same);
        equals.push_int(1, &mut cw);
        equals.ireturn();
        equals.bind(not_same);
        equals.aload(1);
        let function_adapter = cw.class_ref("kotlin/jvm/internal/FunctionAdapter");
        equals.instance_of(function_adapter);
        equals.ifne(adapter);
        equals.push_int(0, &mut cw);
        equals.ireturn();
        equals.bind(adapter);
        equals.aload(0);
        equals.getfield(delegate_field, 1);
        equals.aload(1);
        equals.checkcast(function_adapter);
        let get_delegate = cw.interface_methodref(
            "kotlin/jvm/internal/FunctionAdapter",
            "getFunctionDelegate",
            "()Lkotlin/Function;",
        );
        equals.invokeinterface(get_delegate, 0, 1);
        let object_equals = cw.methodref("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
        equals.invokevirtual(object_equals, 1, 1);
        equals.ireturn();
        equals.link();
        cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &equals);

        let mut hash_code = CodeBuilder::new(1);
        hash_code.aload(0);
        hash_code.getfield(delegate_field, 1);
        let object_hash = cw.methodref("java/lang/Object", "hashCode", "()I");
        hash_code.invokevirtual(object_hash, 0, 1);
        hash_code.ireturn();
        cw.add_method(0x0011, "hashCode", "()I", &hash_code);
    }

    if plan.captures.is_empty() {
        let instance_desc = format!("L{};", plan.internal);
        cw.add_field(0x0019, "INSTANCE", &instance_desc); // ACC_PUBLIC | ACC_STATIC | ACC_FINAL
        let mut clinit = CodeBuilder::new(0);
        let class_index = cw.class_ref(&plan.internal);
        clinit.new_obj(class_index);
        clinit.dup();
        let ctor_ref = cw.methodref(&plan.internal, "<init>", "()V");
        clinit.invokespecial(ctor_ref, 0, 0);
        let field = cw.fieldref(&plan.internal, "INSTANCE", &instance_desc);
        clinit.putstatic(field, 1);
        clinit.ret_void();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit); // ACC_STATIC
    }
    (plan.internal.clone(), cw.finish())
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

/// Whether `fid` is the implementation selected for a class-realized closure. This consumes the
/// explicit IR edge from a lambda to its implementation; generated method spelling is never used as
/// identity, and mixed `-Xlambdas`/`-Xsam-conversions` modes select only the matching closure kind.
fn lambda_impl_uses_class_strategy(ir: &IrFile, fid: u32, modes: LambdaModes) -> bool {
    ir.exprs.iter().any(|expression| {
        let IrExpr::Lambda {
            impl_fn,
            arity,
            sam,
            ..
        } = expression
        else {
            return false;
        };
        *impl_fn == fid
            && (sam.as_ref().is_some_and(|target| target.function_adapter)
                || modes.for_sam(sam.is_some()) == LambdaMode::Class
                || (sam.is_none() && is_high_arity_function(*arity)))
    })
}

/// Attach any user annotations recorded for `field` (by name) to the most recently added field.
/// The annotations on a property's synthetic `$annotations` marker — the PROPERTY's own, which
/// `@Metadata` records as `Property.annotation`. Both retentions rejoin into the single list a
/// metadata record keeps.
fn property_marker_annotations(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    property: &str,
) -> Vec<crate::ir::AppliedAnnotation> {
    let Some(marker) = ir
        .property_annotation_markers
        .get(&(c.fq_name_id(), property.to_string()))
    else {
        return Vec::new();
    };
    ir.function_annotations
        .get(marker)
        .map(|annotations| annotations.applications().cloned().collect())
        .unwrap_or_default()
}

/// `(name, descriptor)` of that marker, read from the emitted function so a value-class-mangled
/// getter's marker is named as the class file spells it.
fn property_marker_signature(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    property: &str,
) -> Option<(String, String)> {
    let marker = ir
        .property_annotation_markers
        .get(&(c.fq_name_id(), property.to_string()))?;
    let function = ir.functions.get(*marker as usize)?;
    Some((function.name.clone(), "()V".to_string()))
}

/// The annotations that landed on the property's BACKING FIELD (`@Target(FIELD)`) — the same records
/// the field's own class-file attribute carries, mirrored as `Property.backingFieldAnnotation`.
fn property_backing_field_annotations(
    c: &crate::ir::IrClass,
    property: &str,
) -> Vec<crate::ir::AppliedAnnotation> {
    c.field_annotations
        .iter()
        .find(|annotations| annotations.field == property)
        .map(|annotations| annotations.annotations.applications().cloned().collect())
        .unwrap_or_default()
}

/// Is this property declared `@JvmField`? The annotation replaces the property's JVM realization
/// wholesale: kotlinc emits NO `getX()`/`setX()` for it and gives the backing field the PROPERTY's
/// declared visibility, so every read and write — inside the class and out — is a field access, and
/// the `@Metadata` record describes only the field.
///
/// Read off the resolved application rather than the spelling: `@JvmField` reaches the FIELD use
/// site by its own declared `@Target` (see `class_field_annotations`), so it is already interned
/// here under its exact identity, and an unrelated user annotation that happens to be spelled
/// `JvmField` resolves to a different one.
///
/// A companion is excluded here because the companion-storage pass hoists its field to the enclosing
/// classifier. A named object's `@JvmField`, by contrast, is a public static on that object class and
/// uses this rule together with the object's backend-selected static storage.
fn is_jvm_field(c: &crate::ir::IrClass, property: &str) -> bool {
    !c.is_companion && c.property_has_jvm_field(property)
}

fn jvm_field_visibility(c: &crate::ir::IrClass, property: &str) -> Option<u16> {
    is_jvm_field(c, property)
        .then(|| {
            c.properties
                .iter()
                .find(|declaration| declaration.name == property)
        })
        .flatten()
        .map(|declaration| match declaration.visibility {
            crate::types::Visibility::Protected => 0x0004,
            crate::types::Visibility::Private => 0x0002,
            _ => 0x0001,
        })
}

fn apply_enum_entry_annotations(cw: &mut ClassWriter, c: &crate::ir::IrClass, field: &str) {
    if let Some(annotations) = c.field_annotations.iter().find(|a| a.field == field) {
        cw.set_last_field_annotations_deferred(&annotations.annotations);
    }
}

pub(crate) fn jvm_can_emit(ir: &IrFile) -> bool {
    fn ty_ok(t: &Ty) -> bool {
        match t.non_null() {
            Ty::Fun(s) => s.params.iter().all(ty_ok) && ty_ok(&s.ret),
            Ty::Obj(_, type_args) => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    fn callee_ok(callee: &Callee) -> bool {
        match callee {
            Callee::Static { .. } | Callee::Special { .. } => true,
            // Realized into `Callee::Special` before emission.
            Callee::Super { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            // A user (sibling-file) method carries `Ty`s; a classpath one a descriptor string.
            Callee::Virtual { params, .. } => match params {
                Some((ps, ret)) => ps.iter().all(ty_ok) && ty_ok(ret),
                None => true,
            },
            Callee::CrossFile { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            Callee::Module { .. }
            | Callee::ModuleWithDefaults { .. }
            | Callee::LocalWithDefaults { .. }
            | Callee::ClassStaticWithDefaults { .. }
            | Callee::External { .. } => false,
            Callee::Local(_)
            | Callee::LocalDefault(_)
            | Callee::ClassStatic { .. }
            | Callee::ClassStaticDefault { .. }
            | Callee::Intrinsic { .. } => true,
        }
    }
    if ir
        .functions
        .iter()
        .any(|f| !ty_ok(&f.ret) || !f.params.iter().all(ty_ok))
    {
        return false;
    }
    if ir.statics.iter().any(|s| !ty_ok(&s.ty)) {
        return false;
    }
    ir.exprs.iter().all(|e| match e {
        IrExpr::Lambda { .. } => true,
        IrExpr::Variable { ty, .. } => ty_ok(ty),
        IrExpr::Call { callee, .. } => callee_ok(callee),
        // Every other checked operation must have been consumed by its JVM realization pass.
        IrExpr::Checked(_) => false,
        // A plugin placeholder that reached emit means its owning plugin didn't run (or couldn't
        // specialize it) — decline the file rather than miscompile (the node has no JVM lowering).
        IrExpr::PluginPlaceholder { .. } => false,
        _ => true,
    })
}

/// The internal class name to `checkcast` a value to when narrowing an erased `Object` to `ty` — or
/// `None` when no narrowing is needed (`Object`/`Any`, a primitive, `Unit`/`Nothing`).
fn checkcast_internal(ty: Ty) -> Option<String> {
    match ty {
        Ty::String => Some("java/lang/String".to_string()),
        _ if ty.is_array() => Some(type_descriptor(ty)),
        Ty::Obj(n, _) if n != "java/lang/Object" && n != "kotlin/Any" => {
            Some(crate::jvm::names::classfile_internal_name(&n.render()))
        }
        _ => None,
    }
}

/// JVM type exposed by a synthesized property accessor. The value-class pass stamps the exact
/// mangled getter only when this property's own type uses its erased field carrier. Consume that
/// decision directly; emission must not repeat classifier/value-class lookup. An explicit backing
/// field with a narrower type does not otherwise change the property's public descriptor.
fn declared_property_accessor_jvm(
    _ir: &IrFile,
    property: &crate::ir::IrProperty,
    field: &crate::ir::IrField,
) -> Ty {
    if property.getter_jvm_name.is_some() {
        jvm_declared_ty(&field.ty)
    } else {
        jvm_declared_ty(&stored_value_ty(property.ty))
    }
}

/// Adapt the physical backing-field value already on the stack to the property's declared return.
/// Resolution has already chosen both types; this is only their JVM representation boundary.
fn emit_backing_field_read_adaptation(
    ir: &IrFile,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::IrProperty,
    field_jvm: Ty,
    accessor_jvm: Ty,
) {
    if field_jvm == accessor_jvm {
        return;
    }
    if field_jvm.is_reference() && accessor_jvm.is_jvm_scalar() {
        unbox_prim_from(cw, code, field_jvm, accessor_jvm);
    } else if accessor_jvm.is_reference() {
        if let Some(storage) = property
            .storage_ty
            .and_then(|ty| ty.non_null().obj_internal())
        {
            if ir.is_value_class_name(storage) && field_jvm.is_jvm_scalar() {
                emit_box_impl(ir, cw, &Ty::obj_name(storage), code);
                return;
            }
        }
        if field_jvm.is_jvm_scalar() {
            box_prim_free(cw, code, field_jvm);
        } else {
            let internal = crate::jvm::names::instanceof_internal_name(accessor_jvm);
            if internal != "java/lang/Object" {
                let class = cw.class_ref(&internal);
                code.checkcast(class);
            }
        }
    }
}

/// Adapt a synthesized setter's declared argument to the physical backing-field representation.
fn emit_backing_field_write_adaptation(
    ir: &IrFile,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::IrProperty,
    accessor_jvm: Ty,
    field_jvm: Ty,
) {
    if accessor_jvm == field_jvm {
        return;
    }
    if accessor_jvm.is_reference() && field_jvm.is_jvm_scalar() {
        if let Some(storage) = property
            .storage_ty
            .and_then(|ty| ty.non_null().obj_internal())
        {
            if ir.is_value_class_name(storage) {
                let class = cw.class_ref(&storage.render());
                code.checkcast(class);
                emit_unbox_impl(ir, cw, &Ty::obj_name(storage), code);
                return;
            }
        }
        unbox_prim_from(cw, code, accessor_jvm, field_jvm);
    } else if accessor_jvm.is_jvm_scalar() && field_jvm.is_reference() {
        box_prim_free(cw, code, accessor_jvm);
    } else if accessor_jvm.is_reference() && field_jvm.is_reference() {
        let internal = crate::jvm::names::instanceof_internal_name(field_jvm);
        if internal != "java/lang/Object" {
            let class = cw.class_ref(&internal);
            code.checkcast(class);
        }
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
enum PropertyAccessorSide {
    Getter,
    Setter,
}

fn emit_declared_property_accessor(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    property: &crate::ir::IrProperty,
    side: PropertyAccessorSide,
    fq_name: &str,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    param_assertions: bool,
) {
    // A private property reached from outside (an `inline` body spliced into its caller) needs the
    // synthetic accessor kotlinc emits for exactly this: `access$get<X>$p(<owner>)<ty>`.
    if matches!(side, PropertyAccessorSide::Getter) && property.needs_access_bridge {
        if let Some(field) = property
            .backing_field
            .and_then(|i| c.fields.get(i as usize))
        {
            let field_jt = jvm_declared_ty(&field.ty);
            let field_desc = type_descriptor(field_jt);
            let accessor_jt = declared_property_accessor_jvm(ir, property, field);
            let accessor_desc = type_descriptor(accessor_jt);
            let name = format!(
                "access${}$p",
                crate::names::property_getter_name(&property.name)
            );
            let bridge_getter_desc = format!("(L{fq_name};){accessor_desc}");
            cw.reserve_method_pool(&name, &bridge_getter_desc, None, &[]);
            let mut g = CodeBuilder::new(1);
            g.aload(0);
            // A property that declares its own getter must be read THROUGH it — the bridge exists to
            // reach the property, not to bypass the user's accessor.
            match property.getter.map(|fid| &ir.functions[fid as usize]) {
                Some(f) => {
                    let d = ir_method_desc(&[], &f.ret);
                    let m = cw.methodref(fq_name, &f.name, &d);
                    g.invokevirtual(m, 0, slot_words(accessor_jt) as i32);
                }
                None => {
                    let physical_name = instance_field_jvm_name(ir, c, field);
                    let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
                    g.getfield(fref, slot_words(field_jt) as i32);
                    emit_backing_field_read_adaptation(
                        ir,
                        cw,
                        &mut g,
                        property,
                        field_jt,
                        accessor_jt,
                    );
                }
            }
            emit_return(accessor_jt, &mut g);
            g.ensure_locals(1);
            g.link();
            cw.add_method(
                0x1009, /* PUBLIC | STATIC | SYNTHETIC */
                &name,
                &bridge_getter_desc,
                &g,
            );
            if property.is_var {
                let setter_name = format!(
                    "access${}$p",
                    crate::names::property_setter_name(&property.name)
                );
                let bridge_setter_desc = format!("(L{fq_name};{accessor_desc})V");
                cw.reserve_method_pool(&setter_name, &bridge_setter_desc, None, &[]);
                let words = slot_words(accessor_jt);
                let mut st = CodeBuilder::new(1 + words);
                st.aload(0);
                load(accessor_jt, 1, &mut st);
                // Same rule for the write: a declared setter is user code and is never bypassed.
                match property.setter.map(|fid| &ir.functions[fid as usize]) {
                    Some(f) => {
                        let d = method_descriptor(&[jvm_declared_ty(&f.params[0])], Ty::Unit);
                        let m = cw.methodref(fq_name, &f.name, &d);
                        st.invokevirtual(m, words as i32, 0);
                    }
                    None => {
                        emit_backing_field_write_adaptation(
                            ir,
                            cw,
                            &mut st,
                            property,
                            accessor_jt,
                            field_jt,
                        );
                        let physical_name = instance_field_jvm_name(ir, c, field);
                        let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
                        st.putfield(fref, slot_words(field_jt) as i32);
                    }
                }
                st.ret_void();
                st.ensure_locals(1 + words);
                st.link();
                cw.add_method(0x1009, &setter_name, &bridge_setter_desc, &st);
            }
        }
    }
    // `@JvmField` IS the declaration's realization: the field is the property's public face and
    // kotlinc emits no accessor beside it. Synthesizing one here would advertise a method the
    // metadata (correctly) never records.
    if property.is_private || is_jvm_field(c, &property.name) {
        return;
    }
    let Some(field_index) = property.backing_field else {
        return;
    };
    let Some(field) = c.fields.get(field_index as usize) else {
        return;
    };
    let type_parameter = ir
        .field_signatures(fq_name)
        .and_then(|signatures| {
            signatures
                .iter()
                .find(|(name, _)| *name == field.name)
                .map(|(_, parameter)| parameter.as_str())
        })
        .or(field.type_param.as_deref());
    let signatures = property_jvm_signatures(formatter, &field.ty, type_parameter);
    let field_jt = jvm_declared_ty(&field.ty);
    let field_desc = type_descriptor(field_jt);
    let accessor_jt = declared_property_accessor_jvm(ir, property, field);
    let accessor_desc = type_descriptor(accessor_jt);
    // Only an `open`/`override` PROPERTY's accessor is overridable — a plain `val` on an open
    // class keeps its FINAL accessor (kotlinc: `open class Engine(val name: String)` emits
    // `public final getName()`); Kotlin rejects overriding a non-open property, so the flag is
    // safe. Interface accessors stay non-final (their default bodies dispatch virtually).
    let overridable = property.is_open || c.is_interface;
    let getter = property
        .getter_jvm_name
        .clone()
        .unwrap_or_else(|| crate::names::property_getter_name(&property.name));
    let occupied = |name: &str, descriptor: &str| {
        c.methods.iter().any(|&fid| {
            let function = &ir.functions[fid as usize];
            function.name == name && ir_method_desc(&function.params, &function.ret) == descriptor
        })
    };
    let getter_desc = format!("(){accessor_desc}");
    if matches!(side, PropertyAccessorSide::Getter) && !occupied(&getter, &getter_desc) {
        // Visit the method header before constructing its code. This is especially observable for a
        // setter guard (`<set-?>`) and for a generic accessor Signature.
        let sig = &signatures.getter;
        let getter_ann = (accessor_jt.is_reference() && field.type_param.is_none()).then(|| {
            if property.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        cw.reserve_method_pool(
            &getter,
            &getter_desc,
            sig.as_deref(),
            &getter_ann.into_iter().collect::<Vec<_>>(),
        );
        let mut g = CodeBuilder::new(1);
        let physical_name = instance_field_jvm_name(ir, c, field);
        let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
        if static_storage(ir, c) {
            g.getstatic(fref, slot_words(field_jt) as i32);
        } else {
            g.aload(0);
            g.getfield(fref, slot_words(field_jt) as i32);
        }
        // A `lateinit var` read throws while the field is still null — kotlinc inserts this at every
        // access, and the accessor is an access like any other.
        if field.is_lateinit() {
            g.dup();
            let lbl = g.new_label();
            g.ifnonnull(lbl);
            g.push_string(&field.name, cw);
            let m = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "throwUninitializedPropertyAccessException",
                "(Ljava/lang/String;)V",
            );
            g.invokestatic(m, 1, 0);
            // The join needs a stackmap frame: `this` in local 0, the (non-null on the taken path)
            // field value on the stack.
            g.bind(lbl);
        }
        emit_backing_field_read_adaptation(ir, cw, &mut g, property, field_jt, accessor_jt);
        emit_return(accessor_jt, &mut g);
        g.ensure_locals(1);
        g.link();
        let access = if overridable { 0x0001 } else { 0x0011 };
        cw.add_method_sig(access, &getter, &getter_desc, &g, sig.as_deref());
    }
    if matches!(side, PropertyAccessorSide::Setter) && property.is_var {
        let setter = property
            .setter_jvm_name
            .clone()
            .unwrap_or_else(|| crate::names::property_setter_name(&property.name));
        let setter_desc = format!("({accessor_desc})V");
        if !occupied(&setter, &setter_desc) {
            let sig = &signatures.setter;
            let setter_ann =
                (accessor_jt.is_reference() && field.type_param.is_none()).then(|| {
                    if property.ty.is_nullable() {
                        "Lorg/jetbrains/annotations/Nullable;"
                    } else {
                        "Lorg/jetbrains/annotations/NotNull;"
                    }
                });
            cw.reserve_method_pool(
                &setter,
                &setter_desc,
                sig.as_deref(),
                &setter_ann.into_iter().collect::<Vec<_>>(),
            );
            // `<set-?>` is the setter value parameter's JVM debug name even when no non-null guard
            // uses it as a String constant. Its UTF8 belongs to this method's header/debug window,
            // before the following declared member.
            cw.seed_utf8("<set-?>");
            let words = slot_words(accessor_jt);
            let mut st = CodeBuilder::new(1 + words);
            // kotlinc guards a non-null REFERENCE setter parameter, naming it `<set-?>`. A primitive
            // cannot be null, and neither is a type parameter that admits null (an unbounded `<T>` is
            // `Any?`); a NON-null-bounded one (`<T : Cargo>`) is guarded like any other reference.
            let guarded = param_assertions
                && accessor_jt.is_reference()
                && !property.ty.is_nullable()
                && is_nonnull_reference_field(ir, fq_name, &field.name, field.ty);
            if guarded {
                st.aload(1);
                st.push_string("<set-?>", cw);
                let m = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                st.invokestatic(m, 2, 0);
            }
            let statics_storage = static_storage(ir, c);
            if !statics_storage {
                st.aload(0);
            }
            load(accessor_jt, 1, &mut st);
            emit_backing_field_write_adaptation(ir, cw, &mut st, property, accessor_jt, field_jt);
            let physical_name = instance_field_jvm_name(ir, c, field);
            let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
            if statics_storage {
                st.putstatic(fref, slot_words(field_jt) as i32);
            } else {
                st.putfield(fref, slot_words(field_jt) as i32);
            }
            st.ret_void();
            st.ensure_locals(1 + words);
            st.link();
            // `private set` narrows only the setter. Accessor synthesis owns the method flags now,
            // so it must preserve that declaration fact instead of widening the setter to public.
            let access = if property.setter_is_private {
                0x0012 // PRIVATE | FINAL
            } else if overridable {
                0x0001
            } else {
                0x0011 // PUBLIC | FINAL
            };
            cw.add_method_sig(access, &setter, &setter_desc, &st, sig.as_deref());
        }
    }
}

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
    for property in &c.properties {
        emit_declared_property_accessor(
            ir,
            c,
            property,
            PropertyAccessorSide::Getter,
            fq_name,
            cw,
            formatter,
            param_assertions,
        );
        emit_declared_property_accessor(
            ir,
            c,
            property,
            PropertyAccessorSide::Setter,
            fq_name,
            cw,
            formatter,
            param_assertions,
        );
        // The property's own annotations ride a synthetic marker method, emitted right here so the
        // method table (and the constant pool behind it) matches kotlinc's.
        if let Some(&marker) = ir
            .property_annotation_markers
            .get(&(c.fq_name_id(), property.name.clone()))
        {
            // The marker is STATIC (kotlinc's shape): no `this` slot.
            emit_method(ir, marker, fq_name, facade, cw, false, env);
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
    markers: &'a std::collections::HashSet<u32>,
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
    } = *emission;
    let fid = match member {
        SourceOrderedMember::Property(property) => {
            emit_declared_property_accessor(
                ir,
                c,
                property,
                PropertyAccessorSide::Getter,
                fq_name,
                cw,
                signature_formatter,
                param_assertions,
            );
            if let Some(getter) = property.getter {
                emit_scheduled_member(emission, SourceOrderedMember::Function(getter), cw);
            }
            emit_declared_property_accessor(
                ir,
                c,
                property,
                PropertyAccessorSide::Setter,
                fq_name,
                cw,
                signature_formatter,
                param_assertions,
            );
            if let Some(setter) = property.setter {
                emit_scheduled_member(emission, SourceOrderedMember::Function(setter), cw);
            }
            // The property's own annotations ride a synthetic marker method, which kotlinc emits
            // directly after that property's accessors — it has no source order of its own.
            if let Some(&marker) = ir
                .property_annotation_markers
                .get(&(c.fq_name_id(), property.name.clone()))
            {
                // The marker is STATIC (kotlinc's shape): no `this` slot.
                emit_method(ir, marker, fq_name, facade, cw, false, env);
            }
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
        emit_method(ir, fid, fq_name, facade, cw, !f.is_static, env);
        if env
            .run
            .private_member_access_bridges
            .borrow()
            .contains(&fid)
        {
            access_bridges::emit_private_member_access_bridge(ir, fid, fq_name, cw, false);
        }
        if ir.function_reference_access_bridges.contains(&fid) {
            access_bridges::emit_function_reference_access_bridge(ir, fid, fq_name, cw, false);
        }
        bridge_emission::emit_value_class_interface_entries(
            ir,
            c,
            cw,
            fid,
            signature_formatter,
            env.bridge_return_adaptations,
            env.run,
        );
    } else {
        cw.add_abstract_method_sig(
            0x0001 | 0x0400,
            &f.name,
            &ir_method_desc(&f.params, &f.ret),
            method_signature(signature_formatter, ir, fid, f).as_deref(),
        );
    }
    // A method with default-valued parameters gets a `<name>$default(…, mask, marker)` synthetic stub
    // (the JVM realization of default arguments). A STATIC method (a value class's `constructor-impl`)
    // has no `self`, so it uses the facade-style stub keyed on the class as owner; an instance member
    // uses the self-carrying variant.
    if let Some(defaults) = ir.param_defaults(fid) {
        if f.is_static {
            // A constructor's `$default` marker is `DefaultConstructorMarker` (kotlinc's ctor ABI),
            // NOT the plain `Object` a function `$default` uses — the value class's `constructor-impl`.
            emit_facade_default_stub(
                ir,
                fid,
                fq_name,
                cw,
                defaults,
                env,
                if f.name == "constructor-impl" && !ir.class_static_local_functions.contains(&fid) {
                    Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker")
                } else {
                    Ty::obj("java/lang/Object")
                },
            );
        } else {
            emit_default_stub(ir, fid, fq_name, facade, cw, defaults, env, false);
        }
    }
}

/// The `$annotations` marker methods emitted with their property's accessors, by fid. kotlinc emits a
/// marker directly after the accessors of the property it describes, not after every property's — so
/// the class's method loop must skip a marker it already wrote here.
fn property_annotation_marker_fids(
    ir: &IrFile,
    c: &crate::ir::IrClass,
) -> std::collections::HashSet<u32> {
    c.properties
        .iter()
        .filter_map(|property| {
            ir.property_annotation_markers
                .get(&(c.fq_name_id(), property.name.clone()))
                .copied()
        })
        .collect()
}

/// A local, anonymous or generated class's `EnclosingMethod`: the JVM class its scope belongs to,
/// and the method when that scope is one, as `(name, descriptor)`. kotlinc's rule: a function (a
/// local function being its own) names itself; a top-level property initializer names the file
/// facade with no method; a classifier's initializer names its primary `<init>`, or no method when
/// the classifier's storage is static (an object, or a companion whose fields its outer class
/// holds, and so its outer class).
fn class_enclosure(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    match c.enclosure? {
        crate::ir::IrEnclosure::Function(function) => function_enclosure(ir, function, facade),
        crate::ir::IrEnclosure::PropertyAccessor {
            property,
            setter: is_setter,
        } => {
            let layout = ir.local_property_layouts.get(&property).unwrap_or_else(|| {
                panic!("property accessor enclosure has no finalized property realization")
            });
            let function = match layout {
                crate::ir::IrLocalPropertyLayout::TopLevelStorage {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        *getter
                    }
                }
                crate::ir::IrLocalPropertyLayout::TopLevelAccessor {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        Some(*getter)
                    }
                }
                crate::ir::IrLocalPropertyLayout::Member {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        *getter
                    }
                }
                crate::ir::IrLocalPropertyLayout::MemberExtension {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        Some(*getter)
                    }
                }
            }
            .unwrap_or_else(|| panic!("source accessor enclosure has no emitted accessor"));
            function_enclosure(ir, function, facade)
        }
        crate::ir::IrEnclosure::Constructor { class, ordinal } => {
            let declaration = &ir.classes[class as usize];
            let mut parameters = if ordinal == 0 {
                class_ctor_jvm_tys(declaration)
            } else {
                let constructor = declaration
                    .secondary_ctors
                    .get(ordinal.saturating_sub(1) as usize)
                    .unwrap_or_else(|| panic!("constructor enclosure has no declared constructor"));
                jvm_tys(
                    &constructor
                        .prefix_params
                        .iter()
                        .chain(&constructor.params)
                        .copied()
                        .collect::<Vec<_>>(),
                )
            };
            if !declaration.enum_entries.is_empty() {
                parameters.splice(0..0, [Ty::String, Ty::Int]);
            }
            Some((
                declaration.fq_name(),
                Some((
                    "<init>".to_string(),
                    method_descriptor(&parameters, Ty::Unit),
                )),
            ))
        }
        crate::ir::IrEnclosure::File => Some((facade.to_string(), None)),
        crate::ir::IrEnclosure::ClassInitializer(class) => {
            let class = &ir.classes[class as usize];
            if class.is_companion {
                let outer = ir
                    .classes
                    .iter()
                    .find(|outer| outer.companion_class == Some(class.fq_name))?;
                let holder = if is_jvm_interface(outer) {
                    class
                } else {
                    outer
                };
                return Some((holder.fq_name(), None));
            }
            if static_storage(ir, class) {
                return Some((class.fq_name(), None));
            }
            class.has_primary_ctor.then(|| {
                let descriptor = method_descriptor(&class_ctor_jvm_tys(class), Ty::Unit);
                (class.fq_name(), Some(("<init>".to_string(), descriptor)))
            })
        }
    }
}

fn function_enclosure(
    ir: &IrFile,
    function: crate::ir::FunId,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    let declaration = &ir.functions[function as usize];
    let owner = declaration
        .dispatch_receiver
        .map(TypeName::render)
        .unwrap_or_else(|| facade.to_string());
    let descriptor = function_descriptor(ir, function);
    Some((owner, Some((declaration.name.clone(), descriptor))))
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
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> Vec<u8> {
    assert_determined_member_signatures(ir, c);
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade, env, opts);
    }
    if let Some(iface) = &c.annotation_impl_of {
        return emit_annotation_impl_class(ir, c, &iface.render(), facade, env, opts);
    }
    if c.is_annotation {
        return emit_annotation_class(ir, c, facade, env, opts, class_meta);
    }
    if c.is_interface {
        return emit_interface_class(ir, c, facade, env, opts, class_meta, extra);
    }
    if let Some(user_tys) = &c.enum_entry_of {
        return enum_entry_subclass::emit_enum_entry_subclass(ir, c, facade, env, opts, user_tys);
    }
    if c.prop_ref.is_some() {
        return emit_prop_ref_class(c, facade, env, opts);
    }
    if c.func_ref.is_some() {
        return emit_func_ref_class(ir, c, facade, env, opts);
    }
    let fq_name = c.fq_name();
    let superclass = c.superclass();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let mut cw = new_classifier_writer(ir, c, &superclass, env, opts);
    // A LOCAL or ANONYMOUS class carries kotlinc's `EnclosingMethod` attribute: without it
    // reflection reads the class as top-level and `simpleName` reports the whole `owner$Local` name.
    // Lowering records the exact semantic scope; the backend only realizes its physical owner and
    // descriptor here.
    if let Some((owner, method)) = class_enclosure(ir, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
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
    register_sealed_subtypes(
        &mut cw,
        ir,
        c,
        opts.class_major.unwrap_or(MAJOR_JAVA8) >= 61,
    );
    env.inner_classes.register(&mut cw);
    // The class HEADER's interface refs intern BEFORE any member entry (kotlinc visits the header
    // first — `object Fast : Factory` pool: this, super, `lib/Factory`, then `<init>`), so add them
    // ahead of the pool seeding below.
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }
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
    let has_continuation_receiver = c.fields.iter().any(|field| field.name == "this$0");
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
        .or_else(|| class_ctor_generic_sig(&signature_formatter, ir, c, &fq_name));
    let byte_parity = !is_coroutine_state_machine(c)
        && opts.emit_class_metadata
        && build_class_metadata(ir, c, opts).is_some();
    let pool_seed = || PlainClassPoolSeed {
        formatter: &signature_formatter,
        ir,
        bodies: env.bodies,
        class: c,
        fq_name: &fq_name,
        ctor_signature: ctor_signature.as_deref(),
    };
    if byte_parity {
        seed_plain_class_pool(pool_seed(), &mut cw);
    }
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
        cw.add_field_late_leading(
            0x0019,
            companion.nested_segment_ref(),
            &format!("L{};", companion.render()),
        );
    }
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    // Backing fields are private; access goes through the synthesized `getX()`/`setX()` accessors
    // (kotlinc does the same) — for both normal classes and objects.
    // A static-storage object's field table leads with INSTANCE (kotlinc's order) — its backing
    // fields are added in the object block below, after the INSTANCE field.
    let mut field_order: Vec<(usize, &crate::ir::IrField)> = if static_storage(ir, c) {
        Vec::new()
    } else {
        c.fields.iter().enumerate().collect()
    };
    // kotlinc appends captures and the outer instance (the constructor prefix) after declared fields.
    let prefix = (c.constructor_prefix_count as usize).min(field_order.len());
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
        // Map the field's (platform-neutral) visibility to JVM access flags: a `private` field →
        // `ACC_PRIVATE` (the default — Kotlin backing fields are private, reached via accessors); a
        // non-private field → `ACC_PUBLIC` (read/written cross-class, e.g. a coroutine continuation's
        // `result`/`label`).
        //
        // A `@JvmField` property has no accessor, so the field IS the declaration's visible face and
        // takes the PROPERTY's declared visibility instead — `protected val` stays `ACC_PROTECTED`,
        // `internal`/`public` become `ACC_PUBLIC` (Kotlin's `internal` is a module-only fact).
        let jvm_field_visibility = jvm_field_visibility(c, name);
        let private = field.is_private();
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
            jvm_field_visibility.unwrap_or(if private { 0x0002 } else { 0x0001 })
                | if field.is_final() { 0x0010 } else { 0 }
                | if static_storage(ir, c) { 0x0008 } else { 0 }
        };
        // A field typed by a bare type parameter (`val a: A`) carries a `Signature` (`TA;`); a
        // PARAMETERIZED concrete type (`val xs: List<String>`) carries its full generic signature. Both
        // like kotlinc; disjoint (a field is one or the other).
        let type_parameter = ir
            .field_signatures(&fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| fname == name))
            .map(|(_, parameter)| parameter.as_str())
            .or(field.type_param.as_deref());
        let field_sig = property_jvm_signatures(&signature_formatter, ty, type_parameter).field;
        let physical_name = instance_field_jvm_name(ir, c, field);
        let field_desc = ir_type_desc(ty);
        // Coroutine continuation fields are compiler-generated storage rather than Kotlin
        // properties, so they are visited eagerly and receive no nullability annotation. Declared
        // properties, a data class's included, use the later field-table visit: their methods
        // establish the preceding pool window and their backing fields carry Kotlin's nullability
        // annotation.
        if is_continuation && matches!(name.as_str(), "result" | "this$0" | "label") {
            // kotlinc interns a continuation's SPILL field names with the field visit, but `result`,
            // `this$0` and `label` first appear where they are USED — `this$0` in the constructor's
            // `putfield`, the other two in `invokeSuspend`. Declaring them eagerly interned all three
            // ahead of the constructor and shifted every later pool entry.
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
            // The constructor prefix's fields (the outer instance, lexical captures) are the
            // compiler's own, and kotlinc annotates no synthetic declaration.
            let field_ann = match field_nullability_kind(ir, &fq_name, name, *ty) {
                _ if field_index < c.constructor_prefix_count as usize => None,
                1 => Some("Lorg/jetbrains/annotations/NotNull;"),
                2 => Some("Lorg/jetbrains/annotations/Nullable;"),
                _ => None,
            };
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
        cw.set_debug_metadata(
            opts.source_file.as_deref().unwrap_or(""),
            &metadata.l,
            &metadata.nl,
            &metadata.i,
            &metadata.s,
            &metadata.n,
            &metadata.m,
            &metadata.c,
            metadata.v,
        );
    }
    // A `companion object`'s `const val`s live on THIS (outer) class as `public static final` +
    // `ConstantValue` fields (kotlinc's layout); they have no `<clinit>` store (the JVM initializes them).
    // DECLARATION order: common lowering groups declarations by shape, while kotlinc's field table
    // follows the companion source order.
    let mut owner_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_matches(&fq_name))
        .map(|(index, property)| (index as u32, property))
        .collect();
    owner_statics.sort_by_key(|(_, property)| property.line);
    for (static_index, s) in owner_statics {
        let desc = ir_type_desc(&s.ty);
        // A `private const val`/`private val` on an object/companion keeps its declared visibility
        // (kotlinc: PRIVATE static final; const reads are inlined so no cross-class getstatic needs it).
        // A `var` is reassignable, so it must NOT carry ACC_FINAL — a `putstatic` on a final field
        // outside `<clinit>` is an IllegalAccessError.
        let final_flag = if s.is_var { 0x0000 } else { 0x0010 };
        // A HOISTED companion property's field is PRIVATE regardless of the property's declared
        // visibility (kotlinc: every access goes through the accessors/bridges, never the field) —
        // EXCEPT under `@JvmField`, where the PUBLIC field IS the property's whole JVM surface
        // (kotlinc emits it public even for an `internal` declaration, with no accessors at all).
        let hoisted = ir.is_jvm_companion_hoisted_static(static_index);
        let jvm_field = ir.is_jvm_field_static(static_index);
        let acc = if (s.visibility.is_private() || hoisted) && !jvm_field {
            0x000A | final_flag // PRIVATE | STATIC [| FINAL]
        } else {
            0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
        };
        // `ConstantValue` is only meaningful on a FINAL field (JVMS 4.7.2 ignores it otherwise), and a
        // `var` is initialized by the `<clinit>` store anyway. A HOISTED companion property never
        // folds either — kotlinc initializes it in `<clinit>` (only `const val` gets the attribute).
        // LATE adds: kotlinc's field-table visit runs after the methods, so a hoisted static's name
        // first interns at its `access$…$cp` bridge and a folded const's name + `ConstantValue`
        // land after the `<clinit>` window.
        let fold = s.is_const && !s.is_var && !hoisted;
        let cv = fold
            .then(|| match ir.expr(s.init) {
                crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null) => {
                    Some(c.clone())
                }
                _ => None,
            })
            .flatten();
        // Generated storage with no declaration of its own is ACC_SYNTHETIC and unannotated.
        let synthetic = ir.is_compiler_generated_static(static_index);
        let acc = if synthetic { acc | 0x1000 } else { acc };
        // Reference-typed statics, including private hoisted fields, carry nullability annotations.
        let ann = (!synthetic && (desc.starts_with('L') || desc.starts_with('['))).then(|| {
            if s.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        let signature = property_jvm_signatures(&signature_formatter, &s.ty, None).field;
        cw.add_field_late_sig(acc, &s.name, &desc, signature.as_deref(), cv, ann);
        // A field carries its FIELD-targeted annotations as `RuntimeInvisibleAnnotations`, BEFORE
        // the nullability entry — kotlinc's attribute order.
        //
        // They come from wherever the field's DECLARATION lives: a hoisted companion property
        // records them on the COMPANION, while a static this class owns outright — one a compiler
        // plugin generated, say — records them on the class itself. The companion path keeps its
        // `@JvmField` condition, which is what that annotation has always meant there: it is the
        // reason the field is the property's whole JVM surface.
        if let Some(annotations) = c
            .companion_class
            .and_then(|companion| ir.class_id_by_name(companion))
            .filter(|_| jvm_field)
            .and_then(|companion| {
                ir.classes[companion as usize]
                    .field_annotations
                    .iter()
                    .find(|annotations| annotations.field == s.name)
            })
            .or_else(|| {
                c.field_annotations
                    .iter()
                    .find(|annotations| annotations.field == s.name)
            })
        {
            cw.set_last_late_field_annotations(&annotations.annotations);
        }
    }
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    // The constructor takes ALL primary-ctor params (`ctor_args`), in declaration order — `val`/`var`
    // params back a field, plain params are arguments only. (Synthesized classes have empty `ctor_args`
    // and fall back to the leading `ctor_param_count` fields.)
    // `(start_pc, line)` for the ctor's LineNumberTable — one per body-property initializer, plus the
    // trailing `return`. Empty when the class has no body properties (kotlinc emits a single entry).
    let mut ctor_lines: Vec<(u16, u32)> = Vec::new();
    let mut primary_ctor_debug = None;
    let param_tys = class_ctor_jvm_tys(c);
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
    let markers = property_annotation_marker_fids(ir, c);
    let member_emission = ScheduledMemberEmission {
        ir,
        class: c,
        fq_name: &fq_name,
        facade,
        signature_formatter: &signature_formatter,
        param_assertions: opts.param_assertions,
        env,
        markers: &markers,
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
    // A class with NO primary constructor emits no primary `<init>` — every `<init>` comes from a
    // secondary constructor (below). Otherwise emit the primary `<init>` here.
    if c.has_primary_ctor {
        let ctor_desc = primary_ctor_descriptor(c);
        let ctor_parameters = if env.java_parameters {
            if is_continuation {
                super::method_parameters::continuation_constructor(
                    c.fields.iter().any(|field| field.name == "this$0"),
                )
            } else {
                super::method_parameters::primary_constructor(c, &param_tys)
            }
        } else {
            Vec::new()
        };
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
            let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj(&fq_name));
            e.slots.insert(0, (receiver, Ty::obj(&fq_name)));
            for (vi, t) in param_tys.iter().enumerate() {
                let value = vi as u32 + 1;
                let s = e.frame.enter(FrameKey::Value(value), *t);
                e.slots.insert(value, (s, *t));
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
            // Debug metadata consumes the position emission actually reached. Reconstructing this
            // later from constructor parameters, constant-pool widths, or assertion policy makes a
            // semantic description masquerade as bytecode layout and can point inside an opcode.
            primary_ctor_debug = Some((
                ctor_desc.clone(),
                u16::try_from(ctor.bytes.len()).expect("a JVM method body fits in u16"),
            ));
            let ctor_param_fields = primary_ctor_parameter_fields(c, param_tys.len());
            // Store only constructor fields explicitly marked as pre-super. A language-level inner
            // class marks its enclosing-instance field because a superclass argument may read it; an
            // ordinary capture does not. Keeping this as ordering metadata avoids interpreting a JVM
            // field name as source semantics. A `putfield` of the current class's own field on the
            // still-uninitialized `this` is legal per JVMS 4.10.2.4.
            for &(param_i, field_i) in &c.pre_super_param_fields {
                let param_i = param_i as usize;
                let field = &c.fields[field_i as usize];
                let param_ty = param_tys[param_i];
                let param_slot = 1 + param_tys[..param_i]
                    .iter()
                    .map(|ty| slot_words(*ty))
                    .sum::<u16>();
                ctor.aload(0);
                load(param_ty, param_slot, &mut ctor);
                let physical_name = instance_field_jvm_name(ir, c, field);
                let fref =
                    e.cw.fieldref(&fq_name, &physical_name, &type_descriptor(field.ty));
                ctor.putfield(fref, slot_words(field.ty) as i32);
            }
            for &statement in &c.super_arg_prelude {
                e.emit(statement, &mut ctor);
            }
            // `super(args)` — `this` is loaded first, so spill any branchy arg to temps before it.
            let super_args = c.super_args.clone();
            if super_args.iter().any(|&a| e.emits_control_flow(a)) {
                let temps = e.spill_to_temps(&super_args, &mut ctor);
                ctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut ctor);
                }
                e.release_operand_spills(&temps);
            } else {
                ctor.aload(0);
                for &a in &super_args {
                    e.emit_value(a, &mut ctor);
                }
            }
            // A base whose primary ctor takes a value-class param — or a SEALED base — has a PRIVATE
            // primary. The checker records whether this exact selection is that primary; only then
            // must a subclass `super(…)` reach it through the PUBLIC|SYNTHETIC
            // `(…args, DefaultConstructorMarker)` accessor rather than the inaccessible declaration.
            let (mut super_param_tys, super_accessor) = super_ctor_jvm_tys(e.ir, c, &superclass);
            let super_defaults =
                e.ir.super_constructor_default_arguments
                    .get(&c.fq_name_id())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
            if !super_defaults.is_empty() {
                super_param_tys = jvm_tys(&c.super_ctor_params);
                for mask in constructor_default_masks(super_defaults, c.super_ctor_params.len()) {
                    ctor.push_int(mask, e.cw);
                    super_param_tys.push(Ty::Int);
                }
                ctor.aconst_null();
                super_param_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            } else if super_accessor {
                ctor.aconst_null();
            }
            let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let super_descriptor =
                e.ir.external_super_constructors
                    .get(&c.fq_name_id())
                    .and_then(|target| target.descriptor.as_deref())
                    .map(str::to_owned)
                    .unwrap_or_else(|| method_descriptor(&super_param_tys, Ty::Unit));
            let super_init = e.cw.methodref(&superclass, "<init>", &super_descriptor);
            ctor.invokespecial(super_init, aw, 0);
            e.this_uninitialized = false;
            // Store this class's own primary-constructor parameter fields: each `val`/`var` param's arg is
            // stored to its field (the property fields are `fields[0..]` in declaration order among params);
            // a plain param is skipped (it stays a local for the initializer body). `is_field` flags come
            // from `ctor_args`; a synthesized class (empty `ctor_args`) stores all leading param fields.
            // SKIPPED when `explicit_param_stores` is set — a desugared class already stores them via
            // explicit `SetField`s at the head of `init_body`; auto-storing too would double-store.
            if !c.explicit_param_stores {
                let mut slot = 1u16;
                for (i, t) in param_tys.iter().enumerate() {
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
                            load(*t, slot, &mut ctor);
                            let physical_name = instance_field_jvm_name(ir, c, &c.fields[field_i]);
                            let fref =
                                e.cw.fieldref(&fq_name, &physical_name, &type_descriptor(*t));
                            ctor.putfield(fref, slot_words(*t) as i32);
                        }
                    }
                    slot += slot_words(*t);
                }
            }
            if let Some(init_body) = c.init_body.filter(|_| !static_storage(ir, c)) {
                e.emit_constructor_init_body(c, init_body, &mut ctor, &mut ctor_lines);
                init_diverges = e.discarding_diverges(init_body);
            }
            max_slot = e.frame.max();
        }
        // A diverging `init` (e.g. `init { throw … }`) leaves no fall-through — the trailing `return`
        // would be dead code after `athrow` (which the verifier rejects without a frame).
        if !init_diverges {
            // The trailing `return` goes back on the class-declaration line, closing the ctor's table.
            if !ctor_lines.is_empty() {
                ctor_lines.push((ctor.bytes.len() as u16, c.decl_line));
            }
            ctor.ret_void();
        }
        ctor.ensure_locals(max_slot);
        ctor.link();
        // An `object`'s constructor is private; a `@JvmInline value class`'s is private too (instances are
        // created via `constructor-impl`/`box-impl`, never `new`); a class whose primary ctor takes a
        // value-class-typed parameter is private too (kotlinc routes construction through a synthetic
        // `(…args, DefaultConstructorMarker)` accessor — emitted below); a `C$Companion`'s is
        // package-private (so the outer class's `<clinit>` can call it without nestmate attributes); a
        // normal class's is public.
        let value_param_ctor = ir.has_value_param_ctor(&fq_name);
        // A SEALED class's primary ctor is private too — subclasses (and Java/reflection) construct
        // through the PUBLIC|SYNTHETIC `(…args, DefaultConstructorMarker)` accessor (kotlinc's shape).
        let ctor_access = if is_continuation || c.is_anonymous_object {
            // A continuation class's ctor is package-private (constructed only by its own file);
            // kotlinc gives an ANONYMOUS class's ctor the same access (flags 0x0000). This remains
            // true when a capture has value-class type: the enclosing class directly constructs the
            // anonymous class, so treating that semantic capture like a declared value-class
            // parameter would make the only reachable constructor private.
            0x0000
        } else if c.is_value {
            // A value class is never constructed through `new` outside its own `box-impl`; kotlinc
            // marks the private primary synthetic as well.
            0x1002
        } else if c.is_singleton() || value_param_ctor || c.is_sealed {
            0x0002
        } else {
            // A DECLARED protected constructor reaches the JVM method too (kotlinc emits `<init>`
            // protected), and a declared PRIVATE one is ACC_PRIVATE: another class calls it
            // through its `constructor_accessors` accessor.
            match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => 0x0004,
                Some(crate::types::Visibility::Private) => 0x0002,
                _ => 0x0001,
            }
        };
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
        if let Some(metadata) = continuation_metadata {
            let has_this0 = c.fields.iter().any(|field| field.name == "this$0");
            let mut ctor_locals: Vec<(String, String, u16)> =
                vec![("this".to_string(), format!("L{fq_name};"), 0)];
            let mut slot = 1u16;
            if has_this0 {
                ctor_locals.push((
                    "this$0".to_string(),
                    format!("L{};", metadata.enclosing_class),
                    slot,
                ));
                slot += 1;
            }
            ctor_locals.push((
                "$completion".to_string(),
                "Lkotlin/coroutines/Continuation;".to_string(),
                slot,
            ));
            let ctor_desc = if has_this0 {
                format!(
                    "(L{};Lkotlin/coroutines/Continuation;)V",
                    metadata.enclosing_class
                )
            } else {
                "(Lkotlin/coroutines/Continuation;)V".to_string()
            };
            cw.set_method_debug("<init>", &ctor_desc, None, &ctor_locals);
        }
        // Declared PRIMARY-constructor annotations (`class C @Mark constructor(…)`), with the same
        // `Deprecated` / `ACC_SYNTHETIC` companions a secondary constructor's carry.
        let primary_annotations = &c.primary_ctor_annotations;
        if !primary_annotations.is_empty() {
            let ctor_desc = method_descriptor(&param_tys, Ty::Unit);
            cw.set_method_annotations("<init>", &ctor_desc, primary_annotations);
            if primary_annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &ctor_desc);
            }
            if primary_annotations.deprecated_hidden() {
                cw.set_method_synthetic("<init>", &ctor_desc);
            }
        }
        // A default on any primary-ctor parameter → kotlinc's synthetic
        // `<init>(params…, int mask, DefaultConstructorMarker)` overload (fills the masked slots from the
        // defaults, then `invokespecial` the real `<init>`).
        if let Some(defaults) = ir.class_ctor_defaults(&fq_name) {
            // (The stub emits its own LineNumberTable — class line, per-fill parameter lines, the
            // ctor's closing-`)` line at `return` — collapsed for single-line declarations.)
            let prefix_count = c.constructor_prefix_count as usize;
            let (prefix_params, source_params) = param_tys
                .split_at_checked(prefix_count)
                .expect("constructor default prefix exceeds its parameters");
            let source_defaults = defaults
                .get(prefix_count..)
                .expect("constructor default prefix exceeds its defaults");
            constructor_defaults::emit_ctor_default_stub_with_prefix(
                ir,
                &fq_name,
                facade,
                prefix_params,
                prefix_count,
                source_params,
                source_defaults,
                None,
                value_param_ctor || c.is_sealed,
                c.primary_ctor_annotations.deprecated(),
                // A declared private primary's overload is package-private, the way kotlinc gives
                // any private constructor's; a sealed class's is its public way in.
                if ir.ctor_visibilities.get(&c.fq_name_id())
                    == Some(&crate::types::Visibility::Private)
                    && !c.is_sealed
                    && !value_param_ctor
                {
                    0x1000
                } else {
                    0x1001
                },
                &mut cw,
                env,
            );
        }
        if byte_parity {
            seed_data_class_pool(pool_seed(), &mut cw);
        }
    } // end `if c.has_primary_ctor`

    for member in after_primary {
        emit_scheduled_member(&member_emission, member, &mut cw);
    }
    // The exact serialization-plugin deserialization constructor emits after every declared member
    // (`write$Self$main` included) and before `<clinit>`, which is kotlinc's member order for it.
    if let Some(secondary_ordinal) = serialization_constructor {
        let sc = &c.secondary_ctors[secondary_ordinal as usize];
        SecondaryConstructorEmitter {
            ir,
            class: c,
            owner: &fq_name,
            facade,
            env,
            writer: &mut cw,
            owner_prefix: &OwnerConstructorPrefix::none(),
        }
        .emit(secondary_ordinal as usize, sc);
    }
    // The child-serializer cache's factories and its `…$cp` accessor follow that constructor, which
    // is kotlinc's order for them. They are generated, so they carry no source order to sort by and
    // the declared-member schedule leaves them out.
    for &fid in c
        .methods
        .iter()
        .filter(|fid| ir.serialization_cache_methods.contains(fid))
    {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
        }
    }
    // EVERY parameter defaulted → kotlinc also emits the no-arg convenience `<init>()`
    // (`AuditFilters()` in Java/reflection), delegating to the `$default` overload with a full
    // mask — AFTER the declared methods (kotlinc's member order), at the primary's declared
    // visibility (a PROTECTED primary gets a protected convenience ctor).
    if c.has_primary_ctor {
        let param_tys = class_ctor_jvm_tys(c);
        let value_param_ctor = ir.has_value_param_ctor(&fq_name);
        let ctor_access = if is_continuation || c.is_anonymous_object {
            0x0000
        } else if c.is_singleton() || c.is_value || value_param_ctor || c.is_sealed {
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
                if !annotations.is_empty() {
                    cw.set_method_annotations("<init>", "()V", annotations);
                    if annotations.deprecated() {
                        cw.mark_method_deprecated("<init>", "()V");
                    }
                    if annotations.deprecated_hidden() {
                        cw.set_method_synthetic("<init>", "()V");
                    }
                }
            }
        }
    }
    // A companion's synthetic `(…, DefaultConstructorMarker)` ctor goes AFTER the accessors —
    // kotlinc's companion member order (private <init>, accessors, marker <init>). A class hiding a
    // value-class-parameter primary puts its accessor after `equals` the same way.
    if c.has_primary_ctor && marker_accessor_emitted_last(ir, c) && has_ctor_marker_accessor(ir, c)
    {
        let physical_parameters = class_ctor_jvm_tys(c);
        let parameter_identities =
            crate::jvm::method_parameters::primary_constructor_identities(c, &physical_parameters);
        constructor_defaults::emit_ctor_marker_accessor(
            &fq_name,
            &physical_parameters,
            &parameter_identities,
            &mut cw,
        );
    }
    bridge_emission::emit_bridges(ir, c, &mut cw, env.bridge_return_adaptations, env.run);
    constructor_accessors::emit_accessors(ir, c, &fq_name, &mut cw);
    // HOISTED companion properties: the private static field lives on THIS class, so the companion's
    // delegating accessors reach it through PUBLIC synthetic `access$get<X>$cp`/`access$set<X>$cp`
    // bridges — emitted AFTER the instance methods, right before `<clinit>` (kotlinc's order).
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| {
            ir.is_jvm_companion_hoisted_static(*index as u32)
                && !ir.is_jvm_field_static(*index as u32)
                && s.owner_matches(&fq_name)
        })
        .map(|(_, s)| s)
    {
        let jt = jvm_declared_ty(&s.ty);
        let desc = type_descriptor(jt);
        // kotlinc visits the bridge's name before its body's field cluster.
        let getter_bridge = format!("access${}$cp", property_getter_name(&s.name));
        cw.reserve_method_name(&getter_bridge);
        cw.seed_utf8(&format!("(){desc}"));
        let mut g = CodeBuilder::new(0);
        let fref = cw.fieldref(&fq_name, &s.name, &desc);
        g.getstatic(fref, slot_words(jt) as i32);
        emit_return(jt, &mut g);
        g.ensure_locals(0);
        g.link();
        cw.add_method(
            0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
            &getter_bridge,
            &format!("(){desc}"),
            &g,
        );
        if s.is_var {
            let setter_bridge = format!("access${}$cp", property_setter_name(&s.name));
            cw.reserve_method_name(&setter_bridge);
            cw.seed_utf8(&format!("({desc})V"));
            // The setter bridge's `<set-?>` LocalVariableTable strings intern at its method visit.
            cw.seed_utf8("<set-?>");
            cw.seed_utf8(&desc);
            let words = slot_words(jt);
            let mut st = CodeBuilder::new(words);
            load(jt, 0, &mut st);
            let fref = cw.fieldref(&fq_name, &s.name, &desc);
            st.putstatic(fref, slot_words(jt) as i32);
            st.ret_void();
            st.ensure_locals(words);
            st.link();
            cw.add_method(0x1019, &setter_bridge, &format!("({desc})V"), &st);
        }
    }
    // A class with a `companion object` gets its `<clinit>` LAST among the methods (kotlinc's
    // order): the `Companion` instance store, then each non-const owner static's initializer (a
    // hoisted companion property, or a companion `const val` whose initializer isn't a compile-time
    // literal — the `ConstantValue` path covers only folded consts). One shared `<clinit>`.
    {
        let clinit_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
            .statics
            .iter()
            .enumerate()
            .filter(|s| {
                s.1.owner_matches(&fq_name)
                    && !(s.1.is_const && static_fields::const_value_idx_peek(ir, s.1.init))
            })
            .map(|(index, s)| (index as u32, s))
            .collect();
        if !static_storage(ir, c) && (c.companion_class.is_some() || !clinit_statics.is_empty()) {
            // kotlinc visits `<clinit>` (name + descriptor) before its body's companion
            // construction and hoisted-initializer constants.
            cw.reserve_method_name("<clinit>");
            cw.seed_utf8("()V");
            let mut e = Emitter::new(
                ir,
                &mut cw,
                env,
                &fq_name,
                facade,
                Ty::Unit,
                clinit_statics.iter().map(|(_, property)| property.init),
            );
            let mut clinit = CodeBuilder::new(0);
            emit_companion_init(e.cw, &mut clinit, &fq_name, c);
            // kotlinc's `<clinit>` LineNumberTable: one entry per hoisted-property store, at the
            // store's pc, mapping to the property's declaration line in the COMPANION source. The
            // `Companion` construction itself has no entry.
            let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
            for (static_index, s) in &clinit_statics {
                let pc = clinit.bytes.len() as u16;
                // A hoisted companion property's line lives on the companion. A static a compiler
                // plugin generated has no property to look up and carries its own.
                let line = if ir.is_jvm_companion_hoisted_static(*static_index) {
                    c.companion_class
                        .as_ref()
                        .and_then(|companion| ir.prop_decl_lines.get(&(*companion, s.name.clone())))
                        .copied()
                        .unwrap_or(0)
                } else {
                    s.line
                };
                if line != 0 {
                    clinit_lines.push((pc, line));
                }
                e.emit_static_initializer_store(&fq_name, s, &mut clinit);
            }
            clinit.ret_void();
            clinit.ensure_locals(e.frame.max());
            clinit.link();
            e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
            if byte_parity && !clinit_lines.is_empty() {
                e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
            }
        }
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
    cw.set_class_annotations(&c.applied_annotations);
    // A cross-module provider's `@Metadata` wins; otherwise compute one from the IR (bounded shapes).
    // An ANONYMOUS class gets kotlinc's minimal k=1 record (LOCAL flags, raw-internal fq_name via
    // the string table's localName marker, supertypes — no members).
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| {
            if c.is_anonymous_object {
                let mut supers: Vec<Ty> = Vec::new();
                if c.has_non_top_superclass() {
                    supers.push(Ty::obj_name(c.superclass));
                }
                supers.extend(c.interfaces.iter_ids().map(Ty::obj_name));
                let (d1_bytes, d2) =
                    crate::metadata::class_builder::build_anonymous_class(&fq_name, &supers);
                let d1: String = d1_bytes.iter().map(|&b| b as char).collect();
                Some(KotlinMetadata {
                    k: 1,
                    mv: vec![2, 4, 0],
                    xi: 48,
                    d1: vec![d1],
                    d2,
                })
            } else {
                build_class_metadata(ir, c, opts)
            }
        })
        .flatten();
    // `-jvm-default=disable`: the interface holds no bodies, so a class that inherits one gets an
    // explicit override forwarding to the holder. Without these the class does not implement its own
    // interface and every inherited call is an `AbstractMethodError`.
    emit_default_impls_forwarders(ir, c, &mut cw, env);
    // Debug tables + nullability annotations (opt-in with metadata) for any class that qualified for a
    // computed `@Metadata` — including data classes (their synthesized methods get a LocalVariableTable
    // + @NotNull/@Nullable). NOTE: the constant-pool seeding (above) is still plain-class only, so a
    // data class is not yet FULLY byte-identical (its pool order differs) — but the attributes match.
    if computed.is_some() && !is_coroutine_state_machine(c) {
        attach_synth_debug_tables(
            ir,
            c,
            &mut cw,
            opts.param_assertions,
            primary_ctor_debug
                .as_ref()
                .map(|(descriptor, pc)| (descriptor.as_str(), *pc)),
            &ctor_lines,
        );
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(ir, c, &mut cw);
    }
    if continuation_metadata.is_some() {
        let self_desc = format!("L{fq_name};");
        cw.set_method_debug(
            "invokeSuspend",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            None,
            &[
                ("this".to_string(), self_desc, 0),
                ("$result".to_string(), "Ljava/lang/Object;".to_string(), 1),
            ],
        );
        // A continuation's `invokeSuspend` is compiler-manufactured, but kotlinc still names its
        // parameter under `-java-parameters`. (Its `<init>` is named where that constructor's own
        // debug tables are attached, with the descriptor in scope there.)
        if env.java_parameters {
            cw.set_method_parameters(
                "invokeSuspend",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                &super::method_parameters::continuation_invoke_suspend(),
            );
        }
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    // Seed every retained `InnerClasses` row's outer-class ref and simple name at kotlinc's
    // post-metadata pool position, in the table's sorted order. An ANONYMOUS class has neither for
    // its own row; its enclosing refs use the `EnclosingMethod` window in `finish`.
    if !is_coroutine_state_machine(c) && !c.is_anonymous_object {
        cw.seed_inner_class_names();
    }
    cw.finish()
}

/// Emit a synthesized property-reference singleton (`Type$prop$N extends PropertyReference1Impl`):
/// a package-private `final` class with a `public static final INSTANCE`, a constructor
/// `super(owner.class, name, "getName()desc", 0)`, a `get(Object)Object` override that reads
/// `((Owner) it).getName()` (boxing a primitive), and a `<clinit>` that builds the singleton. `.name`
/// is inherited from `PropertyReference1Impl` (returns the constructor's name argument).
fn emit_object_as(cw: &mut ClassWriter, code: &mut CodeBuilder, ty: Ty) {
    let ty = ir_ty_to_jvm(&ty);
    if ty.is_jvm_scalar() {
        unbox_prim(cw, code, ty);
    } else if let Some(internal) = checkcast_internal(ty) {
        let class = cw.class_ref(&internal);
        code.checkcast(class);
    }
}

pub(crate) fn parse_physical_method_desc(desc: &str) -> Option<(Vec<Ty>, Ty)> {
    let (params, ret) = crate::jvm::names::parse_method_descriptor(desc)?;
    Some((
        params.into_iter().map(ty_from_field_descriptor).collect(),
        ty_from_field_descriptor(ret),
    ))
}

fn property_getter_descriptor(pr: &crate::ir::PropRef, ext: bool) -> String {
    match (&pr.getter_descriptor, ext) {
        (Some(descriptor), _) => descriptor.clone(),
        (None, false) => format!("(){}", type_descriptor(ir_ty_to_jvm(&pr.prop_ty))),
        (None, true) => unreachable!(
            "a checked extension-property reference reached JVM emission without its getter descriptor"
        ),
    }
}

fn property_setter_target(pr: &crate::ir::PropRef, ext: bool) -> (String, String) {
    let name = pr
        .setter_name
        .clone()
        .unwrap_or_else(|| property_setter_name(&pr.prop_name));
    let descriptor = match (&pr.setter_descriptor, ext) {
        (Some(descriptor), _) => descriptor.clone(),
        (None, false) => format!("({})V", type_descriptor(ir_ty_to_jvm(&pr.prop_ty))),
        (None, true) => unreachable!(
            "a checked mutable extension-property reference reached JVM emission without its setter descriptor"
        ),
    };
    (name, descriptor)
}

struct PropertyCallTarget<'a> {
    owner: &'a str,
    facade: Option<&'a str>,
    array_length: bool,
    name: &'a str,
    descriptor: &'a str,
    params: &'a [Ty],
    owner_is_interface: bool,
    boxed_value_class: Option<TypeName>,
    /// The receiver is a value class's boxed object while the accessor takes its carrier.
    unboxed_receiver_value_class: Option<TypeName>,
    field_access: Option<&'a crate::jvm::inline::PropertyAccess>,
}

struct PropertyReferenceTarget {
    owner: String,
    call_owner: String,
    facade: Option<String>,
    array_length: bool,
    getter_descriptor: String,
    getter_params: Vec<Ty>,
    getter_ret: Ty,
    signature: String,
    getter_field: Option<crate::jvm::inline::PropertyAccess>,
    setter_field: Option<crate::jvm::inline::PropertyAccess>,
    boxed_value_class: Option<TypeName>,
    unboxed_receiver_value_class: Option<TypeName>,
}

impl PropertyReferenceTarget {
    fn new(
        property: &crate::ir::PropRef,
        realization: &crate::jvm::property_references::PropertyReferenceRealization,
        facade: &str,
        bodies: &dyn MethodBodies,
    ) -> Self {
        let semantic_owner = property.owner().expect("property reference owner");
        let array_owner = crate::jvm::names::array_class_descriptor(&semantic_owner);
        let owner = array_owner.clone().unwrap_or_else(|| {
            crate::jvm::jvm_class_map::to_jvm_internal(&semantic_owner).to_string()
        });
        let semantic_call_owner = property
            .call_owner()
            .expect("property reference call owner");
        let call_owner = crate::jvm::names::array_class_descriptor(&semantic_call_owner)
            .unwrap_or_else(|| {
                crate::jvm::jvm_class_map::to_jvm_internal(&semantic_call_owner).to_string()
            });
        let facade = property.ext_facade_or_facade(facade);
        let getter_field = (facade.is_none() && array_owner.is_none())
            .then(|| bodies.property_read_access(&semantic_owner, &property.prop_name))
            .flatten()
            .filter(|access| matches!(access, crate::jvm::inline::PropertyAccess::Field { .. }));
        let setter_field = (property.mutable && facade.is_none() && array_owner.is_none())
            .then(|| bodies.property_write_access(&semantic_owner, &property.prop_name))
            .flatten()
            .filter(|access| matches!(access, crate::jvm::inline::PropertyAccess::Field { .. }));
        let getter_descriptor = property_getter_descriptor(property, facade.is_some());
        let (getter_params, getter_ret) = parse_physical_method_desc(&getter_descriptor)
            .expect("validated property getter descriptor");
        Self {
            owner,
            call_owner,
            array_length: array_owner.is_some() && facade.is_none(),
            signature: format!("{}{}", property.getter_name, getter_descriptor),
            facade,
            getter_descriptor,
            getter_params,
            getter_ret: ir_ty_to_jvm(&getter_ret),
            getter_field,
            setter_field,
            boxed_value_class: realization.boxed_value_class,
            unboxed_receiver_value_class: realization.unboxed_receiver_value_class,
        }
    }

    fn getter<'a>(&'a self, property: &'a crate::ir::PropRef) -> PropertyCallTarget<'a> {
        PropertyCallTarget {
            owner: &self.call_owner,
            facade: self.facade.as_deref(),
            array_length: self.array_length,
            name: &property.getter_name,
            descriptor: &self.getter_descriptor,
            params: &self.getter_params,
            owner_is_interface: property.owner_is_interface,
            boxed_value_class: self.boxed_value_class,
            unboxed_receiver_value_class: self.unboxed_receiver_value_class,
            field_access: self.getter_field.as_ref(),
        }
    }

    fn setter<'a>(
        &'a self,
        property: &'a crate::ir::PropRef,
        name: &'a str,
        descriptor: &'a str,
        params: &'a [Ty],
    ) -> PropertyCallTarget<'a> {
        PropertyCallTarget {
            owner: &self.call_owner,
            facade: self.facade.as_deref(),
            array_length: false,
            name,
            descriptor,
            params,
            owner_is_interface: property.owner_is_interface,
            boxed_value_class: self.boxed_value_class,
            unboxed_receiver_value_class: self.unboxed_receiver_value_class,
            field_access: self.setter_field.as_ref(),
        }
    }
}

fn emit_property_reference_constructor(
    cw: &mut ClassWriter,
    superclass: &str,
    property: &crate::ir::PropRef,
    target: &PropertyReferenceTarget,
    bound: bool,
) {
    let mut code = CodeBuilder::new(if bound { 2 } else { 1 });
    code.aload(0);
    if bound {
        code.aload(1);
    }
    code.ldc_class(target.facade.as_deref().unwrap_or(&target.owner), cw);
    code.push_string(&property.prop_name, cw);
    code.push_string(&target.signature, cw);
    code.push_int(target.facade.is_some() as i32, cw);
    let descriptor = if bound {
        "(Ljava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
    } else {
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
    };
    let constructor = cw.methodref(superclass, "<init>", descriptor);
    code.invokespecial(constructor, if bound { 5 } else { 4 }, 0);
    code.ret_void();
    let own_descriptor = if bound {
        "(Ljava/lang/Object;)V"
    } else {
        "()V"
    };
    finish_code::<0x0000>(
        cw,
        "<init>",
        own_descriptor,
        &mut code,
        if bound { 2 } else { 1 },
    );
}

impl PropertyCallTarget<'_> {
    fn emit_get(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, ret: Ty) {
        if let Some(crate::jvm::inline::PropertyAccess::Field {
            owner,
            name,
            descriptor,
            is_static,
        }) = self.field_access
        {
            let physical = ty_from_field_descriptor(descriptor);
            if *is_static {
                code.pop();
                let field = cw.fieldref(owner, name, descriptor);
                code.getstatic(field, slot_words(physical) as i32);
            } else {
                emit_object_as(cw, code, Ty::obj(owner));
                let field = cw.fieldref(owner, name, descriptor);
                code.getfield(field, slot_words(physical) as i32);
            }
            return;
        }
        if self.array_length {
            let class = cw.class_ref(self.owner);
            code.checkcast(class);
            code.arraylength();
        } else if let Some(facade) = self.facade {
            adapt_property_reference_value(
                cw,
                code,
                self.unboxed_receiver_value_class,
                self.params[0],
            );
            let method = cw.methodref(facade, self.name, self.descriptor);
            code.invokestatic(
                method,
                slot_words(ir_ty_to_jvm(&self.params[0])) as i32,
                slot_words(ret) as i32,
            );
        } else if let Some(value_class) = self.unboxed_receiver_value_class {
            // A MEMBER of a value class: its accessor is realized as a static method over the
            // carrier on the value class itself, so the receiver is unboxed and the call is static.
            adapt_property_reference_value(cw, code, Some(value_class), self.params[0]);
            let method = cw.methodref(self.owner, self.name, self.descriptor);
            code.invokestatic(
                method,
                slot_words(ir_ty_to_jvm(&self.params[0])) as i32,
                slot_words(ret) as i32,
            );
        } else {
            emit_object_as(cw, code, Ty::obj(self.owner));
            if self.owner_is_interface {
                let method = cw.interface_methodref(self.owner, self.name, self.descriptor);
                code.invokeinterface(method, 0, slot_words(ret) as i32);
            } else {
                let method = cw.methodref(self.owner, self.name, self.descriptor);
                code.invokevirtual(method, 0, slot_words(ret) as i32);
            }
        }
    }

    fn emit_set(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, value_local: u16) {
        if let Some(crate::jvm::inline::PropertyAccess::Field {
            owner,
            name,
            descriptor,
            is_static,
        }) = self.field_access
        {
            let physical = ty_from_field_descriptor(descriptor);
            if *is_static {
                code.pop();
            } else {
                emit_object_as(cw, code, Ty::obj(owner));
            }
            code.aload(value_local);
            self.emit_property_value(cw, code, physical);
            let field = cw.fieldref(owner, name, descriptor);
            if *is_static {
                code.putstatic(field, slot_words(physical) as i32);
            } else {
                code.putfield(field, slot_words(physical) as i32);
            }
            return;
        }
        if let Some(facade) = self.facade {
            adapt_property_reference_value(
                cw,
                code,
                self.unboxed_receiver_value_class,
                self.params[0],
            );
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[1]);
            let arg_words = self
                .params
                .iter()
                .map(|param| slot_words(ir_ty_to_jvm(param)) as i32)
                .sum();
            let method = cw.methodref(facade, self.name, self.descriptor);
            code.invokestatic(method, arg_words, 0);
        } else if let Some(value_class) = self.unboxed_receiver_value_class {
            adapt_property_reference_value(cw, code, Some(value_class), self.params[0]);
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[1]);
            let arg_words = self
                .params
                .iter()
                .map(|param| slot_words(ir_ty_to_jvm(param)) as i32)
                .sum();
            let method = cw.methodref(self.owner, self.name, self.descriptor);
            code.invokestatic(method, arg_words, 0);
        } else {
            emit_object_as(cw, code, Ty::obj(self.owner));
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[0]);
            let value_words = slot_words(ir_ty_to_jvm(&self.params[0])) as i32;
            if self.owner_is_interface {
                let method = cw.interface_methodref(self.owner, self.name, self.descriptor);
                code.invokeinterface(method, value_words, 0);
            } else {
                let method = cw.methodref(self.owner, self.name, self.descriptor);
                code.invokevirtual(method, value_words, 0);
            }
        }
    }

    fn emit_property_value(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, physical: Ty) {
        adapt_property_reference_value(cw, code, self.boxed_value_class, physical);
    }
}

/// Bring an erased `Object` on the stack to the accessor's PHYSICAL parameter type: a value class's
/// boxed object is cast and unboxed to its carrier, anything else is cast or unboxed as usual.
fn adapt_property_reference_value(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    boxed_value_class: Option<TypeName>,
    physical: Ty,
) {
    let Some(value_class) = boxed_value_class else {
        emit_object_as(cw, code, physical);
        return;
    };
    let owner = value_class.render();
    let class = cw.class_ref(&owner);
    code.checkcast(class);
    let descriptor = format!("(){}", type_descriptor(ir_ty_to_jvm(&physical)));
    let method = cw.methodref(&owner, "unbox-impl", &descriptor);
    code.invokevirtual(method, 0, slot_words(ir_ty_to_jvm(&physical)) as i32);
}

fn emit_prop_ref_class(
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let pr = c.prop_ref.as_ref().unwrap();
    let realization = env
        .property_reference_realizations
        .get(c.fq_name_id())
        .expect("a synthesized property reference must retain its JVM realization");
    if pr.static_dispatch {
        return emit_toplevel_prop_ref_class(c, pr, realization, facade, opts);
    }
    if pr.bound {
        return emit_bound_prop_ref_class(c, pr, facade, env, opts);
    }
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)
    add_singleton_instance_field(&mut cw, &fq);

    let target = PropertyReferenceTarget::new(pr, realization, facade, env.bodies);
    emit_property_reference_constructor(&mut cw, &superclass, pr, &target, false);

    let mut get = CodeBuilder::new(2);
    get.aload(1);
    target
        .getter(pr)
        .emit_get(&mut cw, &mut get, target.getter_ret);
    box_property_reference_value(
        &mut cw,
        &mut get,
        pr,
        realization.boxed_value_class,
        target.getter_ret,
    );
    get.areturn();
    finish_code::<0x0001>(
        &mut cw,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &mut get,
        2,
    );

    if pr.mutable {
        let (setter, setter_desc) = property_setter_target(pr, target.facade.is_some());
        let (setter_params, _) =
            parse_physical_method_desc(&setter_desc).expect("validated property setter descriptor");
        let mut set = CodeBuilder::new(3);
        set.aload(1);
        target
            .setter(pr, &setter, &setter_desc, &setter_params)
            .emit_set(&mut cw, &mut set, 2);
        set.ret_void();
        finish_code::<0x0001>(
            &mut cw,
            "set",
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
            &mut set,
            3,
        );
    }

    emit_singleton_instance_clinit(&mut cw, &fq);
    cw.finish()
}

/// Emit a bound property-reference (`obj::prop` → `PropertyReference0Impl` subclass): a constructor
/// `(Object receiver)` delegating to `super(receiver, owner.class, name, "getName()desc", 0)` (the base
/// stores the receiver), and a no-arg `get()` reading `((Owner) this.receiver).getName()`. Constructed
/// per use with the captured receiver — no `INSTANCE` singleton.
fn emit_bound_prop_ref_class(
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER

    let realization = env
        .property_reference_realizations
        .get(c.fq_name_id())
        .expect("a synthesized property reference must retain its JVM realization");
    let target = PropertyReferenceTarget::new(pr, realization, facade, env.bodies);
    emit_property_reference_constructor(&mut cw, &superclass, pr, &target, true);

    // `get()Object`: for a member ref `((Owner) this.receiver).getName()`; for an extension ref
    // `Facade.getName((Owner) this.receiver)`. Boxed if primitive.
    let mut get = CodeBuilder::new(1);
    get.aload(0);
    let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
    get.getfield(recv_f, 1);
    target
        .getter(pr)
        .emit_get(&mut cw, &mut get, target.getter_ret);
    box_property_reference_value(
        &mut cw,
        &mut get,
        pr,
        realization.boxed_value_class,
        target.getter_ret,
    );
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);

    // `set(Object)V` (a bound `var` reference): `((Owner) this.receiver).setName(v)` after
    // casting/unboxing the argument to the property type.
    if pr.mutable {
        let (setter, setter_desc) = property_setter_target(pr, target.facade.is_some());
        let (setter_params, _) =
            parse_physical_method_desc(&setter_desc).expect("validated property setter descriptor");
        let mut set = CodeBuilder::new(2);
        set.aload(0);
        let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
        set.getfield(recv_f, 1);
        target
            .setter(pr, &setter, &setter_desc, &setter_params)
            .emit_set(&mut cw, &mut set, 1);
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
    }
    cw.finish()
}

/// Emit a top-level property reference (`::foo` → `(Mutable)PropertyReference0Impl` subclass): an
/// `INSTANCE` singleton whose `get()` does `invokestatic <facade>.getFoo()` (no receiver), and — for a
/// `var` — a `set(Object)` doing `invokestatic <facade>.setFoo(v)`. The super ctor is the 4-arg
/// `(Class, String, String, int)` form with top-level flags = 1. `owner_internal = None` is the facade
/// sentinel (the declaring file class, unknown until emit).
fn emit_toplevel_prop_ref_class(
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    realization: &crate::jvm::property_references::PropertyReferenceRealization,
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    let owner = pr.owner_or_facade(facade);
    let call_owner = pr.call_owner().unwrap_or_else(|| facade.to_string());
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    add_singleton_instance_field(&mut cw, &fq);

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let prop_desc = type_descriptor(prop_jvm);
    // A receiverless accessor's descriptor is its property's own type. The PropRef's recorded
    // descriptor is not it: for a companion-block or access-bridged property that descriptor names
    // the owner it is called with, which this reference does not pass.
    //
    // A VALUE-CLASS-typed one is the exception, and the only one: its accessors exchange the
    // class's CARRIER, which is what the reference realization recorded on this exact target. The
    // boxed convention this path used to keep named `getTopLevel()LZ;` where the declaration is
    // `getTopLevel()I`.
    let carrier = realization.boxed_value_class.is_some();
    let getter_desc = match (&pr.getter_descriptor, carrier) {
        (Some(descriptor), true) => descriptor.clone(),
        _ => format!("(){prop_desc}"),
    };
    let getter_jvm = getter_desc
        .rsplit_once(')')
        .map(|(_, ret)| ty_from_field_descriptor(ret))
        .unwrap_or(prop_jvm);
    let signature = format!("{}{}", pr.getter_name, getter_desc); // e.g. "getFoo()LBox;"

    // `<init>()V`: super(owner.class, "name", "getName()desc", 1).
    let mut ctor = CodeBuilder::new(1);
    ctor.aload(0);
    ctor.ldc_class(&owner, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(1, &mut cw);
    let sup = cw.methodref(
        &superclass,
        "<init>",
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 4, 0);
    ctor.ret_void();
    finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);

    // `get()Object`: invokestatic <facade>.getName(), boxed if primitive.
    let mut get = CodeBuilder::new(1);
    let gref = cw.methodref(&call_owner, &pr.getter_name, &getter_desc);
    get.invokestatic(gref, 0, slot_words(getter_jvm) as i32);
    if carrier {
        box_property_reference_value(
            &mut cw,
            &mut get,
            pr,
            realization.boxed_value_class,
            getter_jvm,
        );
    } else if prop_jvm.is_jvm_scalar() {
        box_prim_free(
            &mut cw,
            &mut get,
            semantic_scalar_adapter(pr.prop_ty, prop_jvm),
        );
    }
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);

    // `set(Object)V` (a `var`): invokestatic <facade>.setName(v) after casting/unboxing the argument.
    if pr.mutable {
        // The NAME is always the one the reference recorded, exactly as the getter's is: the
        // realization that chose it is the only thing that knows a `@JvmName`, an access bridge or
        // a value-class mangle. Only the DESCRIPTOR depends on whether the accessors exchange the
        // carrier. Pairing the two made a boxed-storage value-class property — `var x: Z?`, whose
        // setter still mangles because its PARAMETER does — fall back to the plain spelling and
        // name a method the facade does not declare.
        let setter = pr
            .setter_name
            .clone()
            .unwrap_or_else(|| property_setter_name(&pr.prop_name));
        let setter_desc = match (&pr.setter_descriptor, carrier) {
            (Some(descriptor), true) => descriptor.clone(),
            _ => format!("({prop_desc})V"),
        };
        let setter_jvm = setter_desc
            .strip_prefix('(')
            .and_then(|rest| rest.split_once(')'))
            .map(|(parameter, _)| ty_from_field_descriptor(parameter))
            .unwrap_or(prop_jvm);
        let mut set = CodeBuilder::new(2);
        set.aload(1);
        if carrier {
            // The argument arrives as the BOXED value class through the erased `set(Object)`; the
            // accessor takes the carrier.
            if let Some(value_class) = realization.boxed_value_class {
                let owner = value_class.render();
                let cref = cw.class_ref(&owner);
                set.checkcast(cref);
                let unbox = cw.methodref(
                    &owner,
                    "unbox-impl",
                    &format!("(){}", type_descriptor(setter_jvm)),
                );
                value_class_boundary_conversion(
                    &mut cw,
                    &mut set,
                    pr.prop_ty.is_nullable(),
                    |_, set| set.invokevirtual(unbox, 0, slot_words(setter_jvm) as i32),
                );
            }
        } else if prop_jvm.is_jvm_scalar() {
            let adapter = semantic_scalar_adapter(pr.prop_ty, prop_jvm);
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(adapter).unwrap_or("java/lang/Object"),
            );
            set.checkcast(wref);
            unbox_prim(&mut cw, &mut set, adapter);
        } else if let Some(internal) = checkcast_internal(prop_jvm) {
            let cref = cw.class_ref(&internal);
            set.checkcast(cref);
        }
        let sref = cw.methodref(&call_owner, &setter, &setter_desc);
        set.invokestatic(sref, slot_words(setter_jvm) as i32, 0);
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
    }

    emit_singleton_instance_clinit(&mut cw, &fq);
    cw.finish()
}

/// The wrapper class internal name for a primitive (`Int` → `java/lang/Integer`), for casting an
/// erased `Object` argument before unboxing.
/// Emit a synthesized function-reference subclass (`<Owner>$ref$N extends FunctionReferenceImpl
/// implements Function<arity>`): an UNBOUND ref gets a `public static final INSTANCE` + a no-arg ctor
/// `super(arity, owner.class, name, sig, flags)`; a BOUND ref gets a `(Object)` ctor delegating to
/// `super(arity, receiver, owner.class, name, sig, flags)` (the base stores the receiver). The single
/// erased `invoke(Object…)Object` casts/unboxes its args and dispatches to the target, boxing the
/// result (or returning the `Unit` singleton for a `void` target). Reference EQUALITY (`::f == ::f`,
/// `a::m != b::m`) is inherited from `FunctionReferenceImpl` (compares owner/name/signature/receiver).
/// Exact in-file function invoked by a function-reference carrier. Direct source references retain
/// their stable checked callable identity; structural adapters retain the generated common-IR id.
fn function_reference_target(ir: &IrFile, reference: &crate::ir::FuncRef) -> Option<u32> {
    reference.local_target.or_else(|| {
        reference
            .module_target
            .and_then(|target| ir.checked_callable_functions.get(&target).copied())
    })
}

fn emit_func_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    use crate::ir::FrDispatch;
    let fr = c.func_ref.as_ref().unwrap();
    let physical_arity = u8::try_from(fr.param_tys.len())
        .expect("JVM function-reference arity must fit its runtime carrier");
    // Structural adapters name their exact common-IR target. Derive the physical JVM parameters
    // from that declaration so sparse representation facts (notably shared mutable-capture cells)
    // cannot disagree between the helper descriptor and the reference carrier's invoke call.
    let target_param_tys = fr
        .local_target
        .map(|target| jvm_function_params(ir, target))
        .unwrap_or_else(|| jvm_tys(&fr.target_param_tys));
    let field_capture_count = fr.field_capture_count as usize;
    let field_capture_tys = target_param_tys
        .get(..field_capture_count)
        .expect("callable-reference captures are a prefix of its adapter parameters");
    let field_capture_descs = field_capture_tys
        .iter()
        .map(|capture| type_descriptor(*capture))
        .collect::<Vec<_>>();
    // A missing `owner_class`/`call_owner` is the facade sentinel (a top-level function lives on the
    // file facade, whose name isn't known until emit) — resolve it here.
    let owner_class = fr.owner_class_or_facade(facade);
    let owner_class = crate::jvm::jvm_class_map::to_jvm_internal(&owner_class).to_string();
    let call_owner = fr.call_owner_or_facade(facade);
    let call_owner = crate::jvm::jvm_class_map::to_jvm_internal(&call_owner).to_string();
    let fq = c.fq_name();
    let superclass = if fr.adapted {
        "kotlin/jvm/internal/AdaptedFunctionReference".to_string()
    } else {
        c.superclass()
    };
    // The carrier's generic header: its runtime base class, then the Kotlin function type it
    // implements (and the suspend marker interface), written as a supertype, without wildcards.
    let suspend = fr.is_suspend || matches!(fr.dispatch, FrDispatch::SuspendConvert);
    let signature = JvmSignatureFormatter::new(ir, env)
        .ty_at(&fr.function_type, Wildcards::Suppressed)
        .map(|function| {
            let marker = if suspend {
                "Lkotlin/coroutines/jvm/internal/SuspendFunction;"
            } else {
                ""
            };
            format!("L{superclass};{function}{marker}")
        });
    let mut cw = new_writer_generic(&fq, signature.as_deref(), &superclass, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    // Package-private, kotlinc's shape — EXCEPT when the class lands cross-package (an
    // INLINE-SPLICED reference regenerates the callee module's adapter under the callee's package
    // while the caller lives elsewhere) or is referenced from a PUBLIC INLINE body
    // (`IrFile::public_synthetics`) — package-private there is an IllegalAccessError (corpus
    // `adaptedSuspendFunctionReference.kt`).
    let cross_package =
        fq.rsplit_once('/').map(|(p, _)| p) != facade.rsplit_once('/').map(|(p, _)| p);
    let inline_reachable = ir.public_synthetics.contains(&c.fq_name_id());
    // kotlinc marks every callable-reference carrier ACC_SYNTHETIC.
    cw.set_access(if cross_package || inline_reachable {
        0x1000 | 0x0001 | 0x0010 | 0x0020 // SYNTHETIC | PUBLIC | FINAL | SUPER
    } else {
        0x1000 | 0x0010 | 0x0020 // SYNTHETIC | FINAL | SUPER
    });
    // kotlinc's enclosure record: the scope the reference is written in, and an inner-only
    // `InnerClasses` entry for the class itself and each class it names.
    if let Some((owner, method)) = class_enclosure(ir, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw.add_interface(&jvm_function_interface(physical_arity));
    if suspend {
        // The suspend-conversion adapter also carries kotlinc's suspend-function marker interface.
        cw.add_interface("kotlin/coroutines/jvm/internal/SuspendFunction");
    }
    for (index, descriptor) in field_capture_descs.iter().enumerate() {
        cw.add_field(0x0012, &format!("$captured${index}"), descriptor);
    }

    // The call argument param types begin AFTER the receiver for an unbound member ref.
    let first_arg = match fr.dispatch {
        FrDispatch::VirtualUnbound => 1usize,
        _ => 0,
    };
    // For `StaticBound` the captured receiver is target arg 0, so invoke arg `k` maps to
    // `target_param_tys[k + 1]`.
    let target_offset =
        field_capture_tys.len() + usize::from(matches!(fr.dispatch, FrDispatch::StaticBound));
    let target_ret_ty = fr
        .local_target
        .map(|target| ir.functions[target as usize].ret)
        .unwrap_or(fr.target_ret_ty);
    let target_ret_jvm = jvm_declared_ty(&target_ret_ty);
    let target_returns_void = matches!(target_ret_ty, Ty::Unit | Ty::Nothing);
    let coerce_unit = fr.ret_ty == Ty::Unit && !target_returns_void;
    // Reflection records the physical target descriptor without an unbound receiver.
    let mut signature_desc = String::from("(");
    let reflection_parameters = fr
        .reflection_target_param_tys
        .as_deref()
        .unwrap_or(&target_param_tys);
    let reflection_first_arg = if fr.reflection_target_param_tys.is_none() {
        first_arg.max(usize::from(fr.reflection_receiver_parameter))
    } else {
        0
    };
    for pt in reflection_parameters.iter().skip(reflection_first_arg) {
        signature_desc.push_str(&ir_type_desc(pt));
    }
    signature_desc.push(')');
    let reflection_target_ret = fr.reflection_target_ret_ty.unwrap_or(fr.target_ret_ty);
    let signature_ret = if matches!(reflection_target_ret, Ty::Unit | Ty::Nothing) {
        "V".to_string()
    } else {
        type_descriptor(jvm_declared_ty(&reflection_target_ret))
    };
    signature_desc.push_str(&signature_ret);
    let reflection_name = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
    let signature_name = match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound | FrDispatch::SuspendConvert => {
            reflection_name
        }
        FrDispatch::VirtualUnbound | FrDispatch::VirtualBound => {
            mapped_builtin_virtual_name(&call_owner, reflection_name, &signature_desc)
        }
    };
    let signature = format!("{signature_name}{signature_desc}");

    let call_desc = if matches!(fr.dispatch, FrDispatch::SuspendConvert) {
        // The delegated call is the wrapped value's ERASED `Function{n}.invoke` — `n` erased Object
        // parameters (the invoke's trailing continuation is dropped), Object return.
        let mut d = String::from("(");
        for _ in 0..physical_arity as usize - 1 {
            d.push_str("Ljava/lang/Object;");
        }
        d.push_str(")Ljava/lang/Object;");
        d
    } else {
        let mut d = String::from("(");
        for pt in target_param_tys.iter().skip(first_arg) {
            d.push_str(&ir_type_desc(pt));
        }
        d.push(')');
        let ret_desc = if target_returns_void {
            "V".to_string()
        } else {
            type_descriptor(target_ret_jvm)
        };
        d.push_str(&ret_desc);
        d
    };

    if !field_capture_tys.is_empty() {
        let capture_words: u16 = field_capture_tys.iter().map(|ty| slot_words(*ty)).sum();
        let ctor_locals = 1 + capture_words + u16::from(fr.bound);
        let mut ctor = CodeBuilder::new(ctor_locals);
        let mut slot = 1u16;
        for (index, ty) in field_capture_tys.iter().copied().enumerate() {
            ctor.aload(0);
            load(ty, slot, &mut ctor);
            let field = cw.fieldref(
                &fq,
                &format!("$captured${index}"),
                &field_capture_descs[index],
            );
            ctor.putfield(field, slot_words(ty) as i32 + 1);
            slot += slot_words(ty);
        }
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        if fr.bound {
            ctor.aload(slot);
        }
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let super_descriptor = if fr.bound {
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
        } else {
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
        };
        let sup = cw.methodref(&superclass, "<init>", super_descriptor);
        ctor.invokespecial(sup, if fr.bound { 6 } else { 5 }, 0);
        ctor.ret_void();
        let descriptor = format!(
            "({}{})V",
            field_capture_descs.concat(),
            if fr.bound { "Ljava/lang/Object;" } else { "" }
        );
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", &descriptor, &mut ctor, ctor_locals);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", &descriptor, &mut ctor, ctor_locals);
        }
    } else if fr.bound {
        // `<init>(Object)V`: super(arity, receiver, owner.class, name, sig, flags).
        cw.seed_utf8("<init>");
        cw.seed_utf8("(Ljava/lang/Object;)V");
        let mut ctor = CodeBuilder::new(2);
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        ctor.aload(1);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 6, 0);
        ctor.ret_void();
        let locals =
            function_reference_invoke::reference_constructor_locals(&mut cw, &fq, &["receiver0"]);
        // The ctor's access mirrors the class's: a PUBLIC synthetic is constructed from other
        // packages by spliced code.
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        }
        cw.set_method_debug("<init>", "(Ljava/lang/Object;)V", None, &locals);
    } else {
        // `<init>()V`: super(arity, owner.class, name, sig, flags).
        cw.seed_utf8("<init>");
        cw.seed_utf8("()V");
        let mut ctor = CodeBuilder::new(1);
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 5, 0);
        ctor.ret_void();
        let locals = function_reference_invoke::reference_constructor_locals(&mut cw, &fq, &[]);
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "()V", &mut ctor, 1);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);
        }
        cw.set_method_debug("<init>", "()V", None, &locals);
    }
    // A singleton carrier's `<clinit>` follows its methods, as kotlinc orders them.
    let singleton = field_capture_tys.is_empty() && !fr.bound;
    if let Some(invoke) = fr.invoke {
        emit_method(ir, invoke, &fq, facade, &mut cw, true, env);
        function_reference_invoke::emit_reference_invoke_bridge(
            ir,
            &mut cw,
            &fq,
            fr,
            invoke,
            physical_arity,
        );
        if singleton {
            emit_singleton_instance_clinit(&mut cw, &fq);
            add_singleton_instance_field(&mut cw, &fq);
        }
        // A reference class is local to the scope it was written in.
        let xi = synthetic_class_xi(SYNTHETIC_LOCAL);
        cw.set_kotlin_metadata(3, &[2, 4, 0], xi, &[], &[]);
        return cw.finish();
    }

    // Numbered JVM function interfaces stop at arity 22. Larger Kotlin function types use the
    // single `FunctionN.invoke(Object[])Object` carrier; their semantic arity remains unchanged.
    let arity = physical_arity as u16;
    let high_arity = is_high_arity_function(physical_arity);
    let invoke_desc = jvm_function_invoke_descriptor(physical_arity);
    let invoke_locals = if high_arity { 2 } else { 1 + arity };
    let mut inv = CodeBuilder::new(invoke_locals);
    for (index, ty) in field_capture_tys.iter().copied().enumerate() {
        inv.aload(0);
        let field = cw.fieldref(
            &fq,
            &format!("$captured${index}"),
            &field_capture_descs[index],
        );
        inv.getfield(field, slot_words(ty) as i32);
    }
    // Push the receiver for a member dispatch (`first_arg`, computed above, skips it in the arg loop).
    match fr.dispatch {
        FrDispatch::VirtualBound | FrDispatch::SuspendConvert => {
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::VirtualUnbound => {
            function_reference_invoke::load_erased_function_argument(
                &mut cw, &mut inv, high_arity, 0,
            );
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::Static => {}
        FrDispatch::StaticBound => {
            // The captured receiver is the FIRST static argument: load `this.receiver`, cast to the
            // target receiver type (after any leading ordinary capture fields).
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            if let Some(vc) = &fr.staticbound_recv_unbox {
                // A VALUE-CLASS receiver (`Z(42)::ext`) is stored BOXED: `checkcast` to the box class then
                // `unbox-impl` to the underlying the mangled target expects (`Z`→`int`).
                let vc = vc.render();
                let cref = cw.class_ref(&vc);
                inv.checkcast(cref);
                let under = jvm_declared_ty(
                    target_param_tys
                        .get(field_capture_count)
                        .copied()
                        .as_ref()
                        .unwrap_or(&Ty::Error),
                );
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(under)));
                inv.invokevirtual(m, 0, slot_words(under) as i32);
            } else if let Some(primitive) = target_param_tys
                .get(field_capture_count)
                .map(jvm_declared_ty)
                .filter(|ty| ty.is_jvm_scalar())
            {
                unbox_prim_from(&mut cw, &mut inv, Ty::obj("java/lang/Object"), primitive);
            } else if let Some(internal) = target_param_tys
                .get(field_capture_count)
                .map(jvm_declared_ty)
                .and_then(checkcast_internal)
            {
                let cref = cw.class_ref(&internal);
                inv.checkcast(cref);
            }
        }
    };
    // Push the call arguments (cast/unbox each erased `Object`).
    let mut call_arg_words = field_capture_tys
        .iter()
        .map(|ty| slot_words(*ty) as i32)
        .sum::<i32>()
        + match fr.dispatch {
            // The captured receiver already pushed above occupies one (reference) target slot.
            FrDispatch::StaticBound => target_param_tys
                .get(field_capture_count)
                .map_or(0, |t| slot_words(jvm_declared_ty(t)) as i32),
            _ => 0,
        };
    for (k, pt) in fr.param_tys.iter().enumerate().skip(first_arg) {
        // Suspend conversion: the trailing continuation parameter is NOT forwarded — the wrapped
        // plain function never suspends and takes only the value arguments.
        if matches!(fr.dispatch, FrDispatch::SuspendConvert) && k == fr.param_tys.len() - 1 {
            continue;
        }
        function_reference_invoke::load_erased_function_argument(&mut cw, &mut inv, high_arity, k);
        let jt = ir_ty_to_jvm(pt);
        let target_jt = target_param_tys
            .get(k + target_offset)
            .map(jvm_declared_ty)
            .unwrap_or(jt);
        let value_class_unbox = fr
            .unbox_params
            .get(k)
            .and_then(|value| value.as_ref())
            .filter(|_| !jt.is_jvm_scalar());
        if value_class_unbox.is_some() {
            // `FunctionN.invoke` receives the boxed VALUE CLASS as Object. Its physical target can
            // use the underlying reference type (for example `Value(String)` -> `String`), but that
            // does not license casting the incoming object to the underlying type. The boxed class
            // owns the representation boundary: cast to it, then call `unbox-impl` below.
        } else if jt.is_jvm_scalar() && target_jt.is_jvm_scalar() {
            let adapter = semantic_scalar_adapter(*pt, jt);
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(adapter).unwrap_or("java/lang/Object"),
            );
            inv.checkcast(wref);
            unbox_prim(&mut cw, &mut inv, adapter);
        } else if jt.is_jvm_scalar() && target_jt.is_reference() {
            let target = crate::jvm::names::instanceof_internal_name(target_jt);
            if target != "java/lang/Object" {
                let cref = cw.class_ref(&target);
                inv.checkcast(cref);
            }
        } else if let Some(internal) = checkcast_internal(target_jt) {
            let cref = cw.class_ref(&internal);
            inv.checkcast(cref);
        }
        if let Some(vc) = value_class_unbox {
            emit_value_class_unbox_adapter(
                &mut cw,
                &mut inv,
                *vc,
                target_jt,
                fr.unbox_param_nullable.get(k).copied().unwrap_or(false),
            );
        }
        call_arg_words += slot_words(target_jt) as i32;
    }
    // Dispatch to the target.
    let ret_words = if target_returns_void {
        0
    } else {
        slot_words(target_ret_jvm) as i32
    };
    // A reference to a PRIVATE same-file top-level function can't invokestatic it from this
    // (separate) class — call kotlinc's `access$<name>` facade bridge instead (`emit_pass` emits it
    // for exactly these referenced targets).
    let static_call_name = if fr.call_owner_is_facade()
        && function_reference_target(ir, fr)
            .is_some_and(|target| ir.private_methods.contains(&target))
    {
        format!("access${}", fr.call_name)
    } else {
        fr.call_name.clone()
    };
    match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound => {
            let m = cw.methodref(&call_owner, &static_call_name, &call_desc);
            inv.invokestatic(m, call_arg_words, ret_words);
        }
        // A bound reference to a mapped-builtin member (`"KOTLIN"::get`) invokes the same PHYSICAL JVM
        // method a direct call would (`String.get` → `charAt`) — apply the backend's name mapping here too.
        _ if fr.call_interface => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name, &call_desc);
            let m = cw.interface_methodref(&call_owner, vn, &call_desc);
            inv.invokeinterface(m, call_arg_words, ret_words);
        }
        _ => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name, &call_desc);
            let m = cw.methodref(&call_owner, vn, &call_desc);
            inv.invokevirtual(m, call_arg_words, ret_words);
        }
    }
    // Adapt the result to `Object`: a `void` target yields the `Unit` singleton; a value-class-returning
    // reference boxes the erased underlying back to the value class; a plain primitive is wrapper-boxed.
    if target_returns_void {
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if coerce_unit {
        discard(target_ret_jvm, &mut inv);
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if let Some(owner) = &fr.box_ret {
        let owner = owner.render();
        // A value-class-returning reference: the target returns the ERASED underlying (primitive or the
        // reference underlying) — exactly what `call_desc` requested. Box it back to the value class via
        // `box-impl` so the `Function` result is the boxed VC (`X` object) the invariant requires — a VC in
        // a `FunctionN` slot is boxed. Without it a `typeAdapter::decode` returning `X` hands back the bare
        // underlying that the caller then `checkcast X`es → `ClassCastException`.
        let bi = cw.methodref(
            &owner,
            "box-impl",
            &format!("({})L{};", type_descriptor(target_ret_jvm), owner),
        );
        inv.invokestatic(bi, slot_words(target_ret_jvm) as i32, 1);
    } else if target_ret_jvm.is_jvm_scalar() {
        // `invoke` returns `Object` regardless of the target descriptor. Preserve the function
        // reference's semantic return while selecting that wrapper; the target carrier alone cannot
        // distinguish `UInt` from `Int`.
        box_prim_free(
            &mut cw,
            &mut inv,
            semantic_scalar_adapter(fr.ret_ty, target_ret_jvm),
        );
    }
    inv.areturn();
    finish_code::<0x0001>(&mut cw, "invoke", &invoke_desc, &mut inv, invoke_locals);
    // kotlinc visits a carrier's fields after its methods.
    if singleton {
        emit_singleton_instance_clinit(&mut cw, &fq);
        add_singleton_instance_field(&mut cw, &fq);
    }
    cw.finish()
}

fn verif_for_jvm_free(cw: &mut ClassWriter, t: Ty) -> VerifType {
    match t {
        t if is_jvm_int_category(t) => VerifType::Integer,
        Ty::Long => VerifType::Long,
        Ty::Double => VerifType::Double,
        Ty::Float => VerifType::Float,
        Ty::String => VerifType::Object(cw.class_ref("java/lang/String")),
        t if t.is_array() => VerifType::Object(cw.class_ref(&type_descriptor(t))),
        Ty::Obj(n, _) => VerifType::Object(
            cw.class_ref(&crate::jvm::names::classfile_internal_name(&n.render())),
        ),
        Ty::Null => VerifType::Null,
        _ => VerifType::Top,
    }
}

fn emit_value_class_unbox_adapter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    value_class: TypeName,
    target: Ty,
    nullable: bool,
) {
    let value_class = value_class.render();
    let value_class_ref = cw.class_ref(&value_class);
    code.checkcast(value_class_ref);
    let unbox = cw.methodref(
        &value_class,
        "unbox-impl",
        &format!("(){}", type_descriptor(target)),
    );
    if !nullable {
        code.invokevirtual(unbox, 0, slot_words(target) as i32);
        return;
    }
    let null = code.new_label();
    let end = code.new_label();
    code.dup();
    code.ifnull(null);
    code.invokevirtual(unbox, 0, slot_words(target) as i32);
    code.goto(end);
    code.bind(null);
    code.pop();
    code.aconst_null();
    code.bind(end);
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

/// The `@java.lang.annotation.Target` mirror for an annotation class whose source declares
/// `@Target(...)`, or `None` when it declares none — an ABSENT `@Target` and an EMPTY one are
/// different classfiles (the latter mirrors to `value = []`, and kotlinc emits it).
///
/// The Kotlin target set is PROJECTED, not copied: [`crate::types::java_element_type_of_annotation_target`]
/// maps each target to at most one `ElementType`, Kotlin-only targets drop out, and the survivors are
/// deduplicated and ordered by `ElementType` declaration order (kotlinc builds an `EnumSet`). The
/// projection can be empty — a set of only Kotlin-only targets still mirrors to `value = []`, matching
/// kotlinc; it is the ABSENCE of `@Target` in the source, not an empty projection, that omits it.
fn java_target_mirror(
    applied: &crate::ir::DeclarationAnnotations,
) -> Option<crate::ir::AppliedAnnotation> {
    let kotlin_target = crate::types::type_name("kotlin/annotation/Target");
    let declared = applied
        .iter()
        .map(|retained| &retained.annotation)
        .find(|annotation| annotation.internal == kotlin_target)?;
    let (_, crate::ir::AnnoValue::Array(targets)) = declared
        .values
        .iter()
        .find(|(name, _)| name == "allowedTargets")?
    else {
        return None;
    };
    let mut elements: Vec<usize> = targets
        .iter()
        .filter_map(|target| match target {
            crate::ir::AnnoValue::Enum(_, name) => {
                crate::types::java_element_type_of_annotation_target(name)
            }
            _ => None,
        })
        .collect();
    elements.sort_unstable();
    elements.dedup();
    Some(crate::ir::AppliedAnnotation {
        internal: crate::types::type_name("java/lang/annotation/Target"),
        values: vec![(
            "value".to_string(),
            crate::ir::AnnoValue::Array(
                elements
                    .into_iter()
                    .map(|element| {
                        crate::ir::AnnoValue::Enum(
                            crate::types::type_name("java/lang/annotation/ElementType"),
                            crate::types::JAVA_ELEMENT_TYPES[element].to_string(),
                        )
                    })
                    .collect(),
            ),
        )],
    })
}

/// Emit a Kotlin `annotation class` as a JVM ANNOTATION INTERFACE: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT|
/// ACC_ANNOTATION`, extending `java/lang/annotation/Annotation`, with one `public abstract` accessor per
/// member (`int x()`, `String s()`) named after the property and returning its type — kotlinc's shape.
/// Members come from `fields`. Instances are built by the synthetic impl ([`emit_annotation_impl_class`]).
fn emit_annotation_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, "java/lang/Object", opts);
    // PUBLIC | INTERFACE | ABSTRACT | ANNOTATION
    cw.set_access(class_public_bit(ir, c) | 0x0200 | 0x0400 | 0x2000);
    cw.add_interface("java/lang/annotation/Annotation");
    for field in &c.fields {
        let ret = jvm_declared_ty(&field.ty);
        cw.add_abstract_method(0x0401, &field.name, &format!("(){}", type_descriptor(ret)));
        // PUBLIC|ABSTRACT
    }
    emit_jvm_interface_companion_surface(ir, c, facade, env, &mut cw);
    // Retention/target meta-annotations, matching kotlinc's ORDER: everything the source declares
    // comes first, in source order — `kotlin.annotation.Retention(X)` and `kotlin.annotation.Target`
    // among them — and the JVM mirrors are appended after: `java.lang.annotation.Retention(RUNTIME|
    // CLASS|SOURCE)` (RUNTIME when defaulted), then `java.lang.annotation.Target`. The java retention
    // is what both the JVM and classpath consumers read the retention back from.
    let enum_stamp = |internal: &str, enum_ty: &str, constant: &str| crate::ir::AppliedAnnotation {
        internal: crate::types::type_name(internal),
        values: vec![(
            "value".to_string(),
            crate::ir::AnnoValue::Enum(crate::types::type_name(enum_ty), constant.to_string()),
        )],
    };
    let mut mirrors: Vec<crate::ir::AppliedAnnotation> = Vec::new();
    if let Some(retention) = c.annotation_retention {
        use crate::ir::AnnoRetention;
        let policy = match retention {
            AnnoRetention::Default | AnnoRetention::Runtime => "RUNTIME",
            AnnoRetention::Binary => "CLASS",
            AnnoRetention::Source => "SOURCE",
        };
        mirrors.push(enum_stamp(
            "java/lang/annotation/Retention",
            "java/lang/annotation/RetentionPolicy",
            policy,
        ));
    }
    mirrors.extend(java_target_mirror(&c.applied_annotations));
    // The source's own `@Retention` is REPLACED IN PLACE by the normalized stamp rather than filtered
    // out and re-appended: the class's `annotation_retention` is the authority for the value, but the
    // annotation's position among the others is the source's and kotlinc preserves it.
    let kotlin_retention = crate::types::type_name("kotlin/annotation/Retention");
    let kotlin_retention_stamp = c.annotation_retention.and_then(|retention| {
        use crate::ir::AnnoRetention;
        let constant = match retention {
            AnnoRetention::Default => return None,
            AnnoRetention::Runtime => "RUNTIME",
            AnnoRetention::Binary => "BINARY",
            AnnoRetention::Source => "SOURCE",
        };
        Some(enum_stamp(
            "kotlin/annotation/Retention",
            "kotlin/annotation/AnnotationRetention",
            constant,
        ))
    });
    let user_annotations = crate::ir::DeclarationAnnotations::new(
        c.applied_annotations
            .iter()
            .filter_map(|retained| {
                if retained.annotation.internal != kotlin_retention {
                    return Some(retained.clone());
                }
                kotlin_retention_stamp
                    .clone()
                    .map(|annotation| crate::ir::RetainedAnnotation {
                        retention: retained.retention,
                        annotation,
                    })
            })
            .collect(),
    );
    cw.set_class_annotations(&user_annotations);
    cw.set_runtime_annotations(&mirrors);
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JvmArrayActualRealization {
    Get,
    Set,
    Size,
}

fn array_actual_element_matches(receiver: Ty, declared: Ty) -> bool {
    let Some(stored) = receiver.array_elem() else {
        return false;
    };
    if receiver.is_reference_array() {
        let declared_stored = reference_array_element(ir_ty_to_jvm(&declared));
        type_descriptor(declared_stored) == type_descriptor(ir_ty_to_jvm(&stored))
    } else {
        declared == stored
    }
}

/// The JVM realization of an already-selected Kotlin array `actual` declaration. This recognizes the
/// declaration's complete semantic identity; a same-named function with another owner or signature is
/// an ordinary call. Metadata supplies these declarations, while only the JVM emitter knows that their
/// bodies are array bytecodes rather than methods on a loadable `kotlin/*Array` class.
fn jvm_array_actual_realization(
    owner: TypeName,
    name: &str,
    receiver: Ty,
    params: &[Ty],
    ret: Ty,
) -> Option<JvmArrayActualRealization> {
    if !receiver.is_array() || receiver.non_null().obj_internal() != Some(owner) {
        return None;
    }
    match (name, params, ret) {
        ("get", [Ty::Int], declared_ret)
            if array_actual_element_matches(receiver, declared_ret) =>
        {
            Some(JvmArrayActualRealization::Get)
        }
        ("set", [Ty::Int, declared_element], Ty::Unit)
            if array_actual_element_matches(receiver, *declared_element) =>
        {
            Some(JvmArrayActualRealization::Set)
        }
        ("size", [], Ty::Int) => Some(JvmArrayActualRealization::Size),
        _ => None,
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
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }
    register_sealed_subtypes(
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
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
            if env
                .run
                .private_member_access_bridges
                .borrow()
                .contains(&fid)
            {
                access_bridges::emit_private_member_access_bridge(ir, fid, &fq_name, &mut cw, true);
            }
            if ir.function_reference_access_bridges.contains(&fid) {
                access_bridges::emit_function_reference_access_bridge(
                    ir, fid, &fq_name, &mut cw, true,
                );
            }
            // A PRIVATE default stays a plain private instance method: kotlinc gives it no bridge,
            // no holder entry, and no forwarders anywhere (measured: an interface whose only body
            // is private has NO `$DefaultImpls` at all). A synthesized erasure bridge or an
            // inline-only lambda impl is not a source member either.
            if enable_compat
                && !f.is_static
                && !ir.private_methods.contains(&fid)
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
                let member_desc = ir_method_desc(&f.params, &f.ret);
                let signature = holder_method_signature(
                    &signature_formatter,
                    ir,
                    c.fq_name,
                    method_signature(&signature_formatter, ir, fid, f).as_deref(),
                    &member_desc,
                );
                // Annotation selection reads the SEMANTIC member types when recorded — `f.ret` is
                // already erased, and an erased `T` return must not read as `@NotNull Object`.
                let (semantic_params, semantic_ret) = match ir.member_semantic_sigs.get(&fid) {
                    Some((params, ret)) => (params.clone(), *ret),
                    None => (jd_declared_param_tys(ir, fid), f.ret),
                };
                let physical_params = jvm_function_params(ir, fid);
                let assertion_names =
                    crate::jvm::parameter_names::function_assertions(ir, fid, &physical_params)
                        .expect("a compatibility declaration carries exact assertion identities");
                let guards = if opts.param_assertions {
                    f.param_checks
                        .iter()
                        .enumerate()
                        .map(|(index, check)| {
                            check.as_ref().map(|_| {
                                let _identity = ir
                                    .function_parameter_identities(fid)
                                    .and_then(|identities| identities.get(index))
                                    .expect("a checked compatibility parameter has an identity");
                                assertion_names[index]
                                    .clone()
                                    .expect("a checked compatibility parameter has a JVM label")
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let parameter_names =
                    crate::jvm::parameter_names::function_locals(ir, fid, &physical_params)
                        .expect("a compatibility declaration carries exact parameter identities");
                let method_parameter_names =
                    crate::jvm::parameter_names::function_method_parameters(
                        ir,
                        fid,
                        &physical_params,
                    )
                    .expect("a compatibility declaration carries exact reflection identities");
                interface_compatibility::emit_holder_forward(
                    di,
                    c.fq_name,
                    &f.name,
                    &physical_params,
                    &semantic_params,
                    &parameter_names,
                    &method_parameter_names,
                    &guards,
                    jvm_declared_ty(&f.ret),
                    semantic_ret,
                    signature.as_deref(),
                    // A property accessor has no `fn_decl_lines` entry — its line lives on the
                    // property declaration it realizes.
                    ir.fn_decl_lines.get(&fid).copied().unwrap_or_else(|| {
                        c.properties
                            .iter()
                            .find(|property| {
                                let (getter, setter) = accessor_jvm_names(c, &property.name);
                                getter == f.name || setter == f.name
                            })
                            .map(|property| property.decl_line)
                            .unwrap_or(0)
                    }),
                    opts.java_parameters,
                    JdHolderTarget::AccessBridge,
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
            let desc = ir_method_desc(&f.params, &f.ret);
            cw.add_abstract_method_sig(
                0x0001 | 0x0400,
                &f.name,
                &desc,
                method_signature(&signature_formatter, ir, fid, f).as_deref(),
            );
            // An abstract method still carries kotlinc's nullability annotations.
            let (result, params) = super::abstract_method_nullability::annotations(ir, fid, f);
            if result.is_some() || params.iter().any(Option::is_some) {
                cw.set_method_nullability(&f.name, &desc, result, &params);
            }
            // PUBLIC | ABSTRACT
        }
        // An interface method with default parameters gets a STATIC `<name>$default(iface, params…, mask,
        // marker)` (the JVM realization of interface default args) — it applies the defaults then dispatches
        // to the abstract method via `invokeinterface`. kotlinc emits it ON THE INTERFACE (call sites use
        // it) AND, under a mode that keeps the compatibility holder, a copy on the
        // `<Iface>$DefaultImpls` class (`public final`).
        let deferred_suspend_declaration = ir
            .jvm_suspend_interface_bodies
            .values()
            .any(|(_, declaration)| *declaration == fid);
        let default_fid = ir
            .jvm_suspend_interface_bodies
            .get(&fid)
            .map(|(_, declaration)| *declaration)
            .unwrap_or(fid);
        if !deferred_suspend_declaration {
            if let Some(defaults) = ir.param_defaults(default_fid) {
                // `disable` puts NOTHING executable on the interface, the `$default` stub included: call
                // sites go to the holder's copy instead.
                if bodies_on_interface {
                    emit_default_stub(
                        ir,
                        default_fid,
                        &fq_name,
                        facade,
                        &mut cw,
                        defaults,
                        env,
                        true,
                    );
                }
                // `-jvm-default=no-compatibility` emits NO `$DefaultImpls` at all. Emitting one anyway
                // would publish a holder class the build says does not exist — a downstream compilation
                // resolving against it links to a class kotlinc would never have produced.
                if emits_default_impls {
                    let di = default_impls.get_or_insert_with(|| {
                        let mut w = new_writer(
                            &format!("{fq_name}$DefaultImpls"),
                            "java/lang/Object",
                            opts,
                        );
                        w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                        w
                    });
                    if enable_compat {
                        // The interface owns the real default application; the holder's copy is a
                        // thin synthetic forward to it (kotlinc's `enable` shape).
                        emit_default_stub_forward(ir, default_fid, &fq_name, di);
                    } else {
                        emit_default_stub(
                            ir,
                            default_fid,
                            &fq_name,
                            facade,
                            di,
                            defaults,
                            env,
                            true,
                        );
                    }
                }
            }
        }
    }
    // The `access$…$jd` bridges follow every declared member (kotlinc's order): declared bodies
    // first, then the republished surface for inherited defaults this interface does not redeclare.
    for &fid in &jd_bridge_fids {
        let f = &ir.functions[fid as usize];
        let physical_params = jvm_function_params(ir, fid);
        let parameter_names =
            crate::jvm::parameter_names::function_locals(ir, fid, &physical_params)
                .expect("an access bridge carries exact declaration parameter identities");
        emit_jd_access_bridge(
            &mut cw,
            c.fq_name,
            c.decl_line,
            &f.name,
            &physical_params,
            &parameter_names,
            jvm_declared_ty(&f.ret),
        );
    }
    if enable_compat {
        interface_compatibility::emit_inherited_default_surface(
            ir,
            c,
            &mut cw,
            &mut default_impls,
            opts,
            env,
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
        di.set_kotlin_metadata(3, &[2, 4, 0], xi, &[], &[]);
        extra.push((holder, di.finish()));
    }
    emit_jvm_interface_companion_surface(ir, c, facade, env, &mut cw);
    // A user annotation on an interface is emitted exactly as on a class — kotlinc writes it BEFORE the
    // `@Metadata` entry, which is the order these queue in.
    cw.set_class_annotations(&c.applied_annotations);
    // An interface is a VIEW of the same `IrClass` every other kind is — compute its `@Metadata` (and
    // therefore its debug tables/annotations) through the shared path, exactly like `emit_class`.
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if computed.is_some() {
        attach_synth_debug_tables(ir, c, &mut cw, opts.param_assertions, None, &[]);
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(ir, c, &mut cw);
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    if emitted_default_impls {
        let holder = format!("{fq_name}$DefaultImpls");
        // The outer interface must publish the same nested-class relation as the synthetic holder.
        // Seed after metadata so the class constant lands in kotlinc's class-visit order.
        cw.seed_class(&holder);
        cw.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: holder,
            outer: Some(fq_name.clone()),
            name: Some("DefaultImpls".to_string()),
            access: 0x0019,
        });
    }
    cw.finish()
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
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }

    let field_tys = field_jvm_tys(&c.fields);
    // (bridges emitted after the methods below — `emit_bridges` references emitted method refs)
    let n_params = c.ctor_param_count as usize;
    let user_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    let all_param_tys = class_ctor_jvm_tys(c);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(all_param_tys.iter().copied())
        .collect();
    let ctor_desc = method_descriptor(&ctor_params, Ty::Unit);
    // The generic signature omits the enum ABI prefix and retains only source parameters.
    let ctor_sig = format!(
        "({})V",
        all_param_tys
            .iter()
            .map(|ty| type_descriptor(*ty))
            .collect::<String>()
    );
    let ctor_parameters = if env.java_parameters {
        super::method_parameters::enum_constructor(c).to_vec()
    } else {
        Vec::new()
    };
    // Property backing fields are private (kotlinc), reached through the synthesized `getX()`/`setX()`
    // accessors — for both the primary-constructor fields and body member-property fields
    // (`enum class E { A; val x = … }`), initialized in the constructor via `init_body`.
    let enum_field_acc = |f: &IrField| {
        (if f.is_private() { 0x0002 } else { 0x0001 }) | if f.is_final() { 0x0010 } else { 0 }
    };
    // kotlinc emits an ENUM's `Companion` field FIRST — ahead of the constructor properties, the
    // entry constants and `$VALUES`/`$ENTRIES` — unlike an ordinary class, where it follows the
    // instance fields. Emitting it last also interned its name and descriptor late, so the class
    // differed in constant-pool order even where every member matched.
    let owner_statics: Vec<&crate::ir::IrStatic> =
        ir.statics.iter().filter(|s| s.owner_matches(&fq)).collect();
    // The `Companion` field LEADS the field table, but kotlinc interns its name and descriptor at
    // the field VISIT — late, not here. Emitting it eagerly put those strings at the head of the
    // constant pool and reordered nearly all of it.
    if let Some(companion) = c.companion_class {
        cw.add_field_late_leading(
            0x0019,
            companion.nested_segment_ref(),
            &format!("L{};", companion.render()),
        );
    }
    // A `@Serializable enum`'s `$cachedSerializer$delegate` sits directly after `Companion` and
    // BEFORE the constructor properties and entry constants — the same leading block, not the tail.
    for s in ir.statics.iter().filter(|s| s.owner_matches(&fq)) {
        // A `var` is reassignable, so it must not carry ACC_FINAL (see the class path).
        let final_flag = if s.is_var { 0x0000 } else { 0x0010 };
        let acc = if s.visibility.is_private() {
            0x000A | final_flag // PRIVATE | STATIC [| FINAL]
        } else {
            0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
        };
        // A PARAMETERIZED static keeps its generic `Signature` (the delegate is a
        // `Lazy<KSerializer<Object>>`), and a reference-typed one carries kotlinc's nullability
        // annotation — the same treatment the class and facade field tables give their statics.
        let signatures = property_jvm_signatures(&signature_formatter, &s.ty, None);
        let ann = (field_nullability_kind(ir, &fq, &s.name, s.ty) == 1)
            .then_some("Lorg/jetbrains/annotations/NotNull;");
        // The delegate follows the CONSTRUCTOR PROPERTIES and precedes the entry constants —
        // `Companion, value, $cachedSerializer$delegate, <entries>`. A fixture with no properties
        // cannot tell that apart from "directly after Companion", which is why this needs the
        // explicit index rather than the plain leading form.
        let at = usize::from(c.companion_class.is_some()) + c.fields.len();
        cw.add_field_late_at_sig(
            acc,
            &s.name,
            &ir_type_desc(&s.ty),
            signatures.field.as_deref(),
            ann,
            at,
        );
    }
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
    // The property backing fields intern AFTER the constructor's names, descriptors and its
    // `super(name, ordinal)` reference, and BEFORE the constructor's LocalVariableTable strings —
    // kotlinc's visit order. Each field also mints its `NameAndType`/`Fieldref` here, where kotlinc
    // does, rather than later at the constructor's `putfield`.
    for (f, t) in c.fields[..n_params].iter().zip(&user_tys) {
        let desc = type_descriptor(*t);
        cw.add_field(enum_field_acc(f), &f.name, &desc);
        cw.fieldref(&fq, &f.name, &desc);
    }
    for (f, t) in c.fields[n_params..].iter().zip(&field_tys[n_params..]) {
        let desc = type_descriptor(*t);
        cw.add_field(enum_field_acc(f), &f.name, &desc);
        cw.fieldref(&fq, &f.name, &desc);
    }
    cw.reserve_method_name("this");
    cw.reserve_descriptor(&self_desc);
    cw.reserve_method_name("$enum$name");
    cw.reserve_descriptor("Ljava/lang/String;");
    cw.reserve_method_name("$enum$ordinal");
    cw.reserve_descriptor("I");
    // The DECLARED members come next — kotlinc reaches a property's accessor (`getTag`, its
    // descriptor, its `@NotNull`) before any of the synthesized machinery below. Emitting them only
    // at their method visit left those strings after `values`/`$VALUES` and shifted the pool.
    for (f, t) in c.fields[..n_params].iter().zip(&user_tys) {
        cw.reserve_method_name(&property_getter_name(&f.name));
        cw.reserve_descriptor(&format!("(){}", type_descriptor(*t)));
        if field_nullability_kind(ir, &fq, &f.name, *t) == 1 {
            cw.reserve_descriptor("Lorg/jetbrains/annotations/NotNull;");
        }
    }
    // …then the synthesized members, in kotlinc's visit order, each with the entries its body
    // references: `values()` reads `$VALUES` and calls `Object.clone()`; `valueOf` delegates to
    // `Enum.valueOf` and names its parameter `value`; `getEntries` returns the `@NotNull`
    // `$ENTRIES`. Only after all of them does kotlinc reach the entry constants themselves.
    cw.reserve_method_name("values");
    cw.reserve_descriptor(&format!("(){arr_desc}"));
    cw.reserve_method_name("$VALUES");
    cw.reserve_descriptor(&arr_desc);
    cw.fieldref(&fq, "$VALUES", &arr_desc);
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
    cw.reserve_method_name("$ENTRIES");
    cw.reserve_descriptor("Lkotlin/enums/EnumEntries;");
    cw.fieldref(&fq, "$ENTRIES", "Lkotlin/enums/EnumEntries;");
    cw.reserve_method_name("$values");
    // One static-final constant per entry, plus the private `$VALUES` array.
    for entry in &c.enum_entries {
        cw.add_field(0x0001 | 0x0008 | 0x0010 | ACC_ENUM, &entry.name, &self_desc);
        // `<clinit>`'s `putstatic` for this entry resolves right after the field's own name, before
        // the next entry — kotlinc interleaves them rather than batching the Fieldrefs at the end.
        cw.fieldref(&fq, &entry.name, &self_desc);
        apply_enum_entry_annotations(&mut cw, c, &entry.name);
    }
    cw.add_field(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$VALUES",
        &arr_desc,
    );
    // The `entries` property backing (Kotlin 2.x emits this on EVERY enum): a `private static final`
    // `kotlin/enums/EnumEntries`, initialized in `<clinit>` from `EnumEntriesKt.enumEntries($VALUES)`.
    cw.add_field(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$ENTRIES",
        "Lkotlin/enums/EnumEntries;",
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
    // `(pc, line)` of each constructor-property store, for the `LineNumberTable` below.
    let mut store_lines: Vec<(u16, u32)> = Vec::new();
    let mut max_locals = 1 + ctor_words;
    // Store constructor-property parameters unless lowering already represented those stores in the
    // init body. The existence of a body property is not that contract: an init body can contain
    // only body-property stores while leaving `explicit_param_stores` false.
    if !c.explicit_param_stores {
        let mut slot = 3u16;
        let mut field_i = 0usize;
        for (argument, ty) in c.ctor_args.iter().zip(&all_param_tys) {
            if argument.is_field {
                let name = &c.fields[field_i].name;
                if let Some(line) = property_line(ir, c, field_i as u32) {
                    store_lines.push((ctor.bytes.len() as u16, line));
                }
                ctor.aload(0);
                load(*ty, slot, &mut ctor);
                let field = cw.fieldref(&fq, name, &type_descriptor(*ty));
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
        let mut e = Emitter::new(ir, &mut cw, env, &fq, facade, Ty::Unit, [init_body]);
        let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj(&fq));
        e.slots.insert(0, (receiver, Ty::obj(&fq)));
        // The synthetic name and ordinal come first; no semantic value names them.
        e.frame.enter(FrameKey::Parameter(0), Ty::String);
        e.frame.enter(FrameKey::Parameter(1), Ty::Int);
        for (i, t) in all_param_tys.iter().enumerate() {
            let value = i as u32 + 1;
            let s = e.frame.enter(FrameKey::Value(value), *t);
            e.slots.insert(value, (s, *t));
        }
        e.emit_constructor_init_body(c, init_body, &mut ctor, &mut store_lines);
        max_locals = max_locals.max(e.frame.max());
    }
    // The pc the trailing `return` starts at — kotlinc maps it back to the class HEADER line.
    let ctor_return_pc = ctor.bytes.len() as u16;
    ctor.ret_void();
    ctor.ensure_locals(max_locals);
    ctor.link();
    // A plain enum's constructor is `private` (matching kotlinc — javap then hides the synthetic
    // `(String,int)` params in its display). A subclassed enum's ctor must be reachable from its entry
    // subclasses' `<init>` (an `invokespecial` from another class): kotlinc keeps it `private` and relies
    // on nestmate access, which krusty doesn't emit, so it stays package-private + synthetic here.
    let base_ctor_acc = if has_subclass { ACC_SYNTHETIC } else { 0x0002 };
    // kotlinc emits a generic `Signature` on the enum ctor listing only the USER params (the synthetic
    // leading `(String, int)` are excluded) — e.g. `()V` for a plain enum, `(I)V` for `E(val n: Int)`.
    // javap reads it to display `Color()` instead of `Color(String, int)`; without it the synthetic
    // params leak into the disassembly (a per-enum divergence from kotlinc).
    // An enum declaring ONLY secondary constructors has no primary to emit: every entry names one
    // of the secondaries. Registering the synthesized primary anyway collided with a no-argument
    // secondary — both are `(String, int)V` — and the class failed to load with a
    // `ClassFormatError: Duplicate method name "<init>"`. Its bytes are still built above so the
    // constant pool interns in kotlinc's order.
    let emits_primary_ctor = c.has_primary_ctor || c.secondary_ctors.is_empty();
    if emits_primary_ctor {
        cw.add_method_sig(base_ctor_acc, "<init>", &ctor_desc, &ctor, Some(&ctor_sig));
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
            let mut entries = vec![(0u16, start)];
            if !store_lines.is_empty() {
                // Each retained store maps to its property's source line; the trailing `return`
                // maps back to the class header. A missing line is not reconstructed in the backend.
                entries.extend(store_lines.iter().copied());
                entries.push((ctor_return_pc, header));
            }
            // kotlinc never emits two consecutive entries for the same line.
            entries.dedup_by_key(|(_, l)| *l);
            cw.set_method_lines("<init>", &ctor_desc, &entries);
        }
    }
    if let Some(defaults) = ir
        .class_ctor_defaults(&fq)
        .filter(|defaults| defaults.iter().any(Option::is_some))
    {
        constructor_defaults::emit_ctor_default_stub_with_prefix(
            ir,
            &fq,
            facade,
            &[Ty::String, Ty::Int],
            0,
            &all_param_tys,
            defaults,
            None,
            false,
            false,
            base_ctor_acc | ACC_SYNTHETIC,
            &mut cw,
            env,
        );
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
    let markers = property_annotation_marker_fids(ir, c);
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
                emit_method(ir, fid, &fq, facade, cw, !f.is_static, env);
                if ir.function_reference_access_bridges.contains(&fid) {
                    access_bridges::emit_function_reference_access_bridge(ir, fid, &fq, cw, false);
                }
                // A defaulted member needs its `<name>$default` synthetic here too. The enum writer is a
                // separate path from `emit_class`, so a member declared `fun m(a: Int = 1)` on an enum
                // silently had no stub at all — a call omitting the argument had nothing to dispatch to.
                // Same call as the class path, so the super-call guard rides along: an enum IS
                // inheritable (an entry body subclasses it), which is why kotlinc guards the stub.
                if let Some(defaults) = ir.param_defaults(fid) {
                    emit_default_stub(ir, fid, &fq, facade, cw, defaults, env, false);
                }
            } else {
                // An abstract enum member (`abstract fun t(): String`) — declared `ACC_ABSTRACT`, the
                // entry subclasses override it.
                cw.add_abstract_method_sig(
                    0x0001 | 0x0400,
                    &f.name,
                    &ir_method_desc(&f.params, &f.ret),
                    method_signature(&signature_formatter, ir, fid, f).as_deref(),
                );
            }
        }
    };
    emit_members(&mut cw, &schedule.declared);
    // values(): `$VALUES.clone()` cast back to the array type.
    let mut vals = CodeBuilder::new(0);
    let valref = cw.fieldref(&fq, "$VALUES", &arr_desc);
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
    let entref = cw.fieldref(&fq, "$ENTRIES", "Lkotlin/enums/EnumEntries;");
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
            &fq,
            facade,
            Ty::Unit,
            c.enum_entries
                .iter()
                .flat_map(|entry| entry.argument_prelude.iter().chain(&entry.args).copied()),
        );
        let mut clinit = CodeBuilder::new(0);
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
            // A branchy entry arg (`X(1 == 1)`) must run on a clean stack — spill all args to temps
            // first, then construct (mirrors the `New` node's spill).
            let spill = args.iter().any(|&a| e.emits_control_flow(a));
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
        let valref = e.cw.fieldref(&fq, "$VALUES", &arr_desc);
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
        let entref = e.cw.fieldref(&fq, "$ENTRIES", "Lkotlin/enums/EnumEntries;");
        clinit.putstatic(entref, 1);
        // An ENUM initializes its `Companion` FIRST, then the serializer statics that read through
        // it (`$cachedSerializer$delegate`) — the reverse of a plain class's `<clinit>` order, and
        // the same precedence the leading field block uses.
        emit_companion_init(e.cw, &mut clinit, &fq, c);
        // A generated static's store gets its OWN `<clinit>` line entry, the way each entry's
        // construction does, and the trailing `return` maps to the class's closing line. `<clinit>`'s
        // table is CURATED through `set_method_lines` — `add_method` DROPS a `<clinit>` builder's
        // line marks — so pushing entries here is the only thing that reaches the attribute.
        let mut stepped_away = false;
        for s in &owner_statics {
            if s.line != 0 && clinit_lines.last().map(|&(_, l)| l) != Some(s.line) {
                clinit_lines.push((clinit.bytes.len() as u16, s.line));
                stepped_away = true;
            }
            e.emit_static_initializer_store(&fq, s, &mut clinit);
        }
        // The trailing `return` is mapped back only when a store STEPPED AWAY from the entries'
        // line. An enum with no generated statics has a single-entry table, and adding a closing
        // entry there is four bytes kotlinc does not write.
        if stepped_away
            && c.decl_end_line != 0
            && clinit_lines.last().map(|&(_, line)| line) != Some(c.decl_end_line)
        {
            // The frontend records the source declaration's end while syntax is available; the
            // backend consumes that checked source fact instead of guessing from entry lines.
            clinit_lines.push((clinit.bytes.len() as u16, c.decl_end_line));
        }
        clinit.ret_void();
        // `max_locals` is exactly what the body allocated — entry-arg spills enter the frame, and a
        // `<clinit>` that spills nothing has no locals at all (kotlinc writes 0, not a floor of 2).
        clinit.ensure_locals(e.frame.max());
        clinit.link();
        clinit
    };

    // <clinit> is added LAST (built earlier), matching kotlinc's member order.
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);

    // Erased bridges for a generic-interface method overridden at the enum level
    // (`enum E : A<String> { …; override fun foo(t: String) }` → bridge `foo(Object)`→`foo(String)`).
    bridge_emission::emit_bridges(ir, c, &mut cw, env.bridge_return_adaptations, env.run);
    // An enum is a VIEW of the same `IrClass` — compute its `@Metadata` (and hence debug tables /
    // annotations) through the shared path, exactly like `emit_class` and `emit_interface_class`.
    // An `enum class` implementing an interface needs the same holder forwarders an ordinary class
    // does — it reaches emission through this function, not `emit_class`.
    emit_default_impls_forwarders(ir, c, &mut cw, env);
    let class_metadata = opts
        .emit_class_metadata
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if class_metadata.is_some() {
        attach_synth_debug_tables(
            ir,
            c,
            &mut cw,
            opts.param_assertions,
            emits_primary_ctor.then_some((ctor_desc.as_str(), 0)),
            &[],
        );
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(ir, c, &mut cw);
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
    // Deferred fields realize before class annotations: their generic signatures belong to the
    // field-visit interning window even when Kotlin metadata emission is disabled.
    cw.realize_late_fields();
    // A user annotation on an enum interns with the CLASS-ATTRIBUTE window, immediately before
    // `@Metadata` — kotlinc's order. It is independent of metadata emission: disabling Kotlin
    // metadata must not discard the class's declared annotations.
    cw.set_class_annotations(&c.applied_annotations);
    if let Some(m) = class_metadata {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    // Each retained row's outer-class ref and simple name intern at kotlinc's post-metadata window,
    // in the finished table's sorted order — the same seeding every other classifier path does.
    cw.seed_inner_class_names();
    cw.finish()
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
#[allow(clippy::too_many_arguments)]
fn emit_method_maybe_rescued(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
    rescued: bool,
) {
    if rescued {
        // A rescued must-inline impl IS emitted despite its `inline_only` mark (see
        // `emit_all_with_class_meta`) — bypass the early return.
        emit_method_inner(ir, fid, owner, facade, cw, instance, env);
    } else {
        emit_method(ir, fid, owner, facade, cw, instance, env);
    }
}

fn emit_method(
    ir: &IrFile,
    fid: u32,
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
    emit_method_inner(ir, fid, owner, facade, cw, instance, env);
}

/// Whether this common-IR function has no standalone JVM declaration.
///
/// Both sets are explicit lowering/discovery decisions. In particular, `body == None` is not enough:
/// it also represents real abstract declarations, while an inline-splice implementation loses its
/// body only after the body has been consumed into its call site.
fn standalone_method_is_elided(ir: &IrFile, fid: u32, env: &EmitEnv) -> bool {
    ir.inline_only_fns.contains(&fid) || env.run.dead_lambdas.borrow().contains(&fid)
}

/// Under `-jvm-default=disable`, emit an override on `c` for each inherited interface member whose
/// body lives on that interface's `$DefaultImpls` holder.
///
/// kotlinc emits `public <ret> f(args) { return I$DefaultImpls.f(this, args); }`. A member the class
/// declares itself is left alone — it already overrides the abstract interface method.
/// How an implementing class's compatibility forwarder reaches the inherited interface body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ForwarderDispatch {
    /// `invokestatic` on a `$DefaultImpls` holder (the `disable` realization, own module or
    /// dependency).
    HolderStatic,
    /// `invokespecial` on a direct superinterface's default method (the `enable` realization).
    InterfaceSpecial,
}

/// Whether `candidate` transitively derives from the interface `ancestor`, read through the
/// symbol-source classifier model (module and classpath providers both expose direct supertypes).
fn interface_derives_from(
    symbols: &dyn BackendClassifierSource,
    candidate: crate::types::TypeName,
    ancestor: crate::types::TypeName,
) -> bool {
    let mut pending = vec![candidate];
    let mut seen = std::collections::HashSet::new();
    while let Some(owner) = pending.pop() {
        if !seen.insert(owner) {
            continue;
        }
        let Some(shape) = symbols.classifier(owner) else {
            continue;
        };
        for parent in shape.supertypes.iter().copied() {
            if parent == ancestor {
                return true;
            }
            if symbols
                .classifier(parent)
                .is_some_and(|parent| parent.is_interface())
            {
                pending.push(parent);
            }
        }
    }
    false
}

/// The transitive interface closure of the `direct` supertypes, in a TOPOLOGICAL order of the
/// derives-from DAG: every interface precedes all of its ancestors, so a derived redeclaration
/// claims a member key before its ancestor's, and incomparable interfaces keep the declaration
/// order of the `direct` list. (A pairwise comparator was not a total order — incomparable pairs
/// compared `Equal` inconsistently.) Read entirely through the symbol-source classifier model:
/// this pass neither searches IR classes nor retries a missing class against another origin.
fn sorted_interface_closure(
    symbols: &dyn BackendClassifierSource,
    direct: Vec<crate::types::TypeName>,
) -> Vec<(
    crate::types::TypeName,
    std::sync::Arc<crate::backend::BackendClassifierFact>,
)> {
    // DFS post-order emits ancestors before derived; reversing yields the topological order.
    fn visit(
        symbols: &dyn BackendClassifierSource,
        owner: crate::types::TypeName,
        seen: &mut std::collections::HashSet<crate::types::TypeName>,
        out: &mut Vec<(
            crate::types::TypeName,
            std::sync::Arc<crate::backend::BackendClassifierFact>,
        )>,
    ) {
        if !seen.insert(owner) {
            return;
        }
        let Some(shape) = symbols
            .classifier(owner)
            .filter(|shape| shape.is_interface())
        else {
            return;
        };
        for parent in shape.supertypes.iter().copied() {
            visit(symbols, parent, seen, out);
        }
        out.push((owner, shape));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    // The direct list is walked reversed and the post-order reversed again, so incomparable
    // direct supertypes come out in declaration order.
    for owner in direct.into_iter().rev() {
        visit(symbols, owner, &mut seen, &mut out);
    }
    out.reverse();
    out
}

fn backend_member_jvm_name(member: &crate::backend::BackendMemberFact) -> String {
    if let Some(name) = &member.physical_name {
        return name.to_string();
    }
    match &member.name {
        crate::backend::BackendMemberName::Declared(name) => name.to_string(),
        crate::backend::BackendMemberName::PropertyGetter(name) => {
            crate::jvm::names::property_getter_name(name)
        }
        crate::backend::BackendMemberName::PropertySetter(name) => {
            crate::jvm::names::property_setter_name(name)
        }
    }
}

fn emit_default_impls_forwarders(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    use crate::jvm::parameter_names;
    if c.is_interface {
        return;
    }
    let symbols = env.signature_symbols;
    let derives_from = |candidate: crate::types::TypeName, ancestor: crate::types::TypeName| {
        interface_derives_from(symbols, candidate, ancestor)
    };
    let closure = sorted_interface_closure(symbols, c.interfaces.iter_ids().collect());

    let method_key = |name: &str, params: &[Ty]| {
        (
            name.to_string(),
            params
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>(),
        )
    };
    // Include already-derived generic/covariant bridges. They are emitted before these forwarders;
    // ignoring them creates duplicate `(name, descriptor)` methods on a concrete implementer.
    let mut implemented = c
        .methods
        .iter()
        .map(|fid| {
            let function = &ir.functions[*fid as usize];
            method_key(&function.name, &jvm_function_params(ir, *fid))
        })
        .chain(
            c.bridges
                .iter()
                .map(|bridge| method_key(&bridge.name, &bridge.erased_params)),
        )
        .collect::<std::collections::HashSet<_>>();
    // A property override is realized as a FIELD-backed or computed accessor synthesized OUTSIDE
    // `c.methods` (`class Ann : Greeter { override val who = "HR" }` derives `getWho` from the
    // field). Without these keys the pass emits a forwarder DUPLICATING that accessor —
    // `ClassFormatError: Duplicate method name` at class-load time. Keyed by FULL descriptor and
    // recorded only for accessors the class actually EMITS: a `val` has no setter (suppressing an
    // inherited `setX(I)V` forwarder on its name alone left the class abstract), and a same-name
    // accessor with a DIFFERENT return coexists with the forwarder on the JVM — kotlinc emits
    // both `getX()Ljava/lang/String;` (the accessor) and `getX()I` (the forwarder).
    let mut emitted_accessors: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for (name, ty, has_setter, is_private) in c
        .fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.ty,
                !field.is_final(),
                field.is_private(),
            )
        })
        .chain(
            c.properties
                .iter()
                .map(|p| (p.name.as_str(), p.ty, p.is_var, p.visibility.is_private())),
        )
    {
        // A private property has no accessors — in-class reads go straight to the field.
        if is_private {
            continue;
        }
        let (getter, setter) = accessor_jvm_names(c, name);
        let jt = jvm_declared_ty(&ty);
        emitted_accessors.insert((getter, method_descriptor(&[], jt)));
        if has_setter {
            emitted_accessors.insert((setter, method_descriptor(&[jt], Ty::Unit)));
        }
    }
    // An `invokespecial` interface forwarder must NAME a direct superinterface of this class (the
    // JVM's rule for interface `super` calls); the body then resolves to the maximally-specific
    // default. Pick the first DECLARED superinterface through which the winning declaration is
    // inherited — measured on kotlinc: `class C : D, B` with the override on `B` names `B`, and
    // `class C : B` with the body up on `A` names `B`, not `A`.
    let interface_special_target = |declaring: crate::types::TypeName| {
        c.interfaces
            .iter_ids()
            .find(|&direct| direct == declaring || derives_from(direct, declaring))
    };
    let mut selected = std::collections::HashSet::new();
    let mut write_forwarder = |interface: crate::types::TypeName,
                               name: &str,
                               param_tys: &[Ty],
                               semantic_params: &[Ty],
                               parameter_identities: &[crate::fir::ResolvedParameterIdentity],
                               ret: Ty,
                               semantic_ret: Ty,
                               target_owner: &str,
                               target_name: &str,
                               target_descriptor: &str,
                               dispatch: ForwarderDispatch| {
        assert_eq!(
            parameter_identities.len(),
            param_tys.len(),
            "an inherited forwarder needs every declaration parameter identity"
        );
        let desc = method_descriptor(param_tys, ret);
        let mut code = CodeBuilder::new(1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>());
        code.aload(0);
        let mut slot = 1u16;
        for ty in param_tys {
            match *ty {
                Ty::Long => code.lload(slot),
                Ty::Double => code.dload(slot),
                Ty::Float => code.fload(slot),
                t if t.is_reference() => code.aload(slot),
                _ => code.iload(slot),
            }
            slot += slot_words(*ty);
        }
        let argument_words = 1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>();
        match dispatch {
            ForwarderDispatch::HolderStatic => {
                let target = cw.methodref(target_owner, target_name, target_descriptor);
                code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
            }
            // The interface publishes the body as a DEFAULT method: the forwarder is a Java-style
            // interface `super` call. The JVM requires the named interface to be a DIRECT
            // superinterface of this class — the caller selects it.
            ForwarderDispatch::InterfaceSpecial => {
                let target = cw.interface_methodref(target_owner, target_name, target_descriptor);
                code.invokespecial(target, argument_words as i32, slot_words(ret) as i32);
            }
        }
        match ret {
            Ty::Unit => code.ret_void(),
            Ty::Long => code.lreturn(),
            Ty::Double => code.dreturn(),
            Ty::Float => code.freturn(),
            t if t.is_reference() => code.areturn(),
            _ => code.ireturn(),
        }
        finish_code::<0x0041>(cw, name, &desc, &mut code, argument_words);
        let mut locals = vec![("this".to_string(), format!("L{};", c.fq_name()), 0)];
        let mut slot = 1u16;
        let parameter_names =
            parameter_names::resolved_local_variables(parameter_identities, semantic_params, name);
        for (parameter, parameter_name) in param_tys.iter().zip(parameter_names) {
            if let Some(parameter_name) = parameter_name {
                locals.push((parameter_name, local_variable_desc(*parameter), slot));
            }
            slot += slot_words(*parameter);
        }
        cw.set_method_debug(
            name,
            &desc,
            (c.decl_line != 0).then_some((0, c.decl_line)),
            &locals,
        );
        let ann = |ty: Ty| {
            if matches!(ty.non_null(), Ty::TyParam(..)) || !ir_ty_to_jvm(&ty).is_reference() {
                None
            } else if ty.is_nullable() {
                Some("Lorg/jetbrains/annotations/Nullable;")
            } else {
                Some("Lorg/jetbrains/annotations/NotNull;")
            }
        };
        let parameter_annotations = semantic_params.iter().copied().map(ann).collect::<Vec<_>>();
        cw.set_method_nullability(name, &desc, ann(semantic_ret), &parameter_annotations);
        // Only a holder call makes the class REFERENCE the nested holder; an `invokespecial`
        // forwarder names the interface alone, and kotlinc records no `InnerClasses` entry for it.
        if dispatch == ForwarderDispatch::HolderStatic {
            cw.add_inner_class(crate::jvm::classfile::InnerClassSpec {
                inner: target_owner.to_string(),
                outer: Some(interface.render()),
                name: Some("DefaultImpls".to_string()),
                access: 0x0019,
            });
        }
    };
    for (interface, shape) in closure {
        for member in &shape.surface {
            let physical_params = if member.physical_params.len() == member.params.len() {
                member.physical_params.clone()
            } else {
                member.params.clone()
            };
            let mut param_tys = jvm_tys(&physical_params);
            let mut ret = jvm_declared_ty(&member.physical_ret);
            let mut semantic_params = member.params.to_vec();
            let mut parameter_identities = member.parameter_identities.to_vec();
            let mut semantic_ret = member.ret;
            assert_eq!(
                parameter_identities.len(),
                semantic_params.len(),
                "an inherited member needs exact metadata parameter identities"
            );
            // A `suspend` member's PHYSICAL realization is its CPS shape — a trailing
            // `Continuation` and an `Object` return. The semantic record keeps the declared
            // params/return, so adjust here or the forwarder declares a method the interface does
            // not have (and misses the class's own CPS-shaped override in `implemented`). kotlinc
            // names the parameter `$completion`, annotates it `@NotNull`, and the return
            // `@Nullable`.
            if member.suspend() {
                param_tys.push(Ty::obj("kotlin/coroutines/Continuation"));
                ret = Ty::obj("java/lang/Object");
                parameter_identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
                semantic_params.push(Ty::obj("kotlin/coroutines/Continuation"));
                semantic_ret = Ty::nullable(Ty::obj("java/lang/Object"));
            }
            let name = backend_member_jvm_name(member);
            let key = method_key(&name, &param_tys);
            // The nearest declaration wins even when it is abstract: an abstract redeclaration
            // suppresses a farther ancestor's body rather than exposing it as a fake override.
            if !selected.insert(key.clone()) {
                continue;
            }
            if member.is_abstract()
                || member.visibility == crate::types::Visibility::Private
                || implemented.contains(&key)
                || emitted_accessors.contains(&(name.clone(), method_descriptor(&param_tys, ret)))
            {
                continue;
            }
            let (target_owner, target_name, target_desc, dispatch) = match member.realization {
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true,
                } => {
                    let Some(owner) = member.owner else { continue };
                    (
                        owner.render(),
                        name.clone(),
                        member.descriptor.to_string(),
                        ForwarderDispatch::HolderStatic,
                    )
                }
                crate::libraries::MemberRealization::Dispatch
                    if env.jvm_default == JvmDefaultMode::Disable && shape.source =>
                {
                    let mut with_receiver = vec![Ty::obj_name(interface)];
                    with_receiver.extend_from_slice(&param_tys);
                    (
                        crate::types::type_name_nested_child(interface, "DefaultImpls").render(),
                        name.clone(),
                        method_descriptor(&with_receiver, ret),
                        ForwarderDispatch::HolderStatic,
                    )
                }
                // A KOTLIN interface member whose body is a JVM default method on the interface
                // (this module under `enable`, or a dependency compiled under `enable`/
                // `no-compatibility`): kotlinc forwards with a Java-style interface `super` call.
                // A JAVA default method never gets a forwarder, and `no-compatibility` emits none.
                crate::libraries::MemberRealization::Dispatch
                    if env.jvm_default != JvmDefaultMode::NoCompatibility && shape.is_kotlin =>
                {
                    let Some(named) = interface_special_target(interface) else {
                        continue;
                    };
                    (
                        named.render(),
                        name.clone(),
                        method_descriptor(&param_tys, ret),
                        ForwarderDispatch::InterfaceSpecial,
                    )
                }
                _ => continue,
            };
            write_forwarder(
                interface,
                &name,
                &param_tys,
                &semantic_params,
                &parameter_identities,
                ret,
                semantic_ret,
                &target_owner,
                &target_name,
                &target_desc,
                dispatch,
            );
            implemented.insert(key);
        }
    }
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
    emit_method_inner_with_holder(ir, fid, owner, facade, cw, true, env, Some(receiver));
}

/// `@NotNull`/`@Nullable` for one semantic type in the `-jvm-default` compatibility surface, the
/// same selection the implementing-class forwarders use: reference types only, and never a bare
/// type variable (kotlinc leaves `T`-typed positions unannotated).
fn jd_nullability_annotation(ty: &Ty) -> Option<&'static str> {
    if matches!(ty.non_null(), Ty::TyParam(..)) || !ir_ty_to_jvm(ty).is_reference() {
        None
    } else if ty.is_nullable() {
        Some("Lorg/jetbrains/annotations/Nullable;")
    } else {
        Some("Lorg/jetbrains/annotations/NotNull;")
    }
}

/// A declared member's parameter types with the side-table declared nullability applied, so the
/// compatibility surface annotates `x: T?` parameters `@Nullable` exactly as the abstract
/// declaration path does.
fn jd_declared_param_tys(ir: &IrFile, fid: u32) -> Vec<Ty> {
    let f = &ir.functions[fid as usize];
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    f.params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if declared_nullable
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
            {
                Ty::nullable(*t)
            } else {
                *t
            }
        })
        .collect()
}

/// The `access$<name>$jd` bridge kotlinc puts on an `enable`-mode interface for each of its
/// non-private default methods: a `public static synthetic` whose body makes the NON-VIRTUAL call
/// (`invokespecial` on the interface's own method) that the `$DefaultImpls` forward and legacy
/// `super`-callers need. Its `LineNumberTable` is one entry at the invoke instruction, on the
/// interface's declaration line — measured, not inferred.
fn emit_jd_access_bridge(
    cw: &mut ClassWriter,
    interface: crate::types::TypeName,
    decl_line: u32,
    member_name: &str,
    param_tys: &[Ty],
    parameter_names: &[Option<String>],
    ret: Ty,
) {
    assert_eq!(
        parameter_names.len(),
        param_tys.len(),
        "an access bridge needs every declaration parameter identity"
    );
    let fq = interface.render();
    let member_desc = method_descriptor(param_tys, ret);
    let mut with_receiver = vec![Ty::obj_name(interface)];
    with_receiver.extend_from_slice(param_tys);
    let bridge_desc = method_descriptor(&with_receiver, ret);
    let name = format!("access${member_name}$jd");
    cw.reserve_method_name(&name);
    cw.reserve_descriptor(&bridge_desc);
    let argument_words = 1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    code.aload(0);
    let mut slot = 1u16;
    for ty in param_tys {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    if decl_line != 0 {
        code.mark_line(decl_line);
    }
    let target = cw.interface_methodref(&fq, member_name, &member_desc);
    code.invokespecial(target, argument_words as i32, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    finish_code::<0x1009>(cw, &name, &bridge_desc, &mut code, argument_words); // PUBLIC | STATIC | SYNTHETIC
    let mut locals = vec![("$this".to_string(), format!("L{fq};"), 0)];
    let mut slot = 1u16;
    for (index, parameter) in param_tys.iter().enumerate() {
        if let Some(parameter_name) = &parameter_names[index] {
            locals.push((
                parameter_name.clone(),
                local_variable_desc(*parameter),
                slot,
            ));
        }
        slot += slot_words(*parameter);
    }
    cw.set_method_debug(&name, &bridge_desc, None, &locals);
}

/// Where an `enable`-mode holder forward sends its call.
enum JdHolderTarget<'a> {
    /// The interface's own `access$<name>$jd` bridge (the member is a default method here).
    AccessBridge,
    /// A `disable`-compiled dependency's `$DefaultImpls` static — the inherited member has no
    /// default method anywhere to bridge to.
    DependencyHolder {
        declaring: crate::types::TypeName,
        holder: crate::types::TypeName,
        descriptor: &'a str,
    },
}

/// The holder's `enable`-mode `$default` copy: a thin synthetic forward reloading every stub
/// argument (receiver, parameters, masks, marker) into one `invokestatic` on the interface's own
/// `$default` stub — kotlinc's shape: no locals table, a one-entry `LineNumberTable` at the
/// member's declaration line.
fn emit_default_stub_forward(ir: &IrFile, fid: u32, owner: &str, cw: &mut ClassWriter) {
    let f = &ir.functions[fid as usize];
    let ret = jvm_declared_ty(&f.ret);
    let stub_params = default_stub_params(ir, fid, Ty::obj(owner));
    let desc = method_descriptor(&stub_params, ret);
    let name = format!("{}$default", f.name);
    cw.reserve_method_name(&name);
    cw.reserve_descriptor(&desc);
    let argument_words = stub_params.iter().map(|t| slot_words(*t)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let mut slot = 0u16;
    for ty in &stub_params {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    let target = cw.interface_methodref(owner, &name, &desc);
    code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    code.ensure_locals(argument_words);
    code.link();
    cw.add_method(default_stub_access(ir, fid), &name, &desc, &code);
    if let Some(&line) = ir.fn_decl_lines.get(&fid) {
        cw.set_method_lines(&name, &desc, &[(0, line)]);
    }
}

/// A moved interface body keeps the source method's generic signature, but `$DefaultImpls` makes
/// the interface receiver its first static parameter and promotes the interface's type parameters
/// to method type parameters. Transform the already-computed method signature so suspend and other
/// specialized signatures retain their exact tail rather than being reconstructed by ABI shape.
fn holder_method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    receiver: crate::types::TypeName,
    method_signature: Option<&str>,
    descriptor: &str,
) -> Option<String> {
    let class_signature = ir.class_signature_name(receiver);
    let class_type_params = class_signature
        .map(|signature| signature.type_params.as_slice())
        .unwrap_or_default();
    if class_type_params.is_empty() && method_signature.is_none() {
        return None;
    }

    fn leading_type_parameters(signature: &str) -> (&str, &str) {
        if !signature.starts_with('<') {
            return ("", signature);
        }
        let mut depth = 0usize;
        for (index, byte) in signature.bytes().enumerate() {
            match byte {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        return (&signature[1..index], &signature[index + 1..]);
                    }
                }
                _ => {}
            }
        }
        ("", signature)
    }

    let class_declaration = class_signature
        .and_then(|signature| jvm_type_params(formatter, signature))
        .unwrap_or_default();
    let class_declaration = class_declaration
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or_default();
    let base = method_signature.unwrap_or(descriptor);
    let (method_declaration, method_tail) = leading_type_parameters(base);
    let declaration = if class_declaration.is_empty() && method_declaration.is_empty() {
        String::new()
    } else {
        format!("<{class_declaration}{method_declaration}>")
    };

    let receiver_ty = if class_type_params.is_empty() {
        Ty::obj_name(receiver)
    } else {
        let arguments = class_type_params
            .iter()
            .map(|parameter| {
                let bound = parameter
                    .bounds
                    .first()
                    .map(|(bound, _)| *bound)
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::ty_param(&parameter.name, bound)
            })
            .collect::<Vec<_>>();
        Ty::obj_args_name(receiver, &arguments)
    };
    let receiver_signature = formatter.method_ty(&receiver_ty, Wildcards::Declared)?;
    let parameters = method_tail.strip_prefix('(')?;
    Some(format!("{declaration}({receiver_signature}{parameters}"))
}

fn emit_method_inner(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
) {
    emit_method_inner_with_holder(ir, fid, owner, facade, cw, instance, env, None);
}

/// `holder_receiver` is `Some(interface)` when the body is being written onto that interface's
/// `$DefaultImpls` holder: the code and slots are the instance method's, but the method is `static`
/// and its descriptor carries the receiver as parameter 0.
#[allow(clippy::too_many_arguments)]
fn emit_method_inner_with_holder(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
    holder_receiver: Option<crate::types::TypeName>,
) {
    let f = &ir.functions[fid as usize];
    let body = f.body.unwrap();
    let param_tys = jvm_function_params(ir, fid);
    let ret = jvm_declared_ty(&f.ret);
    let mut e = Emitter::new(ir, cw, env, owner, facade, ret, [body]);
    // kotlinc's transformer keeps a suspend body's own local names: they name the spills.
    let transformed = env.emit_time_machines.transformed(fid);
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
    for (i, t) in param_tys.iter().enumerate() {
        let vi = i as u32 + if instance { 1 } else { 0 };
        let slot = e.frame.enter(FrameKey::Value(vi), *t);
        e.slots.insert(vi, (slot, *t));
    }
    let transformed = transformed.map(|machine| {
        let completion = param_tys
            .len()
            .checked_sub(1)
            .and_then(|index| u32::try_from(index).ok())
            .expect("a transformed suspend function has a physical completion parameter")
            + u32::from(instance);
        let slot = e.arm_transformed_machine(machine, completion);
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
            .map(|suspension| suspension.call)
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
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    // The cache's compiler-invented factories and accessor record no generic `Signature`: the
    // attribute exists for a source or Java caller, and nothing can name these methods to call
    // them. Keep this scoped to the producer's exact identities; unrelated synthetic methods may
    // still have a source-visible generic contract.
    let method_sig = (!ir.serialization_cache_methods.contains(&fid))
        .then(|| method_signature(&signature_formatter, ir, fid, f))
        .flatten();
    let reserved_sig = match holder_receiver {
        Some(receiver) => holder_method_signature(
            &signature_formatter,
            ir,
            receiver,
            method_sig.as_deref(),
            &method_descriptor(&param_tys, ret),
        ),
        None => match ir.jvm_suspend_interface_bodies.get(&fid).copied() {
            Some((receiver, _)) => holder_method_signature(
                &signature_formatter,
                ir,
                receiver,
                method_sig.as_deref(),
                &method_descriptor(f.params.get(1..).unwrap_or_default(), ret),
            ),
            None => method_sig,
        },
    };
    let lambda_impl = ir.lambda_own_params_from.contains_key(&fid);
    let declared_nullability::DeclaredNullability {
        result: ret_ann,
        parameters: param_anns,
    } = declared_nullability::declared_nullability(ir, fid);
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
    // kotlinc annotates nullability only on declarations a source caller can reach. A
    // HIDDEN-deprecated one is emitted ACC_SYNTHETIC for binary compatibility alone and carries
    // neither `@NotNull` nor `@Nullable`; a PRIVATE method (declared, or a data class's `copy`
    // under `DataClassCopyRespectsConstructorVisibility`) likewise gets none — the annotations
    // exist for Java interop, which cannot see it.
    // kotlinc annotates DECLARED methods. A compiler-invented accessor (`access$…$cp`) gets no
    // nullability annotation, the same way it gets no generic `Signature`.
    let nullability_annotated = !declared_annotations.deprecated_hidden()
        && !ir.private_methods.contains(&fid)
        && !ir.synthetic_methods.contains(&fid)
        && !ir.jvm_nullability_unannotated_methods.contains(&fid);
    // The USER annotations on this function's parameters. kotlinc's writer visits the method's own
    // annotations, then the whole `RuntimeVisibleParameterAnnotations` attribute, then
    // `RuntimeInvisible…`, interning each type as it writes it. `reserve_method_pool_with_annotations`
    // interns the declaration's own annotations first, so the order here is: the return annotation,
    // every parameter's RUNTIME-retained type, then per parameter its BINARY-retained types followed
    // by that parameter's synthesized nullability type.
    let user_param_anns: &[crate::ir::DeclarationAnnotations] = ir
        .fn_param_annotations
        .get(&fid)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let split: Vec<(
        Vec<crate::ir::AppliedAnnotation>,
        Vec<crate::ir::AppliedAnnotation>,
    )> = user_param_anns
        .iter()
        .map(crate::jvm::classfile::split_declaration_annotations)
        .collect();
    let descriptor_of = |a: &crate::ir::AppliedAnnotation| format!("L{};", a.internal);
    let visible_ann_types: Vec<String> = split
        .iter()
        .flat_map(|(visible, _)| visible.iter().map(descriptor_of))
        .collect();
    let invisible_ann_types: Vec<Vec<String>> = split
        .iter()
        .map(|(_, invisible)| invisible.iter().map(descriptor_of).collect())
        .collect();
    let mut ann_types: Vec<&str> = ret_ann
        .into_iter()
        .filter(|_| nullability_annotated)
        .collect();
    ann_types.extend(visible_ann_types.iter().map(String::as_str));
    for (i, nullability) in emitted_param_anns.iter().enumerate() {
        if let Some(types) = invisible_ann_types.get(i) {
            ann_types.extend(types.iter().map(String::as_str));
        }
        if nullability_annotated {
            ann_types.extend(nullability.iter().copied());
        }
    }
    let method_parameters = if env.java_parameters {
        super::method_parameters::function(ir, fid, &param_tys, holder_receiver)
    } else {
        Vec::new()
    };
    e.cw.reserve_method_pool_with_annotations(
        &f.name,
        &reserved_desc,
        reserved_sig.as_deref(),
        &ann_types,
        &declared_annotations,
        &method_parameters,
    );
    let mut code = CodeBuilder::new(e.frame.size());
    // kotlinc guards each non-null reference parameter of a visible function with
    // `Intrinsics.checkNotNullParameter(param, "name")` at method entry — emit the same.
    let param_checks = f.param_checks.clone();
    let parameter_identities = ir.function_parameter_identities(fid);
    let assertion_names = crate::jvm::parameter_names::function_assertions(ir, fid, &param_tys);
    for (i, check) in param_checks.iter().enumerate() {
        if check.is_some() {
            let _identity = parameter_identities
                .and_then(|identities| identities.get(i))
                .expect("a checked parameter carries an exact identity");
            let name = assertion_names
                .as_ref()
                .and_then(|names| names.get(i))
                .and_then(Clone::clone)
                .expect("a checked parameter carries an assertion spelling");
            let vi = i as u32 + if instance { 1 } else { 0 };
            if let Some(&(slot, _)) = e.slots.get(&vi) {
                e.checked_parameters.insert(vi);
                code.aload(slot);
                code.push_string(&name, e.cw);
                let m = e.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
            }
        }
    }
    // A LAMBDA IMPL's LineNumberTable starts at the post-guard pc, mapped to the body's line —
    // kotlinc's shape even for an empty body (whose emission marks no line of its own).
    if lambda_impl {
        if let Some(&line) = ir.fn_decl_lines.get(&fid) {
            code.mark_line(line);
        }
    }
    // kotlinc opens every EMITTED `inline fun` body with an inline-depth marker: `iconst_0;
    // istore_<n>` into a synthetic `$i$f$<name>` int local covering the body — its inliner tracks
    // splice depth through these. Emitted after the parameter guards; the body's LineNumberTable
    // then naturally starts at the post-store pc.
    let inline_marker: Option<(u16, u16)> =
        (!instance && ir.top_level_inline_functions.contains(&fid)).then(|| {
            let slot = e
                .frame
                .enter_temp(TempRole::InlineDepthMarker, Ty::Int)
                .slot();
            code.push_int(0, e.cw);
            store(Ty::Int, slot, &mut code);
            (slot, code.bytes.len() as u16)
        });
    // An emitted inline TEMPLATE opens its body on the function's own body line, which kotlinc
    // records even when the body's first instruction belongs to a later line: a template's body is
    // what a call site expands, so both positions are kept. `mark_line_retained` is what lets the
    // body's own first mark join this one at the same offset instead of replacing it; where the two
    // agree — the ordinary case — it deduplicates and nothing is added.
    if inline_marker.is_some() {
        if let Some(&line) = ir.fn_decl_lines.get(&fid) {
            if line != 0 {
                code.mark_line_retained(line);
            }
        }
    }
    // Building the machine: everything the discovery pass learned is in hand, so the entry,
    // the dispatch and the state it re-enters can be emitted around the same body.
    let machine_states = match (machine_slots, env.run.machine_plan(fid)) {
        (Some(slots), Some(plan)) => {
            // The continuation the machine is re-entered on is the trailing `$completion`
            // parameter. Resolve it BEFORE anything is armed: arming is what makes the body emit
            // markers and spills, and a machine armed for a prologue that then declined would leave
            // both behind — markers no erasure pass is reached for, over a slot never assigned.
            let completion = param_tys.len().saturating_sub(1) as u32 + u32::from(instance);
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
                            .private_methods
                            .contains(&fid)
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
    // kotlinc writes it even when no other local is recorded. LVT strings intern EAGERLY, right
    // after this method's body (kotlinc's per-method visit order) — deferring them to the write
    // phase batches every method's table entries at the end, a pool-order divergence on any
    // multi-method facade.
    if let Some((slot, start)) = inline_marker {
        let marker_name = format!("$i$f${}", f.name);
        e.cw.seed_utf8(&marker_name);
        e.cw.seed_utf8("I");
        code.add_local_entry(start, None, slot, &marker_name, "I");
    }
    // Method locals precede `this` and parameters in kotlinc's table order.
    if e.record_locals {
        for (_, slot, start, name, desc) in std::mem::take(&mut e.open_locals) {
            e.cw.seed_utf8(&name);
            e.cw.seed_utf8(&desc);
            code.add_local_entry(start, None, slot, &name, &desc);
        }
    }
    // Suspend rewriting invalidates source-local expression ids, but its physical parameters remain
    // stable and reflection/debug tooling still expects `$completion` (plus the declared receiver and
    // arguments) in the LocalVariableTable.
    if e.record_locals || ir.suspend_funs.contains(&fid) || holder_receiver.is_some() {
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
            let receiver_name = if holder_receiver.is_some() {
                "$this"
            } else {
                "this"
            };
            e.cw.seed_utf8(receiver_name);
            e.cw.seed_utf8(&this_desc);
            code.add_local_entry(0, None, 0, receiver_name, &this_desc);
        }
        let mut slot = u16::from(instance);
        let local_names = parameter_identities
            .and_then(|_| crate::jvm::parameter_names::function_locals(ir, fid, &param_tys));
        for (i, t) in param_tys.iter().enumerate() {
            let pname = local_names
                .as_ref()
                .and_then(|names| names.get(i))
                .and_then(Clone::clone);
            if let Some(pname) = pname {
                let pdesc = local_variable_desc(*t);
                e.cw.seed_utf8(&pname);
                e.cw.seed_utf8(&pdesc);
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
    // Top-level/`static` functions are always `final` (kotlinc emits `public static final`). An
    // instance method of a *final* class (nothing extends it) is also `final` and can never be
    // overridden, so marking it is safe; in an open/extended class we conservatively leave it
    // non-`final` (a method-level `open`/`override` model would refine this).
    let access = if holder_receiver.is_some() {
        // STATIC, with the member's own visibility: a private interface member's body is a PRIVATE
        // static on the holder, as kotlinc emits it.
        if ir.private_methods.contains(&fid) {
            0x000a // PRIVATE | STATIC
        } else {
            0x0009 // PUBLIC | STATIC
        }
    } else if instance {
        // kotlinc keeps an `Object`-override (a data class's toString/hashCode/equals) open even in a
        // final class, so honor `open_methods`; otherwise a method of a final class is itself final.
        let final_class = !ir.classes.iter().any(|o| o.superclass_matches(owner));
        // An interface default method must NOT be `final` (the JVM rejects a final interface method).
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name_matches(owner) && o.is_interface);
        let fin = final_class && !ir.open_methods.contains(&fid) && !owner_is_iface;
        // A `private set` setter is `private final` (kotlinc); else `public` (+`final` per above).
        let vis = if ir.private_methods.contains(&fid) {
            0x0002
        } else {
            0x0001
        };
        // A private method is `final` on a CLASS, but a private INTERFACE method must NOT carry `ACC_FINAL`
        // (`ClassFormatError: illegal modifiers 0x12`) — private already makes it non-virtual.
        vis | if fin || (ir.private_methods.contains(&fid) && !owner_is_iface) {
            0x0010
        } else {
            0
        }
    } else {
        // A `static` method is `<vis> static final` (kotlinc) — EXCEPT on an interface, where a `final`
        // static method is illegal (`ClassFormatError`), or a value class's `constructor-impl`/
        // `<name>-impl` delegate members, which kotlinc emits `public static` (non-`final`) and marks via
        // `open_methods`. `box-impl`/`equals-impl0` stay `public static final` (not opened). Visibility
        // derives from the member's own (a private declaration — or a lambda impl, which kotlinc always
        // emits private — is `ACC_PRIVATE`).
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name_matches(owner) && o.is_interface);
        let vis = if ir.private_methods.contains(&fid) {
            // Under `-Xlambdas=class` the body is called from the lambda's OWN class, so a private
            // impl would be an `IllegalAccessError` at the delegating `invoke`. kotlinc has no such
            // method to place — it moves the body into `invoke` — so package-private here is the
            // narrowest visibility that keeps the delegation working.
            // A static interface method must carry exactly one of ACC_PUBLIC / ACC_PRIVATE
            // (JVMS 4.6), so an interface's impl opens all the way to public instead.
            let called_from_lambda_class =
                lambda_impl_uses_class_strategy(ir, fid, env.lambda_modes);
            match (called_from_lambda_class, owner_is_iface) {
                (true, true) => 0x0001,
                (true, false) => 0x0000,
                (false, _) => 0x0002,
            }
        } else {
            0x0001
        };
        if owner_is_iface || ir.open_methods.contains(&fid) {
            vis | 0x0008 // <vis> | STATIC
        } else {
            vis | 0x0018 // <vis> | STATIC | FINAL
        }
    };
    // A value class's `box-impl`/`unbox-impl` are compiler-manufactured box adapters — kotlinc marks them
    // `ACC_SYNTHETIC`.
    let access = access
        | if ir.synthetic_methods.contains(&fid) {
            0x1000
        } else {
            0
        }
        | if ir.bridge_methods.contains(&fid) {
            0x0040 // ACC_BRIDGE
        } else {
            0
        };
    // A method with own type parameters (`fun <T> …`) → the tparam-based signature; otherwise a method
    // whose concrete param/return type is PARAMETERIZED (`getXs(): List<String>`, `copy(List<String>)`)
    // → its generic signature. `f.params`/`f.ret` are the SOURCE types (retain `<…>` args); `param_tys`/
    // `ret` are erased.
    let desc = reserved_desc;
    e.cw.add_method_sig(access, &f.name, &desc, &code, reserved_sig.as_deref());
    if let Some((machine, slot)) = transformed {
        transformed_suspensions::request_transform(ir, e.cw, fid, (&f.name, &desc), machine, slot);
    }
    e.cw.set_method_parameters(&f.name, &desc, &method_parameters);
    // kotlinc annotates a reference return and each reference parameter of a declared method.
    if nullability_annotated
        && (ret_ann.is_some() || emitted_param_anns.iter().any(Option::is_some))
    {
        e.cw.set_method_nullability(&f.name, &desc, ret_ann, &emitted_param_anns);
    }
    // The USER parameter annotations (their types are already interned, above).
    if !user_param_anns.is_empty() {
        e.cw.set_method_param_annotations(&f.name, &desc, user_param_anns);
    }
    if ir.deprecated_methods.contains(&fid) {
        e.cw.mark_method_deprecated(&f.name, &desc);
    }
    // User annotations declared on the function. A HIDDEN-deprecated declaration additionally gets
    // `ACC_SYNTHETIC`: kotlinc keeps it only for binary compatibility, and a consumer reads both
    // facts (the annotation for resolution, the flag for the JVM) off this realization.
    if let Some(annotations) = ir.function_annotations.get(&fid) {
        e.cw.set_method_annotations(&f.name, &desc, annotations);
        if annotations.deprecated() {
            e.cw.mark_method_deprecated(&f.name, &desc);
        }
        if annotations.deprecated_hidden() {
            e.cw.set_method_synthetic(&f.name, &desc);
        }
    }
}

/// Whether declaration-site variance becomes a JVM wildcard at the position being formatted.
///
/// kotlinc writes those wildcards in PARAMETER positions only: a return type and a field type get the
/// invariant spelling, at EVERY nesting depth (`fun <U> deep(a: Map<String, List<U>>): Map<String,
/// List<U>>` signs its parameter `Ljava/util/Map<Ljava/lang/String;+Ljava/util/List<+TU;>;>;` and its
/// return `Ljava/util/Map<Ljava/lang/String;Ljava/util/List<TU;>;>;`). An explicit `in`/`out`
/// projection the user wrote is not declaration-site variance and renders in either mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wildcards {
    /// A parameter position: realize declaration-site variance as `+`/`-`.
    Declared,
    /// A return or field position: spell every argument invariantly.
    Suppressed,
}

/// Format backend-agnostic semantic types into JVM generic-signature elements. The ordinary JVM
/// descriptor and the optional generic `Signature` attribute are separate classfile declarations: the
/// former supplies runtime calling types, while this formatter preserves type parameters, type
/// arguments, and declaration-site variance for classpath readers.
struct JvmSignatureFormatter<'a> {
    ir: &'a IrFile,
    symbols: &'a dyn BackendClassifierSource,
    run: &'a EmitRun,
}

impl<'a> JvmSignatureFormatter<'a> {
    fn new(ir: &'a IrFile, env: &'a EmitEnv<'_>) -> Self {
        Self {
            ir,
            symbols: env.signature_symbols,
            run: env.run,
        }
    }

    fn current_class(&self, classifier: TypeName) -> Option<&crate::ir::IrClass> {
        self.ir
            .classes
            .iter()
            .find(|candidate| candidate.fq_name == classifier)
    }

    fn classifier_signature_chain(
        &self,
        owner: TypeName,
    ) -> Option<Vec<(TypeName, Vec<TypeVariance>)>> {
        let mut chain = Vec::new();
        let mut current = owner;
        loop {
            let (variances, outer) = if let Some(classifier) = self.current_class(current) {
                let variances = if classifier.type_params.is_empty() {
                    Vec::new()
                } else {
                    let Some(signature) = self.ir.class_signature_name(current) else {
                        self.run.set_emit_error(format!(
                            "internal: current IR classifier '{}' has type arguments but no checked generic signature",
                            current.render()
                        ));
                        return None;
                    };
                    signature
                        .type_params
                        .iter()
                        .map(|parameter| parameter.variance)
                        .collect()
                };
                let outer = if classifier.is_inner_class {
                    let Some(outer) = current.nested_owner() else {
                        self.run.set_emit_error(format!(
                            "internal: inner classifier '{}' has no semantic enclosing classifier",
                            current.render()
                        ));
                        return None;
                    };
                    Some(outer)
                } else {
                    None
                };
                (variances, outer)
            } else {
                let Some(classifier) = self.symbols.classifier(current) else {
                    self.run.set_emit_error(format!(
                        "internal: JVM signature references classifier '{}' absent from checked symbols",
                        current.render()
                    ));
                    return None;
                };
                let own_count = classifier.own_type_parameter_count;
                if own_count > classifier.type_param_variances.len() {
                    self.run.set_emit_error(format!(
                        "internal: classifier '{}' publishes {} own type parameters but only {} semantic variances",
                        current.render(),
                        own_count,
                        classifier.type_param_variances.len()
                    ));
                    return None;
                }
                (
                    classifier.type_param_variances[..own_count].to_vec(),
                    classifier.outer_instance,
                )
            };
            chain.push((current, variances));
            let Some(outer) = outer else { break };
            current = outer;
        }
        chain.reverse();
        Some(chain)
    }

    /// JVM generic applications contain parameters declared by the classifier and its non-static
    /// inner-class chain. A body-local classifier's semantic application additionally carries type
    /// parameters captured from its enclosing function so the frontend can check its members and
    /// supertypes. Those captures are not JVM class parameters (kotlinc uses the local/anonymous
    /// class raw at use sites), so remove exactly the explicitly published captured suffix.
    fn classifier_usage_arguments<'b>(
        &self,
        owner: TypeName,
        arguments: &'b [Ty],
        declared: usize,
    ) -> Option<&'b [Ty]> {
        if arguments.len() == declared {
            return Some(arguments);
        }
        let captured = if let Some(classifier) = self.current_class(owner) {
            classifier.captured_type_params.len()
        } else {
            let classifier = self.symbols.classifier(owner)?;
            classifier
                .type_param_variances
                .len()
                .checked_sub(classifier.own_type_parameter_count)
                .unwrap_or_else(|| {
                    self.run.set_emit_error(format!(
                        "internal: classifier '{}' has more own type parameters than semantic parameters",
                        owner.render()
                    ));
                    0
                })
        };
        if captured != 0 && arguments.len() == declared + captured {
            return Some(&arguments[..declared]);
        }
        self.run.set_emit_error(format!(
            "internal: JVM signature supplies {} type arguments to classifier '{}' with {} type parameters across its inner-class chain",
            arguments.len(),
            owner.render(),
            declared
        ));
        None
    }

    fn declaration_variance(&self, owner: TypeName, index: usize) -> Option<TypeVariance> {
        let chain = self.classifier_signature_chain(owner)?;
        let total: usize = chain.iter().map(|(_, variances)| variances.len()).sum();
        let Some(variance) = chain
            .iter()
            .flat_map(|(_, variances)| variances.iter().copied())
            .nth(index)
        else {
            self.run.set_emit_error(format!(
                "internal: JVM signature supplies type argument {} to classifier '{}' with {} type parameters across its inner-class chain",
                index + 1,
                owner.render(),
                total
            ));
            return None;
        };
        Some(variance)
    }

    fn classifier_is_closed(&self, owner: TypeName) -> Option<bool> {
        if let Some(classifier) = self.current_class(owner) {
            return Some(
                classifier.is_object
                    || classifier.is_annotation
                    || (!classifier.is_interface && !classifier.is_abstract && !classifier.is_open),
            );
        }
        let Some(classifier) = self.symbols.classifier(owner) else {
            self.run.set_emit_error(format!(
                "internal: JVM wildcard optimization references classifier '{}' absent from checked symbols and current IR",
                owner.render()
            ));
            return None;
        };
        Some(match classifier.kind {
            crate::libraries::TypeKind::Object | crate::libraries::TypeKind::Annotation => true,
            crate::libraries::TypeKind::Class => {
                !classifier.is_abstract && !classifier.is_extensible
            }
            crate::libraries::TypeKind::Enum => !classifier.is_extensible,
            crate::libraries::TypeKind::Interface => false,
        })
    }

    fn can_have_subtypes_ignoring_nullability(&self, ty: Ty) -> Option<bool> {
        let ty = match ty {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => *inner,
            ty => ty,
        };
        if ty == Ty::Nothing {
            return Some(false);
        }
        if matches!(ty, Ty::TyParam(..)) {
            return Some(true);
        }
        // Core's COMPACT variants for final Kotlin classifiers (`Unit`, `String`, the scalars and
        // unsigned types) have no `kotlin_class_internal`, and the permissive fallback below would
        // hand them a spurious `? extends` (`Function1<…, +Lkotlin/Unit;>` where kotlinc writes the
        // invariant spelling) — they are final, so answer directly.
        if matches!(ty, Ty::Unit | Ty::String) || ty.is_jvm_scalar() || ty.is_unsigned() {
            return Some(false);
        }
        // Core has compact variants for common Kotlin classifiers, but that storage choice does not
        // change the JVM wildcard rule. Ask for their semantic classifier identity exactly as for an
        // `Obj`; otherwise final `String`/numeric arguments would incorrectly gain `? extends`.
        let Some(owner) = ty.kotlin_class_internal() else {
            return Some(true);
        };
        let arguments = ty.type_args();
        let chain = self.classifier_signature_chain(owner)?;
        let declared_arguments = chain.iter().map(|(_, variances)| variances.len()).sum();
        let arguments = self.classifier_usage_arguments(owner, arguments, declared_arguments)?;
        let is_closed = self.classifier_is_closed(owner)?;
        if !is_closed {
            return Some(true);
        }
        for (index, argument) in arguments.iter().copied().enumerate() {
            let declaration = self.declaration_variance(owner, index)?;
            let (use_site, argument) = match argument {
                Ty::InProjection(inner) => (TypeVariance::In, *inner),
                Ty::OutProjection(inner) => (TypeVariance::Out, *inner),
                // A star is already the complete JVM wildcard. Its frontend read bound is not a
                // classfile type argument and must not participate in wildcard optimization.
                Ty::StarProjection(_) => return Some(true),
                argument => (TypeVariance::Invariant, argument),
            };
            let effective = if use_site == TypeVariance::Invariant {
                declaration
            } else {
                use_site
            };
            if effective == TypeVariance::Out
                && self.can_have_subtypes_ignoring_nullability(argument)?
            {
                return Some(true);
            }
            if effective == TypeVariance::In && argument.non_null() != Ty::obj("kotlin/Any") {
                return Some(true);
            }
        }
        Some(false)
    }

    fn wildcard_is_redundant(&self, variance: TypeVariance, argument: Ty) -> Option<bool> {
        match variance {
            TypeVariance::Invariant => Some(true),
            TypeVariance::Out => self
                .can_have_subtypes_ignoring_nullability(argument)
                .map(|can_have_subtypes| !can_have_subtypes),
            TypeVariance::In => Some(argument.non_null() == Ty::obj("kotlin/Any")),
        }
    }

    fn type_argument(
        &self,
        declaration: TypeVariance,
        argument: Ty,
        wildcards: Wildcards,
    ) -> Option<String> {
        match argument {
            Ty::StarProjection(_) => Some("*".to_string()),
            Ty::InProjection(inner) => Some(format!("-{}", self.ty_at(inner, wildcards)?)),
            Ty::OutProjection(inner) => Some(format!("+{}", self.ty_at(inner, wildcards)?)),
            argument => {
                let mut signature = String::new();
                if wildcards == Wildcards::Declared
                    && !self.wildcard_is_redundant(declaration, argument)?
                {
                    match declaration {
                        TypeVariance::In => signature.push('-'),
                        TypeVariance::Out => signature.push('+'),
                        TypeVariance::Invariant => {}
                    }
                }
                signature.push_str(&self.ty_at(&argument, wildcards)?);
                Some(signature)
            }
        }
    }

    fn function_ty(&self, signature: &crate::types::FnSig, wildcards: Wildcards) -> Option<String> {
        let arity = signature.params.len() + usize::from(signature.suspend);
        if arity > 22 {
            // `FunctionN` has only its covariant result parameter. Kotlin metadata carries the
            // complete parameter list; the Java generic Signature records only this JVM carrier.
            let result = if signature.suspend {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                signature.ret
            };
            return Some(format!(
                "Lkotlin/jvm/functions/FunctionN<{}>;",
                self.type_argument(TypeVariance::Out, result, wildcards)?
            ));
        }
        let mut rendered = format!("Lkotlin/jvm/functions/Function{arity}<");
        for parameter in &signature.params {
            rendered.push_str(&self.type_argument(TypeVariance::In, *parameter, wildcards)?);
        }
        if signature.suspend {
            rendered.push_str("-Lkotlin/coroutines/Continuation<-");
            rendered.push_str(&self.ty_at(&signature.ret, wildcards)?);
            rendered.push_str(">;");
            rendered.push_str("+Ljava/lang/Object;");
        } else {
            rendered.push_str(&self.type_argument(TypeVariance::Out, signature.ret, wildcards)?);
        }
        rendered.push_str(">;");
        Some(rendered)
    }

    /// One parameter or return position in a method `Signature`. Positions without generic structure
    /// use their exact JVM descriptor spelling; structured positions are rendered from the semantic
    /// type. This is a structural choice, not a recovery path after semantic formatting failed.
    fn method_ty(&self, ty: &Ty, wildcards: Wildcards) -> Option<String> {
        let semantic = match ty {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner,
            ty => ty,
        };
        match semantic {
            Ty::TyParam(..) | Ty::Fun(_) => self.ty_at(ty, wildcards),
            Ty::Obj(_, arguments) if !arguments.is_empty() => self.ty_at(ty, wildcards),
            Ty::InProjection(_)
            | Ty::OutProjection(_)
            | Ty::StarProjection(_)
            | Ty::Null
            | Ty::Error => {
                self.run.set_emit_error(format!(
                    "internal: invalid semantic method-signature type {ty:?}"
                ));
                None
            }
            _ => Some(ir_type_desc(ty)),
        }
    }

    /// Translate one semantic Kotlin type into a JVM generic-signature element. Kotlin declaration-
    /// site variance has no classfile equivalent, so the JVM backend realizes it as a wildcard on
    /// each otherwise-unprojected use-site argument. Explicit Kotlin `in`/`out` projections already
    /// carry their own direction and take precedence.
    /// A type in a PARAMETER position (declaration-site variance becomes a wildcard).
    fn ty(&self, ty: &Ty) -> Option<String> {
        self.ty_at(ty, Wildcards::Declared)
    }

    fn ty_at(&self, ty: &Ty, wildcards: Wildcards) -> Option<String> {
        if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = ty {
            return self.ty_at(inner, wildcards);
        }
        if let Ty::TyParam(name, _) = ty {
            return Some(format!(
                "T{};",
                crate::types::type_parameter_source_name(name)
            ));
        }
        if ty.non_null().is_jvm_scalar() {
            return Some(boxed_descriptor(ty.non_null()));
        }
        match *ty {
            Ty::String => Some("Ljava/lang/String;".to_string()),
            Ty::Unit => Some("Lkotlin/Unit;".to_string()),
            Ty::Nothing => Some("Lkotlin/Nothing;".to_string()),
            Ty::InProjection(inner) => Some(format!("-{}", self.ty_at(inner, wildcards)?)),
            Ty::OutProjection(inner) => Some(format!("+{}", self.ty_at(inner, wildcards)?)),
            Ty::Fun(signature) => self.function_ty(signature, wildcards),
            // `kotlin.Array<E>` has no JVM class: its realization is the ARRAY type `[E`, and that is
            // how a signature must spell it. Writing `Lkotlin/Array<…>;` names a class no loader can
            // resolve, so any reader of the attribute (reflection, a Java consumer, a decompiler)
            // fails on it. kotlinc writes `[` + the element's signature — and, since a signature that
            // adds nothing over the descriptor is omitted entirely, `Array<String>` ends up with no
            // attribute at all while `Array<T>` keeps `[TT;`.
            Ty::Obj(owner, arguments) if owner.matches("kotlin/Array") && arguments.len() == 1 => {
                // The element's own variance is not written: a JVM array type has no argument list to
                // put it on. `Array<out String>` erases to `[Ljava/lang/String;`, as kotlinc emits.
                let element = match &arguments[0] {
                    Ty::InProjection(inner)
                    | Ty::OutProjection(inner)
                    | Ty::StarProjection(inner) => inner,
                    argument => argument,
                };
                Some(format!("[{}", self.ty_at(element, wildcards)?))
            }
            Ty::Obj(owner, arguments) => {
                let chain = self.classifier_signature_chain(owner)?;
                let declared_arguments: usize =
                    chain.iter().map(|(_, variances)| variances.len()).sum();
                let arguments =
                    self.classifier_usage_arguments(owner, arguments, declared_arguments)?;
                let (outer, _) = chain.first()?;
                let outer_internal = outer.render();
                let jvm = crate::jvm::names::classfile_internal_name(&outer_internal);
                let mut signature = format!("L{jvm}");
                let mut argument_index = 0;
                for (segment_index, (classifier, variances)) in chain.iter().enumerate() {
                    if segment_index != 0 {
                        signature.push('.');
                        signature.push_str(classifier.nested_segment_ref());
                    }
                    if !variances.is_empty() {
                        signature.push('<');
                        for &variance in variances {
                            signature.push_str(&self.type_argument(
                                variance,
                                arguments[argument_index],
                                wildcards,
                            )?);
                            argument_index += 1;
                        }
                        signature.push('>');
                    }
                }
                signature.push(';');
                Some(signature)
            }
            _ => None,
        }
    }
}

fn jvm_method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    g: &crate::ir::IrGenericSig,
    f: &crate::ir::IrFunction,
) -> Option<String> {
    let mut s = jvm_type_params(formatter, g)?;
    s.push('(');
    for parameter in &g.params {
        s.push_str(&formatter.method_ty(parameter, Wildcards::Declared)?);
    }
    s.push(')');
    let ret = g.ret.as_ref().unwrap_or(&f.ret);
    s.push_str(&formatter.method_ty(ret, Wildcards::Suppressed)?);
    Some(s)
}

/// Format a class's generic shape into a JVM class `Signature` (`<T:Ljava/lang/Object;>Ljava/lang/Object;`).
fn jvm_class_signature(
    formatter: &JvmSignatureFormatter<'_>,
    g: &crate::ir::IrGenericSig,
) -> Option<String> {
    let mut s = jvm_type_params(formatter, g)?;
    if g.supers.is_empty() {
        // A plain generic class with no (parameterized) supertypes: just extends `Object`.
        s.push_str("Ljava/lang/Object;");
    } else {
        // The parameterized superclass + interfaces (`Ljava/lang/Object;LOperation<Lkotlin/Result<..>;>;`),
        // formatted from the platform-agnostic `Ty`s so a reader recovers a member's concrete generic
        // return. A class header is not a method-parameter position: declaration-site variance is
        // suppressed (`interface L<E> : List<E>` implements Java `List<E>`, not `List<? extends E>`).
        // An explicit source projection remains encoded by `ty_at` itself.
        for sup in &g.supers {
            s.push_str(&formatter.ty_at(sup, Wildcards::Suppressed)?);
        }
    }
    Some(s)
}

/// A `Ty` as a JVM generic-signature type element: a primitive in a generic position is its BOXED wrapper
/// (`Int` → `Ljava/lang/Integer;`), a reference maps its internal (`kotlin/Any` → `java/lang/Object`) and
/// carries its (recursively formatted) type arguments. `None` for a shape not representable here.
/// The generic `Signature` element for a parameterized concrete type (`List<String>` →
/// `Ljava/util/List<Ljava/lang/String;>;`); `None` when erasure loses nothing. `T?` unwraps (generics
/// survive nullability). Bare type parameters are handled separately via `field_signatures`.
/// A FIELD's generic `Signature`, which follows the return-position rule: no declaration-site
/// wildcards. A constructor PARAMETER of the same declared type takes them, so it goes through
/// [`parameterized_sig_at`] with the parameter mode instead.
fn parameterized_sig(formatter: &JvmSignatureFormatter<'_>, ty: &Ty) -> Option<String> {
    parameterized_sig_at(formatter, ty, Wildcards::Suppressed)
}

fn parameterized_sig_at(
    formatter: &JvmSignatureFormatter<'_>,
    ty: &Ty,
    wildcards: Wildcards,
) -> Option<String> {
    let inner = match ty {
        Ty::Nullable(t) | Ty::PlatformNullable(t) => t,
        t => t,
    };
    match inner {
        Ty::Obj(_, args) if !args.is_empty() => {
            let sig = formatter.ty_at(inner, wildcards)?;
            (sig != ir_type_desc(inner)).then_some(sig)
        }
        Ty::Fun(_) => formatter.ty_at(inner, wildcards),
        _ => None,
    }
}

/// The three JVM generic `Signature` attributes that realize one Kotlin property declaration.
/// Storage location (instance field, object static, or file-facade static) does not change these
/// declaration signatures, so every physical emitter consumes this common shape.
struct JvmPropertySignatures {
    field: Option<String>,
    getter: Option<String>,
    setter: Option<String>,
}

fn property_jvm_signatures(
    formatter: &JvmSignatureFormatter<'_>,
    ty: &Ty,
    type_parameter: Option<&str>,
) -> JvmPropertySignatures {
    if let Some(parameter) = type_parameter {
        return JvmPropertySignatures {
            field: Some(format!("T{parameter};")),
            getter: Some(format!("()T{parameter};")),
            setter: Some(format!("(T{parameter};)V")),
        };
    }
    JvmPropertySignatures {
        field: parameterized_sig(formatter, ty),
        getter: method_parameterized_sig(formatter, &[], ty),
        setter: method_parameterized_sig(formatter, std::slice::from_ref(ty), &Ty::Unit),
    }
}

/// A method's generic `Signature`, from whichever source applies: its own declared type parameters, the
/// `suspend` CPS shape, or a parameterized concrete parameter/return type. `None` when erasure loses
/// nothing. Shared by concrete and abstract emission — an abstract method has no body, which is not a
/// reason to drop its signature.
/// `Signature` for a member whose SEMANTIC types mention enclosing-class type parameters: each
/// position is a bare `T<name>;` reference or a plain non-generic descriptor. `None` when any
/// position needs deeper generic formatting (those flow through the ordinary formatters).
fn member_semantic_signature(params: &[Ty], ret: Ty) -> Option<String> {
    fn part(t: Ty) -> Option<String> {
        match t {
            Ty::TyParam(name, _) => Some(format!(
                "T{};",
                crate::types::type_parameter_source_name(name)
            )),
            Ty::Nullable(inner) if matches!(*inner, Ty::TyParam(..)) => part(*inner),
            t if !crate::types::ty_mentions_any_param(t) && t.type_args().is_empty() => {
                Some(crate::jvm::names::type_descriptor(ir_ty_to_jvm(&t)))
            }
            _ => None,
        }
    }
    let mut out = String::from("(");
    for &p in params {
        out.push_str(&part(p)?);
    }
    out.push(')');
    out.push_str(&part(ret)?);
    Some(out)
}

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
    if let Some(generic) = ir.signatures.get(&fid) {
        let generic = value_class_signatures::physical_generic_signature(ir, fid, f, generic);
        return jvm_method_signature(formatter, &generic, f);
    }
    if let (Some((params, ret)), Some(_)) = (
        ir.member_semantic_sigs.get(&fid),
        ir.suspend_declared_sigs.get(&fid),
    ) {
        return suspend_method_sig(formatter, params, ret);
    }
    if let Some((params, ret)) = value_class_signatures::physical_member_signature(ir, fid, f) {
        // A member using ENCLOSING-CLASS type parameters signs with bare references (`(TT;)TT;`)
        // and declares nothing — the parameters belong to the class header's own signature.
        if let Some(sig) = member_semantic_signature(&params, ret) {
            return Some(sig);
        }
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
    method_parameterized_sig(formatter, &f.params, &f.ret)
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
    // A type argument drops nullability (`OrganizationId?` and `String?` both sign as the bare type),
    // so unwrap before formatting.
    let ret = match ret {
        Ty::Nullable(inner) => inner,
        t => t,
    };
    // The suspend return travels as `Continuation<-RET>`, a PARAMETER of the erased method, and
    // kotlinc wildcards inside it: `suspend fun <U> f(): Cont<U>` signs
    // `(Lkotlin/coroutines/Continuation<-LCont<+TU;>;>;)Ljava/lang/Object;`.
    let ret_arg = formatter.ty_at(ret, Wildcards::Declared)?;
    let mut s = String::from("(");
    for p in params {
        s.push_str(&formatter.method_ty(p, Wildcards::Declared)?);
    }
    s.push_str("Lkotlin/coroutines/Continuation<-");
    s.push_str(&ret_arg);
    s.push_str(">;)Ljava/lang/Object;");
    Some(s)
}

/// A method's generic `Signature` when a concrete param/return type is parameterized (`getXs()` →
/// `()Ljava/util/List<Ljava/lang/String;>;`); non-generic positions keep their erased descriptor,
/// `None` when none are parameterized.
fn method_parameterized_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
) -> Option<String> {
    // Runs for every emitted method — bail before building any string when no position can carry one.
    let is_parameterized = |t: &Ty| {
        let inner = match t {
            Ty::Nullable(t) | Ty::PlatformNullable(t) => t,
            t => t,
        };
        matches!(inner, Ty::Fun(_))
            || matches!(inner, Ty::Obj(_, arguments) if !arguments.is_empty())
    };
    if !params
        .iter()
        .chain(std::iter::once(ret))
        .any(is_parameterized)
    {
        return None;
    }
    let mut s = String::from("(");
    for p in params {
        s.push_str(&formatter.method_ty(p, Wildcards::Declared)?);
    }
    s.push(')');
    s.push_str(&formatter.method_ty(ret, Wildcards::Suppressed)?);
    Some(s)
}

/// The primary constructor's generic `Signature` — bare type-parameter params (`(TT;)V`) and
/// parameterized concrete params (`(Ljava/util/List<Ljava/lang/String;>;)V`), others erased; `None` when
/// none need generics. Shared by the pool seeder and the attribute emitter so both produce one string.
fn class_ctor_generic_sig(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fq_name: &str,
) -> Option<String> {
    let param_tys = class_ctor_jvm_tys(c);
    let ftp = ir.field_signatures(fq_name);
    let is_field: Vec<bool> = if c.ctor_args.is_empty() {
        vec![true; param_tys.len()]
    } else {
        c.ctor_args.iter().map(|a| a.is_field).collect()
    };
    let mut sig = String::from("(");
    let mut any = false;
    let mut field_i = 0usize;
    for (i, t) in param_tys.iter().enumerate() {
        let declared_ty = c.ctor_args.get(i).and_then(|argument| argument.declared_ty);
        let declared_type_parameter = declared_ty.and_then(|ty| match ty {
            Ty::TyParam(name, _) => Some(name),
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => match *inner {
                Ty::TyParam(name, _) => Some(name),
                _ => None,
            },
            _ => None,
        });
        if let Some(parameter) = declared_type_parameter {
            sig.push_str(&format!(
                "T{};",
                crate::types::type_parameter_source_name(parameter)
            ));
            any = true;
            if is_field.get(i).copied().unwrap_or(true) {
                field_i += 1;
            }
            continue;
        }
        if let Some(parameterized) = declared_ty.and_then(|ty| {
            // Constructor arguments are parameter positions even when they also declare a property.
            parameterized_sig_at(formatter, &ty, Wildcards::Declared)
        }) {
            sig.push_str(&parameterized);
            any = true;
            if is_field.get(i).copied().unwrap_or(true) {
                field_i += 1;
            }
            continue;
        }
        if is_field.get(i).copied().unwrap_or(true) {
            let f = c.fields.get(field_i);
            let fname = f.map(|f| f.name.as_str()).unwrap_or("");
            if let Some((_, tp)) = ftp.and_then(|ftp| ftp.iter().find(|(fp, _)| fp == fname)) {
                sig.push_str(&format!("T{tp};"));
                any = true;
            } else if let Some(ps) = f.and_then(|f| {
                // A constructor parameter is a PARAMETER position, even though the same declaration
                // also backs a field, whose own signature suppresses the wildcards.
                parameterized_sig_at(formatter, &f.ty, Wildcards::Declared)
            }) {
                sig.push_str(&ps);
                any = true;
            } else {
                sig.push_str(&type_descriptor(*t));
            }
            field_i += 1;
        } else {
            sig.push_str(&type_descriptor(*t));
        }
    }
    sig.push_str(")V");
    any.then_some(sig)
}

/// The shared `<T:bound…>` type-parameter DECLARATION section, or `""` when there are no own type
/// parameters (e.g. a generic class's getter `getA()` → `()TA;` USES the class's `A` but declares none).
/// `None` if any bound can't be represented.
fn jvm_type_params(
    formatter: &JvmSignatureFormatter<'_>,
    g: &crate::ir::IrGenericSig,
) -> Option<String> {
    if g.type_params.is_empty() {
        return Some(String::new());
    }
    let mut s = String::from("<");
    for parameter in &g.type_params {
        s.push_str(&parameter.name);
        if parameter.bounds.is_empty() {
            s.push_str(":Ljava/lang/Object;");
            continue;
        }
        let bounds = parameter.bounds.iter();
        if bounds.clone().all(|(_, is_interface)| *is_interface) {
            s.push(':');
        }
        for (bound, _) in bounds {
            s.push(':');
            s.push_str(&jvm_bound_descriptor(formatter, bound)?);
        }
    }
    s.push('>');
    Some(s)
}

/// A type-parameter upper bound as a JVM signature element: `kotlin/Any` → `Ljava/lang/Object;`, a
/// primitive → its boxed wrapper (`kotlin/Int` → `Ljava/lang/Integer;`). `None` for anything else.
fn jvm_bound_descriptor(formatter: &JvmSignatureFormatter<'_>, bound: &Ty) -> Option<String> {
    if *bound == Ty::obj("kotlin/Any") {
        return Some("Ljava/lang/Object;".to_string());
    }
    if bound.is_jvm_scalar() {
        return bound.nullable_boxed().map(type_descriptor);
    }
    formatter.ty(bound)
}

/// kotlinc opens a member's `$default` synthetic with a guard on the trailing marker parameter:
/// a `super.m()` call carrying defaults is impossible to dispatch (the stub would re-enter the
/// OVERRIDE through `invokevirtual`), so it passes a non-null marker and the stub throws.
///
/// Only a member whose owner can be inherited from gets it. Measured against kotlinc 2.4.10: an
/// `open`/`abstract`/`sealed` class does, and so does an `enum class` (its entries may carry bodies,
/// which subclass it); a final class — including a `data class`, a nested or companion object, and a
/// private one — does not, nor does an interface's `$DefaultImpls` or a file facade, none of which
/// can be the receiver of such a `super` call.
fn emit_default_super_guard(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    marker_slot: u16,
    method_name: &str,
) {
    code.aload(marker_slot);
    let ok = code.new_label();
    // The fall-through target is a branch target, so it needs a StackMapTable entry: the method-entry
    // locals with an empty stack (kotlinc writes a `same_frame` here). Without it the JVM rejects the
    // method — "Expecting a stackmap frame at branch target".
    code.ifnull(ok);
    let cls = cw.class_ref("java/lang/UnsupportedOperationException");
    code.new_obj(cls);
    code.dup();
    code.push_string(
        &format!(
            "Super calls with default arguments not supported in this target, function: {method_name}"
        ),
        cw,
    );
    let ctor = cw.methodref(
        "java/lang/UnsupportedOperationException",
        "<init>",
        "(Ljava/lang/String;)V",
    );
    code.invokespecial(ctor, 1, 0);
    code.athrow();
    code.bind(ok);
}

/// Whether a CLASS can be inherited from, and so whether its members' `$default` synthetics carry
/// kotlinc's super-call guard. An interface is not asked — it is always a possible `super<I>.m()`
/// receiver, so its stub is guarded unconditionally. See [`emit_default_super_guard`].
fn owner_is_inheritable(ir: &IrFile, owner: &str) -> bool {
    ir.classes.iter().any(|c| {
        c.fq_name_matches(owner)
            && !c.is_object
            && (c.is_open || c.is_abstract || c.is_sealed || !c.enum_entries.is_empty())
    })
}

/// Emit the JVM `<name>$default(self, params…, mask: int, marker: Object)` synthetic stub for an
/// instance method with default-valued parameters: for each defaulted param, `if ((mask & (1<<i)) != 0)
/// param = <default>;` then tail-call the real method. The default-value exprs reference `self` as value
/// 0. This is the JVM realization of default arguments — the `param_defaults` *meaning* is in the IR.
#[allow(clippy::too_many_arguments)]
fn emit_default_stub(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    is_interface: bool,
) {
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    // The REAL (base-method) param types unbox every value class. `stub_param_tys` is the `$default`
    // signature, where a nullable-underlying value-class param stays BOXED (kotlinc): the stub takes the
    // value class, `box-impl`s any default-filled value, and `unbox-impl`s before delegating to the base.
    let real_params = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    let stub_param_tys: Vec<Ty> = real_params
        .iter()
        .enumerate()
        .map(|(i, t)| boxed.get(&i).copied().unwrap_or(*t))
        .collect();
    let recv_offset = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(recv_offset)
        .expect("an extension receiver is a leading physical parameter");
    let ret = jvm_declared_ty(&f.ret);
    let owner_ty = Ty::obj(owner);
    // kotlinc interns a synthetic's name and descriptor at its method header, before its body.
    let stub_name = format!("{method_name}$default");
    let stub_desc = method_descriptor(&default_stub_params(ir, fid, owner_ty), ret);
    cw.reserve_method_name(&stub_name);
    cw.reserve_descriptor(&stub_desc);
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        owner,
        facade,
        ret,
        defaults.iter().flatten().copied(),
    );
    // value 0 = self; values 1..=n = the real params; then mask + marker (not value-indexed).
    let receiver = e.frame.enter(FrameKey::Receiver, owner_ty);
    e.slots.insert(0, (receiver, owner_ty));
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in stub_param_tys.iter().enumerate() {
        let value = (i + 1) as u32;
        let slot = e.frame.enter(FrameKey::Value(value), *t);
        e.slots.insert(value, (slot, *t));
        param_slots.push((slot, *t));
    }
    let mask_count = default_mask_count(logical_param_count);
    let mask_slots: Vec<u16> = (0..mask_count)
        .map(|mask| {
            let s = e.frame.enter(
                FrameKey::Parameter((stub_param_tys.len() + mask) as u16),
                Ty::Int,
            );
            // The mask ints and the trailing marker are BACKEND temporaries: no semantic value
            // names them, they only have to be typed in every frame this stub records.
            // Held for the whole stub: nothing releases a mask word before the method ends.
            let _ = e.lease_temporary(s, Ty::Int);
            s
        })
        .collect();
    let marker_slot = e.frame.enter(
        FrameKey::Parameter((stub_param_tys.len() + mask_count) as u16),
        Ty::obj("java/lang/Object"),
    );
    let _ = e.lease_temporary(marker_slot, Ty::obj("java/lang/Object"));

    let mut code = CodeBuilder::new(e.frame.size());
    // An INTERFACE's stub is guarded too: `super<I>.m()` is a real call site, so kotlinc puts the
    // guard on whichever class carries the mask-expanding body — the interface itself under
    // `-jvm-default=enable`, the `$DefaultImpls` holder under `disable`. (The enable-mode holder copy
    // is a thin forward emitted by `emit_default_stub_forward` and correctly carries no guard: it
    // passes the marker straight through to the body that does check it.)
    if is_interface || owner_is_inheritable(ir, owner) {
        emit_default_super_guard(e.cw, &mut code, marker_slot, &method_name);
    }
    // A MEMBER EXTENSION's physical params — and its registered defaults — lead with the extension
    // receiver: slice that prefix off (the receiver never defaults) and offset the slots, so the
    // mask bits stay LOGICAL (kotlinc's convention).
    emit_default_param_overwrites(
        &mut e,
        &mut code,
        &defaults[recv_offset..],
        recv_offset,
        &param_slots,
        &mask_slots,
    );
    code.aload(0);
    for (i, &(pslot, pty)) in param_slots.iter().enumerate() {
        load(pty, pslot, &mut code);
        // A boxed value-class stub param unboxes to the underlying the base (mangled) method expects.
        if let Some(vc) = boxed.get(&i) {
            emit_unbox_impl(ir, e.cw, vc, &mut code);
        }
    }
    let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
    let desc = method_descriptor(&real_params, ret);
    let is_private = ir.private_methods.contains(&fid);
    if is_interface {
        // The default stub is a STATIC interface method; it dispatches to the real (abstract) member via
        // `invokeinterface` on `$this`.
        let m = e.cw.interface_methodref(owner, &method_name, &desc);
        code.invokeinterface(m, aw, slot_words(ret) as i32);
    } else if is_private {
        // A PRIVATE member is non-virtual — `invokevirtual` on it fails resolution pre-nestmates
        // (class-file major 52); kotlinc dispatches with `invokespecial`.
        let m = e.cw.methodref(owner, &method_name, &desc);
        code.invokespecial(m, aw, slot_words(ret) as i32);
    } else {
        let m = e.cw.methodref(owner, &method_name, &desc);
        code.invokevirtual(m, aw, slot_words(ret) as i32);
    }
    emit_return(ret, &mut code);
    code.ensure_locals(e.frame.max());
    code.link();

    e.cw.add_method(default_stub_access(ir, fid), &stub_name, &stub_desc, &code);
    // kotlinc gives the synthetic a one-entry LineNumberTable at the function's DECLARATION line
    // (`fn_sig_lines`) — not the body-attributed `fn_decl_lines`, which points at an expression
    // body's own line when the signature wraps.
    if let Some(&line) = ir
        .fn_sig_lines
        .get(&fid)
        .or_else(|| ir.fn_decl_lines.get(&fid))
    {
        e.cw.set_method_lines(&stub_name, &stub_desc, &[(0, line)]);
    }
}

/// The `$default` stub's physical parameter list, shared by the stub emitter and the holder's
/// `enable`-mode forward (their descriptors must agree or the forward links to nothing): the
/// receiver, the declared parameters (a nullable-underlying value-class parameter stays BOXED),
/// one `int` mask per 32 logical parameters, and the trailing `Object` marker.
fn default_stub_params(ir: &IrFile, fid: u32, owner_ty: Ty) -> Vec<Ty> {
    let real_params = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    let recv_offset = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(recv_offset)
        .expect("an extension receiver is a leading physical parameter");
    let mut stub_params = vec![owner_ty];
    stub_params.extend(
        real_params
            .iter()
            .enumerate()
            .map(|(i, t)| boxed.get(&i).copied().unwrap_or(*t)),
    );
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_param_count),
    ));
    stub_params.push(Ty::obj("java/lang/Object"));
    stub_params
}

fn default_stub_boxed_parameters(ir: &IrFile, fid: u32) -> HashMap<usize, Ty> {
    ir.default_stub_boxed_params
        .get(&fid)
        .map(|parameters| parameters.iter().copied().collect())
        .unwrap_or_default()
}

fn static_default_stub_params(ir: &IrFile, fid: u32, marker: Ty) -> Vec<Ty> {
    let function = &ir.functions[fid as usize];
    let mut parameters = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    for (index, parameter) in parameters.iter_mut().enumerate() {
        if let Some(boxed) = boxed.get(&index) {
            *parameter = *boxed;
        }
    }
    let receiver_prefix = usize::from(function.is_static && function.dispatch_receiver.is_some())
        + usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_parameter_count = parameters
        .len()
        .checked_sub(receiver_prefix)
        .expect("callable receivers are leading physical parameters");
    parameters.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_parameter_count),
    ));
    parameters.push(marker);
    parameters
}

/// The access flags of a member's `$default` synthetic: kotlinc mirrors the origin's visibility —
/// with PRIVATE demoted to package-private (the stub is invoked from call sites that could not reach the
/// private member itself) — always `| STATIC | SYNTHETIC`. Keyed on the IR's visibility model in ONE
/// place: it currently distinguishes public vs private (`ir.private_methods`); when the IR carries
/// protected/internal, their mappings extend here.
fn default_stub_access(ir: &IrFile, fid: u32) -> u16 {
    let vis = if ir.private_methods.contains(&fid) {
        0x0000 // package-private
    } else {
        0x0001 // ACC_PUBLIC
    };
    vis | 0x1008 // ACC_STATIC | ACC_SYNTHETIC
}

/// `defaults` is LOGICAL (Kotlin value parameters, extension receiver excluded); `recv_offset`
/// counts the leading physical receiver slots. The MASK bit index is the LOGICAL parameter index —
/// kotlinc numbers `$default` mask bits over the declared value parameters, so an extension's
/// receiver does not shift them (verified against kotlinc 2.4.0: `fun Host.tag(name, port = 9)` →
/// port checks bit 2, not bit 4).
fn emit_default_param_overwrites(
    e: &mut Emitter<'_>,
    code: &mut CodeBuilder,
    defaults: &[Option<u32>],
    recv_offset: usize,
    param_slots: &[(u16, Ty)],
    mask_slots: &[u16],
) {
    let logical_param_count = param_slots
        .len()
        .checked_sub(recv_offset)
        .expect("an extension receiver is a leading physical parameter");
    for (i, def) in defaults.iter().enumerate().take(logical_param_count) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i + recv_offset];
            code.iload(mask_slots[i / 32]);
            code.push_int(default_mask_bit(i), e.cw);
            code.iand();
            let skip = code.new_label();
            code.ifeq(skip);
            // The value-class realization pass has already adapted the checked default root to this
            // exact physical stub slot. Emission only stores that selected representation.
            e.emit_value(*def_expr, code);
            store(pty, pslot, code);
            code.bind(skip);
        }
    }
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

fn emit_constructor_default_arguments(
    omitted: &[u32],
    parameter_count: usize,
    code: &mut CodeBuilder,
    cw: &mut ClassWriter,
) {
    if omitted.is_empty() {
        return;
    }
    let masks = constructor_default_masks(omitted, parameter_count);
    for mask in masks {
        code.push_int(mask, cw);
    }
    code.aconst_null();
}

fn constructor_default_masks(omitted: &[u32], parameter_count: usize) -> Vec<i32> {
    if omitted.is_empty() {
        return Vec::new();
    }
    let mut masks = vec![0; default_mask_count(parameter_count)];
    for &parameter in omitted {
        let parameter = parameter as usize;
        masks[parameter / 32] |= default_mask_bit(parameter);
    }
    masks
}

/// A value class's (erased) underlying JVM type — its single field's type.
fn vc_underlying_jvm(ir: &IrFile, vc: &Ty) -> Ty {
    vc.obj_internal()
        .and_then(|fq| {
            let fq = fq.render();
            ir.classes.iter().find(|c| c.fq_name_matches(&fq))
        })
        .and_then(|c| c.fields.first())
        .map(|f| jvm_declared_ty(&f.ty))
        .unwrap_or(Ty::obj("java/lang/Object"))
}

/// Emit `VC.box-impl(<underlying>)LVC;` (static) — boxes the underlying value on the stack into `VC`.
fn emit_box_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let fq = vc
        .obj_internal()
        .map(|n| n.render())
        .unwrap_or_else(|| "java/lang/Object".to_string());
    let u = vc_underlying_jvm(ir, vc);
    let m = cw.methodref(&fq, "box-impl", &format!("({})L{fq};", type_descriptor(u)));
    code.invokestatic(m, slot_words(u) as i32, 1);
}

/// Emit `VC.unbox-impl()<underlying>` (virtual) — unboxes the `VC` on the stack to its underlying.
fn emit_unbox_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let fq = vc
        .obj_internal()
        .map(|n| n.render())
        .unwrap_or_else(|| "java/lang/Object".to_string());
    let u = vc_underlying_jvm(ir, vc);
    let m = cw.methodref(&fq, "unbox-impl", &format!("(){}", type_descriptor(u)));
    code.invokevirtual(m, 0, slot_words(u) as i32);
}

/// Emit the `foo$default(params…, int mask, Object marker)` synthetic for a TOP-LEVEL facade function
/// (kotlinc's default-argument ABI). Unlike [`emit_default_stub`] (an instance member) there is NO leading
/// `self`: the real parameters occupy value-indices `0..n` (the STATIC layout the defaults were lowered
/// with), and the stub dispatches to the real facade method via `invokestatic`. For each `mask & (1<<i)`
/// bit set, the argument slot is overwritten with `default_i` before the dispatch.
/// Whether an emitted body contains reification-marker nodes (a `<reified T>` fn realized as a
/// standalone method) — its `$default` must inline the body, never delegate.
fn body_has_reified_markers(ir: &IrFile, body: crate::ir::ExprId) -> bool {
    fn walk(ir: &IrFile, e: crate::ir::ExprId) -> bool {
        if matches!(
            ir.expr(e),
            IrExpr::ReifiedClassMarker { .. } | IrExpr::ReifiedTypeOp { .. }
        ) {
            return true;
        }
        let mut found = false;
        crate::ir::for_each_child(&ir.exprs, e, &mut |child| {
            if walk(ir, child) {
                found = true;
            }
        });
        found
    }
    walk(ir, body)
}

fn emit_facade_default_stub(
    ir: &IrFile,
    fid: u32,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    marker: Ty,
) {
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    let real_params = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    let stub_param_tys = real_params
        .iter()
        .enumerate()
        .map(|(index, parameter)| boxed.get(&index).copied().unwrap_or(*parameter))
        .collect::<Vec<_>>();
    let ret = jvm_declared_ty(&f.ret);
    // A source value-class member is now a static `*-impl` whose leading carrier parameter is the
    // former dispatch receiver. It participates in the physical descriptor but not in Kotlin's
    // default-mask ordinals, just like an extension receiver.
    let dispatch_prefix = usize::from(f.is_static && f.dispatch_receiver.is_some());
    let extension_prefix = usize::from(ir.extension_receiver_fns.contains(&fid));
    let receiver_prefix = dispatch_prefix + extension_prefix;
    let logical_param_count = real_params
        .len()
        .checked_sub(receiver_prefix)
        .expect("callable receivers are leading physical parameters");
    // kotlinc interns the synthetic's NAME + DESCRIPTOR at its method header, before any constant
    // its body (the delegating call, the default fills) introduces.
    {
        let mask_words = default_mask_count(logical_param_count);
        let stub_desc = method_descriptor(
            &stub_param_tys
                .iter()
                .copied()
                .chain(std::iter::repeat_n(Ty::Int, mask_words))
                .chain(std::iter::once(marker))
                .collect::<Vec<_>>(),
            ret,
        );
        cw.seed_utf8(&format!("{method_name}$default"));
        cw.seed_utf8(&stub_desc);
    }

    let mut e = Emitter::new(
        ir,
        cw,
        env,
        facade,
        facade,
        ret,
        defaults.iter().flatten().copied(),
    );
    // No `self`: value-index `i` = the i-th real parameter (the static layout the defaults were lowered
    // with); then mask + marker (not value-indexed).
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in stub_param_tys.iter().enumerate() {
        let slot = e.frame.enter(FrameKey::Value(i as u32), *t);
        e.slots.insert(i as u32, (slot, *t));
        param_slots.push((slot, *t));
    }
    let mask_count = default_mask_count(logical_param_count);
    let mask_slots: Vec<u16> = (0..mask_count)
        .map(|mask| {
            let s = e.frame.enter(
                FrameKey::Parameter((stub_param_tys.len() + mask) as u16),
                Ty::Int,
            );
            // Backend temporaries — see the member stub: typed in frames, named by no value.
            // Held for the whole stub: nothing releases a mask word before the method ends.
            let _ = e.lease_temporary(s, Ty::Int);
            s
        })
        .collect();
    let marker_slot = e.frame.enter(
        FrameKey::Parameter((stub_param_tys.len() + mask_count) as u16),
        marker,
    );
    let _ = e.lease_temporary(marker_slot, marker);

    let mut code = CodeBuilder::new(e.frame.size());
    // A top-level EXTENSION's registered defaults/names carry a leading `$receiver` slot; the mask
    // bits stay LOGICAL (kotlinc's convention), so slice the receiver prefix off and offset slots.
    emit_default_param_overwrites(
        &mut e,
        &mut code,
        &defaults[extension_prefix..],
        receiver_prefix,
        &param_slots,
        &mask_slots,
    );
    // A REIFIED base (its body carries reification markers) cannot be DELEGATED to: the real
    // method throws at runtime and exists only to be inlined. kotlinc's `$default` therefore
    // inlines the whole body after the default fills — emit the same: the body was lowered with
    // value ids 0..n bound to the parameters, exactly this frame's layout.
    if let Some(body) = f.body.filter(|&body| body_has_reified_markers(ir, body)) {
        e.emit(body, &mut code);
        if ret == Ty::Unit && !e.discarding_diverges(body) {
            code.ret_void();
        }
    } else {
        for (index, &(pslot, pty)) in param_slots.iter().enumerate() {
            load(pty, pslot, &mut code);
            if let Some(value_class) = boxed.get(&index) {
                emit_unbox_impl(ir, e.cw, value_class, &mut code);
            }
        }
        let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
        let desc = method_descriptor(&real_params, ret);
        let m = e.cw.methodref(facade, &method_name, &desc);
        code.invokestatic(m, aw, slot_words(ret) as i32);
        emit_return(ret, &mut code);
    }
    code.ensure_locals(e.frame.max());
    code.link();

    let mut stub_params = stub_param_tys;
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_param_count),
    ));
    stub_params.push(marker);
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(
        default_stub_access(ir, fid),
        &format!("{method_name}$default"),
        &desc,
        &code,
    );
    // kotlinc gives the synthetic a one-entry LineNumberTable at the function's DECLARATION line
    // (`fn_sig_lines`) — not the body-attributed `fn_decl_lines`, which points at an expression
    // body's own line when the signature wraps.
    if let Some(&line) = ir
        .fn_sig_lines
        .get(&fid)
        .or_else(|| ir.fn_decl_lines.get(&fid))
    {
        e.cw.set_method_lines(&format!("{method_name}$default"), &desc, &[(0, line)]);
    }
}

/// The target-neutral identity and selected declaration shape shared by JVM property reads and writes.
/// Keeping these fields together prevents the two realizers from accepting subtly different owner,
/// interface, or physical-type inputs as the semantic operation grows more metadata.
struct PropertyOperation<'a> {
    expression: crate::ir::ExprId,
    receiver: Option<crate::ir::ExprId>,
    owner: &'a str,
    name: &'a str,
    ty: &'a Ty,
    interface: bool,
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    /// The narrow bytecode provider — lets the emitter read a cross-module `inline fun`'s compiled
    /// body (`bodies.body`) to splice it at the call site (the bytecode inliner).
    bodies: &'a dyn MethodBodies,
    /// The per-emit-run accumulators — the deep sites record a used lambda / an emit-or-inline bail
    /// here (formerly thread-locals).
    run: &'a EmitRun,
    /// `-jvm-default`, so a call site can tell where an interface's `$default` synthetic lives.
    jvm_default: JvmDefaultMode,
    property_realizations: &'a crate::jvm::property_realizations::PropertyRealizations,
    default_call_operands: &'a crate::jvm::default_call_operands::DefaultCallOperands,
    unit_result_tail_forwards: &'a crate::jvm::suspend::UnitResultTailForwards,
    owner: String,
    facade: String,
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
    /// Safe-call guards that share one null exit (`a?.b?.c`), by guard `When`: the label, and whether
    /// this guard is the chain's outermost one, which binds it and yields the null result.
    safe_call_null_exits: HashMap<u32, (Label, bool)>,
    /// A chain's receiver temporaries declared after its first guard already jumped to the shared
    /// null exit, by that exit: none of them is assigned on every path into it.
    safe_call_exit_temporaries: HashMap<Label, Vec<u32>>,
    /// Every `Variable` index → its JVM type (file-wide); a `value_ty(GetValue)` fallback for a slot not
    /// yet registered in `slots` (queried before its declaration emits — e.g. an inline result temp).
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
    /// The suspensions of the function being emitted, by call expression, in machine order. Empty
    /// for every function whose machine the IR pass owns.
    machine_suspensions: HashSet<u32>,
    /// The suspension points kotlinc's coroutine transformer takes, when it takes this function.
    transformed_suspensions: transformed_suspensions::TransformedSuspensions,
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
    /// Operand-stack verification types sitting BELOW the expression currently being emitted (an
    /// arithmetic LHS held on the stack across a branchy RHS, e.g. a data-class `hashCode` accumulator
    /// `result*31 + <branchy nullable-field hash>`). Prepended to every recorded stack-map frame's stack
    /// so the pending operand is typed through the branch (matching kotlinc), avoiding the spill-to-temp
    /// krusty would otherwise need. Pushed/popped around the branchy RHS in `emit_binop`.
    /// Open source locals: `(block_depth, slot, start_pc, name, descriptor)`.
    open_locals: Vec<(usize, u16, u16, String, String)>,
    /// Current block nesting depth; the function body is depth 1.
    block_depth: usize,
    /// The source line of the statement currently being emitted, when it has one. An operand that
    /// carries its own line leaves that line in effect; the instruction that CONSUMES the operand
    /// belongs to the statement, and kotlinc marks it back to this line.
    statement_line: Option<u32>,
    /// Whether this method records source-local debug entries.
    record_locals: bool,
    /// kotlinc's `isInsideCondition`: a `when` branch condition is being emitted, so an inlined
    /// call in it marks its own line again after the inlined code.
    inside_condition: bool,
    /// Slot 0 remains the verifier's special uninitialized receiver until the constructor delegates.
    this_uninitialized: bool,
    /// Independent realization strategies for plain lambdas and SAM conversions.
    lambda_modes: LambdaModes,
    /// Active `finally` bodies, outermost first. A source-level control transfer executes these
    /// before leaving its protected region; the stack carries exact IR identities, not syntax.
    return_finalizers: Vec<u32>,
    /// Protected-region accumulators for the active `try`s that have a `finally`, outermost first.
    /// A copy of a try's own finalizer must not lie inside that try's own ranges, or an exception
    /// raised while the finalizer runs re-enters the same handler and runs it a second time.
    finally_regions: Vec<FinallyRegion>,
    /// A statement at the lexical tail of a loop body may branch directly to the loop's next
    /// iteration. This is an emitter control-flow target, not a semantic `continue` manufactured in
    /// common IR. Blocks pass it only to their terminal statement.
    terminal_statement_target: Option<Label>,
}

impl<'a> Emitter<'a> {
    fn new(
        ir: &'a IrFile,
        cw: &'a mut ClassWriter,
        env: &EmitEnv<'a>,
        owner: &str,
        facade: &str,
        ret: Ty,
        roots: impl IntoIterator<Item = u32>,
    ) -> Self {
        let roots: Vec<u32> = roots.into_iter().collect();
        Self {
            ir,
            cw,
            bodies: env.bodies,
            run: env.run,
            jvm_default: env.jvm_default,
            property_realizations: env.property_realizations,
            default_call_operands: env.default_call_operands,
            unit_result_tail_forwards: env.unit_result_tail_forwards,
            owner: owner.to_string(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            temporaries: backend_temporaries::BackendTemporaries::default(),
            unassigned_values: HashSet::new(),
            label_unassigned_values: HashMap::new(),
            safe_call_null_exits: HashMap::new(),
            safe_call_exit_temporaries: HashMap::new(),
            var_types: collect_body_var_types(ir, roots.iter().copied()),
            value_stores: non_null_operands::ValueStores::collect(ir, &roots),
            checked_parameters: HashSet::new(),
            frame: frame_map::FrameMap::default(),
            continuation_slot: None,
            machine_suspensions: HashSet::new(),
            transformed_suspensions: HashMap::new(),
            machine_next_ordinal: 0,
            machine_entered: false,
            machine_resumes: Vec::new(),
            machine_joins: Vec::new(),
            machine: None,
            ret,
            loop_stack: Vec::new(),
            open_locals: Vec::new(),
            block_depth: 0,
            statement_line: None,
            record_locals: false,
            inside_condition: false,
            this_uninitialized: false,
            lambda_modes: env.lambda_modes,
            return_finalizers: Vec::new(),
            finally_regions: Vec::new(),
            terminal_statement_target: None,
        }
    }

    /// THE unified host+lambda splice (the merge of the branchy and lambda paths): splice a possibly
    /// BRANCHY host `inline fun` body, replacing each zero-arg lambda-parameter `Function0.invoke` site
    /// with that lambda's body. Handles `require(cond) { msg }` / `check(cond) { msg }` and the like —
    /// where the lambda runs only on a branch. Final-body dataflow carries any existing operand prefix
    /// through ordinary branches; handlers, suspensions, and external transfers require a spill
    /// boundary.
    /// Returns `false` (caller falls back / skips) on any unsupported shape.
    #[allow(clippy::too_many_arguments)]
    fn try_inline_unified(
        &mut self,
        call_expression: u32,
        callee: &str,
        inline_only: bool,
        descriptor: &str,
        args: &[u32],
        leading_non_argument_operands: usize,
        body: &crate::jvm::classreader::MethodCode,
        base: u16,
        code: &mut CodeBuilder,
        reified: &crate::jvm::inline::ReifiedArguments,
    ) -> bool {
        let Some(params) = parse_descriptor_params(descriptor) else {
            return false;
        };
        crate::trace_compiler!(
            "splice",
            "inline operands {:?}",
            args.iter()
                .map(|&argument| (argument, self.ir.expr(argument), self.value_ty(argument)))
                .collect::<Vec<_>>()
        );
        if params.len() != args.len() {
            return false;
        }
        let Some(materialized_roles) = self
            .ir
            .call_materialized_lambda_params
            .get(&call_expression)
        else {
            return false;
        };
        let lambda_parameters: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(index, &argument)| {
                matches!(
                    self.ir.expr(argument),
                    IrExpr::Lambda {
                        inline_body: Some(_),
                        ..
                    }
                ) && index
                    .checked_sub(leading_non_argument_operands)
                    .and_then(|parameter| materialized_roles.get(parameter))
                    == Some(&false)
            })
            .map(|(i, _)| i)
            .collect();
        // ONE plan for caller locals: where the relocated host body ends, and where each
        // substituted lambda's own locals begin. A substituted lambda's parameter slot is closed by
        // the splice, so the body occupies that many slots fewer; and a lambda's locals go at the
        // first slot free WHERE ITS INVOKE IS, not above every host local, because the reference
        // compiler reuses slots belonging to host locals that are not written yet.
        let spliced_frame =
            crate::jvm::inline::spliced_frame(body, descriptor, &lambda_parameters, base);
        let top_local = spliced_frame
            .as_ref()
            .map_or(base + body.max_locals, |frame| frame.top_local);
        self.frame.reserve_through(top_local);
        // Build each lambda argument's pre-relocated body, leaving its boxed result on the stack, and
        // record whether its instruction graph branches.
        let mut lam_splices: Vec<crate::jvm::inline::LambdaSplice> = Vec::new();
        // Capture initializers belong to lambda-creation time, in argument evaluation order. A
        // capture that is already a caller local needs no code; every other checked value is
        // materialized once when its lambda operand is reached, then the spliced body reads that
        // stable slot. This is required for nested inline lambdas (the captured value can itself be
        // a lambda), and also preserves side effects if a future capture initializer is not pure.
        let mut capture_materializations: Vec<(usize, u32, u16, Ty)> = Vec::new();
        // The deepest operand stack any spliced lambda body reaches — the host's `max_stack` must cover it,
        // since the body is inlined into the host (a deep lambda body, e.g. `123 != intArrayOf() as Any`,
        // would otherwise overflow the host's stack). Propagated to `splice_inline` below.
        let mut lam_max_stack = 0u16;
        for (i, &a) in args.iter().enumerate() {
            if !lambda_parameters.contains(&i) {
                continue;
            }
            let (bodies, lam_max_locals, lam_stack) = if let IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                inline_body,
                ..
            } = self.ir.expr(a).clone()
            {
                let Some(inline_body) = inline_body else {
                    // Callable references and ordinary function values use the same Lambda IR
                    // carrier, but they are not necessarily the inline callable parameter. Keep
                    // them as host operands; only a lambda carrying its checked inline body is a
                    // substitution candidate.
                    continue;
                };
                let arity = arity as usize;
                let impl_f = &self.ir.functions[impl_fn as usize];
                // The impl method's parameters are `[captures…, lambda_params…]`.
                // `arity` is the source-level lambda arity. It cannot recover this boundary after
                // the suspend pass appends a physical `Continuation` parameter. The capture list is
                // the exact boundary already carried by the IR.
                let n_cap = captures.len();
                if impl_f.params.len() < n_cap {
                    return false;
                }
                let physical_impl_params = jvm_function_params(self.ir, impl_fn);
                let cap_tys = physical_impl_params[..n_cap].to_vec();
                let lam_tys = physical_impl_params[n_cap..].to_vec();
                let semantic_signature = self
                    .ir
                    .logical_types
                    .get(&a)
                    .and_then(|ty| match ty.non_null() {
                        Ty::Fun(signature) => Some((signature.params.clone(), signature.ret)),
                        _ => None,
                    })
                    .filter(|(params, _)| params.len() == arity);
                let lam_semantic_tys = semantic_signature
                    .as_ref()
                    .map(|(params, _)| params.as_slice())
                    .unwrap_or(&impl_f.params[n_cap..]);
                crate::trace_compiler!(
                    "splice",
                    "inline lambda expression={a} impl={impl_fn} impl_params={:?} semantic={semantic_signature:?}",
                    impl_f.params
                );
                // This body is emitted in a scratch frame whose lambda-parameter slots have not been
                // installed yet. Asking `value_ty` here would read same-numbered slots from the outer
                // caller (for example, infer `it + 1` as the caller's `List`) and omit result boxing.
                // The checked expression type is stable and independent of physical slot layout.
                let body_value_ty = self
                    .ir
                    .logical_types
                    .get(&inline_body)
                    .copied()
                    .unwrap_or(impl_f.ret);
                // Each capture binds to the caller's actual slot (a mutable capture writes through).
                let mut cap_slots: Vec<(u16, Ty)> = Vec::with_capacity(captures.len());
                for (k, &cap) in captures.iter().enumerate() {
                    let slot = if let IrExpr::GetValue(v) = self.ir.expr(cap) {
                        let Some(&(slot, _)) = self.slots.get(v) else {
                            return false;
                        };
                        slot
                    } else {
                        // Left with the rest of the call's frame when the splice finishes.
                        let slot = self
                            .frame
                            .enter_temp(TempRole::LambdaCapture, cap_tys[k])
                            .slot();
                        capture_materializations.push((i, cap, slot, cap_tys[k]));
                        slot
                    };
                    cap_slots.push((slot, cap_tys[k]));
                }
                // This lambda's own locals start where the host's frame is free at the invoke, not
                // above every host local. `None` only when the body could not be decoded, in which
                // case the splice below declines too.
                let ordinal = lambda_parameters
                    .iter()
                    .position(|&parameter| parameter == i)
                    .expect("a substituted lambda is one of the collected lambda parameters");
                // Above the host's frame, and above every slot this lambda's own captures occupy:
                // a capture is live for the whole body that reads it, so a parameter placed on one
                // overwrites the value the body was given.
                let capture_ceiling = cap_slots
                    .iter()
                    .map(|&(slot, ty)| slot + slot_words(ty))
                    .max()
                    .unwrap_or(0);
                let lambda_slot_base = spliced_frame
                    .as_ref()
                    .and_then(|frame| frame.lambda_bases.get(ordinal).copied())
                    .unwrap_or(self.frame.size())
                    .max(capture_ceiling);
                // How many sites the host invokes this lambda from. One body serves them all unless
                // the body carries a suspension: then each site is a state of this machine, and a
                // state is one position with its own spill set, so the body is built once per site
                // and each copy marks its suspensions with its own ordinals. The copies are laid out
                // from the same slot base, so they differ in nothing but those ordinals — which is
                // what lets the discovery pass and the build pass number them alike.
                let sites = spliced_frame
                    .as_ref()
                    .and_then(|frame| frame.site_counts.get(ordinal).copied())
                    .unwrap_or(1)
                    .max(1);
                let copies = self.frame.mark();
                let states_before = self.machine_next_ordinal;
                let mut bodies: Vec<crate::jvm::inline::LambdaBody> = Vec::new();
                let mut lam_max_locals = 0u16;
                let mut lam_stack = 0u16;
                loop {
                    self.frame.rewind_to(copies);
                    // Build the lambda body into a scratch builder. The host left the lambda's `arity`
                    // arguments on the stack (as `Object`, the erased `FunctionN.invoke` parameters);
                    // unbox a primitive parameter, or `checkcast` a specific reference parameter to its
                    // type, then store it (top = last). Then run the body, then box the result to `Object`
                    // (matching the replaced `invoke`'s `Object` result).
                    let mut scratch = CodeBuilder::new(self.frame.size());
                    scratch.set_stack(arity as u16);
                    let mut lam_locals_declared: Vec<(u16, u16, String, String)> = Vec::new();
                    let mut lambda_slot = lambda_slot_base;
                    let mut param_slots: Vec<(u16, Ty)> = cap_slots.clone();
                    param_slots.extend(std::iter::repeat_n((0u16, Ty::Error), arity));
                    for j in (0..arity).rev() {
                        let jt = lam_tys[j];
                        if jt.is_jvm_scalar() {
                            // `FunctionN.invoke` hands every argument over as `Object`. Select its adapter
                            // from the lambda's semantic parameter before using the physical carrier for
                            // the local slot; otherwise `UInt` is mistaken for boxed `Int` here.
                            unbox_prim_from(
                                self.cw,
                                &mut scratch,
                                Ty::obj("java/lang/Object"),
                                semantic_scalar_adapter(lam_semantic_tys[j], jt),
                            );
                        } else if let Some(internal) = checkcast_internal(jt) {
                            let ci = self.cw.class_ref(&internal);
                            scratch.checkcast(ci);
                        }
                        let slot = lambda_slot;
                        lambda_slot += slot_words(jt);
                        self.frame.reserve_through(lambda_slot);
                        store(jt, slot, &mut scratch);
                        param_slots[n_cap + j] = (slot, jt);
                        // Its scope opens once the store completes, and runs to the end of the body.
                        if self.record_locals {
                            if let Some(name) = self
                                .ir
                                .fn_params
                                .get(&impl_fn)
                                .and_then(|info| info.identities.get(n_cap + j))
                                .and_then(|identity| identity.source_name.as_ref())
                            {
                                lam_locals_declared.push((
                                    u16::try_from(scratch.bytes.len()).unwrap_or(u16::MAX),
                                    slot,
                                    name.clone(),
                                    crate::jvm::names::type_descriptor(jt),
                                ));
                            }
                        }
                    }
                    // The reference compiler opens an inlined lambda body with its own inline-depth
                    // marker — `iconst_0; istore` into a `$i$a$-<callee>-<caller>` local — exactly as it
                    // opens an inlined function body with `$i$f$<callee>`. The host's marker arrives
                    // inside the relocated host body; this one has no other source, because the lambda
                    // body is emitted from IR rather than relocated.
                    let depth_marker = lambda_slot;
                    lambda_slot += 1;
                    self.frame.reserve_through(lambda_slot);
                    scratch.push_int(0, self.cw);
                    store(Ty::Int, depth_marker, &mut scratch);
                    if self.record_locals {
                        if let Some(origin) = self.ir.lambda_origins.get(&impl_fn) {
                            lam_locals_declared.push((
                                u16::try_from(scratch.bytes.len()).unwrap_or(u16::MAX),
                                depth_marker,
                                crate::jvm::debug_local_names::spliced_lambda_marker_name(
                                    callee,
                                    &self.owner,
                                    &origin.implementation_name,
                                    origin.implementation_ordinal,
                                ),
                                "I".to_string(),
                            ));
                        }
                    }
                    let body_ret =
                        self.emit_fn_body_inline(inline_body, &param_slots, &mut scratch);
                    if body_ret.is_jvm_scalar() {
                        // The erased `invoke` result is `Object`, so reverse the same semantic adapter
                        // choice after the inline body leaves its physical carrier on the stack. Use
                        // the BODY's value type, not the contextual lambda declaration return: a block
                        // accepted as `() -> Any?` can still produce a primitive `Boolean`/`Int` here.
                        box_prim_free(
                            self.cw,
                            &mut scratch,
                            semantic_scalar_adapter(body_value_ty, body_ret),
                        );
                    }
                    scratch.link_local_branches(); // enclosing-loop transfers remain owned by the caller
                    let Some(lam_insns) = crate::jvm::inline::disassemble_lambda(
                        &scratch.bytes,
                        &scratch.external_branches(),
                    ) else {
                        return false;
                    };
                    lam_max_locals = lam_max_locals.max(scratch.max_locals);
                    lam_stack = lam_stack.max(scratch.max_stack);
                    bodies.push(crate::jvm::inline::LambdaBody {
                        body: lam_insns,
                        locals: lam_locals_declared,
                        lines: scratch.line_marks().to_vec(),
                        handlers: scratch.resolved_exceptions(),
                    });
                    let suspends = self.machine_next_ordinal > states_before;
                    if !suspends || bodies.len() >= sites {
                        break;
                    }
                }
                (bodies, lam_max_locals, lam_stack)
            } else {
                continue;
            };
            if code.max_locals < lam_max_locals {
                code.max_locals = lam_max_locals;
            }
            self.frame.reserve_through(lam_max_locals);
            lam_max_stack = lam_max_stack.max(lam_stack);
            lam_splices.push(crate::jvm::inline::LambdaSplice {
                param_index: i,
                bodies,
            });
        }
        if lam_splices.is_empty() {
            return false; // no lambda argument — not this path
        }
        // Probe at offset 0. Switch padding and absolute handler/external-transfer offsets require a
        // second splice at the method's real byte offset; relative branches do not.
        let Some(probe) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            0,
            self.cw,
            reified,
        ) else {
            crate::trace_compiler!("splice", "probe declined ({descriptor})");
            return false;
        };
        let needs_relayout = probe.needs_relayout;
        // Ordinary branches preserve the caller's operand prefix and final-body dataflow computes it.
        // A handler clears that prefix, while an external transfer targets code outside the splice;
        // neither is sound until the surrounding operands have been spilled.
        let needs_empty_stack = !probe.handlers.is_empty() || !probe.external_branches.is_empty();
        if needs_empty_stack && code.stack_height() != 0 {
            crate::trace_compiler!(
                "splice",
                "unified BAIL: control transfer requires empty stack but stack_height={}",
                code.stack_height()
            );
            return false;
        }
        let ret_words = descriptor_ret_words(descriptor);
        // Emit each NON-lambda argument (the operands the host prologue stores into its parameter slots).
        let mut arg_words = 0i32;
        for (i, &a) in args.iter().enumerate() {
            if lam_splices.iter().any(|splice| splice.param_index == i) {
                for &(_, capture, slot, ty) in capture_materializations
                    .iter()
                    .filter(|(argument, ..)| *argument == i)
                {
                    self.emit_value(capture, code);
                    self.adapt_physical_operand_for(capture, self.value_ty(capture), ty, code);
                    store(ty, slot, code);
                }
                continue;
            }
            self.emit_value(a, code);
            let at = self.value_ty(a);
            self.adapt_physical_call_operand_for(call_expression, i, a, at, params[i], code);
            arg_words += slot_words(params[i]) as i32;
        }
        if !needs_relayout {
            // Position-independent host + lambda: append the probed bytes at any stack height.
            // The host's stack must cover the host body PLUS the deepest spliced lambda body (a safe upper
            // bound on the real peak) — else a deep lambda body overflows the host's operand stack.
            let ret_words = if probe.falls_through { ret_words } else { 0 };
            let splice_start = code.bytes.len();
            code.splice_inline(
                &probe.bytes,
                &probe.external_branches,
                body.max_stack + lam_max_stack + probe.stack_growth,
                top_local,
                arg_words,
                ret_words,
                probe.falls_through,
            );
            self.record_spliced_lines(&probe.lines, body, inline_only, splice_start, code);
            self.record_spliced_locals(&probe.locals, inline_only, splice_start, code);
            return true;
        }
        // RE-splice at the real method offset so any switch in the host/lambda body pads correctly.
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            splice_start,
            self.cw,
            reified,
        ) else {
            crate::trace_compiler!("splice", "probe declined ({descriptor})");
            return false;
        };
        // Register the spliced body's relocated exception handlers (try/catch/finally from `use`/
        // `synchronized`/`runCatching`). Final-body analysis derives their handler-entry frames.
        bind_inline_handlers(code, &bs.handlers);
        let ret_words = if bs.falls_through { ret_words } else { 0 };
        // Host stack must cover the host body PLUS the deepest spliced lambda body (safe upper bound).
        code.splice_inline(
            &bs.bytes,
            &bs.external_branches,
            body.max_stack + lam_max_stack + bs.stack_growth,
            top_local,
            arg_words,
            ret_words,
            bs.falls_through,
        );
        self.record_spliced_lines(&bs.lines, body, inline_only, 0, code);
        self.record_spliced_locals(&bs.locals, inline_only, 0, code);
        true
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

    /// Spill the locals that must survive suspension `ordinal`, and record which state to resume in.
    ///
    /// Emitted BEFORE the call's operands, which is also where the dependency's own stack prefix
    /// still sits: this block empties that prefix into locals, so the operands are pushed onto a
    /// clean stack and the call's own emission needs to know nothing about any of it.
    fn emit_machine_spills(&mut self, ordinal: usize, code: &mut CodeBuilder) {
        let Some(machine) = self.machine.clone() else {
            return;
        };
        let Some(suspension) = machine.plan.suspensions.get(ordinal) else {
            return;
        };
        // What the dependency left on the stack under this call goes into locals first — top value
        // into the last slot — so the call's own operands are pushed onto an empty stack and nothing
        // is lost to the `areturn` a suspension leaves through.
        for &(slot, ty) in suspension.prefix.iter().rev() {
            store(ir_ty_to_jvm(&ty), slot, code);
        }
        for (slot, ty, field, descriptor) in coroutine_machine::suspension_fields(suspension) {
            code.aload(machine.slots.continuation);
            load(ir_ty_to_jvm(&ty), slot, code);
            let reference = self.cw.fieldref(&machine.internal, &field, descriptor);
            code.putfield(reference, slot_words(ir_ty_to_jvm(&ty)) as i32);
        }
        code.aload(machine.slots.continuation);
        code.push_int(ordinal as i32 + 1, self.cw);
        let label = self.cw.fieldref(&machine.internal, "label", "I");
        code.putfield(label, 1);
    }

    /// The `COROUTINE_SUSPENDED` check that follows a suspension's call, and the state the dispatch
    /// re-enters at.
    ///
    /// The call's result is on the stack. If the callee suspended, this frame returns that sentinel
    /// and the machine is re-entered later at the marked position, which restores the spilled locals
    /// and pushes the resumed value instead. Both paths join with one value on the stack, so
    /// whatever consumes the call cannot tell which one ran.
    fn emit_machine_check(&mut self, ordinal: usize, code: &mut CodeBuilder) {
        let Some(machine) = self.machine.clone() else {
            return;
        };
        let Some(suspension) = machine.plan.suspensions.get(ordinal) else {
            return;
        };
        let join = code.new_label();
        code.dup();
        code.aload(machine.slots.suspended);
        code.if_acmpne(join);
        code.aload(machine.slots.suspended);
        code.areturn();
        // The resume point: where the dispatch's restore block re-enters, with the spills restored
        // and nothing on the stack. A resumption that failed is rethrown HERE, inside the body —
        // inside any `try` the body wraps around the suspension, which is the only place a `catch`
        // there can see the callee's exception — and a successful one pushes its value and falls
        // into the join. kotlinc lays its state out at this same position, for the same reason.
        //
        // The `areturn` above ended the stream and nothing in this builder branches here, so the
        // position is declared an external arrival. The marker names it for the enclosing method,
        // which owns both the label the restore block jumps to and the frame — one recorded in this
        // builder would be merged with the host's locals when the body is relocated, claiming locals
        // the dispatch cannot produce.
        let resume = code.new_label();
        code.bind_external_target(resume);
        code.set_stack_height(0);
        if let Ok(marker) = u16::try_from(ordinal) {
            code.coroutine_marker(crate::jvm::classfile::CoroutineMarker::Resume, marker);
        }
        let throw_on_failure =
            self.cw
                .methodref("kotlin/ResultKt", "throwOnFailure", "(Ljava/lang/Object;)V");
        code.aload(machine.slots.result);
        code.invokestatic(throw_on_failure, 1, 0);
        code.aload(machine.slots.result);
        // Where the two paths meet: the call that did not suspend, and the resume point above, both
        // with the call's result on the stack.
        code.bind(join);
        if let Ok(marker) = u16::try_from(ordinal) {
            code.coroutine_marker(crate::jvm::classfile::CoroutineMarker::Join, marker);
        }
        // Both paths meet here holding only the call's result, so the dependency's own values go
        // back on the stack AFTER the join — once, for the two of them. The code the splice wrapped
        // around this one then finds exactly what it pushed, with the result on top.
        if !suspension.prefix.is_empty() {
            let result = machine.slots.result;
            code.astore(result);
            for &(slot, ty) in &suspension.prefix {
                load(ir_ty_to_jvm(&ty), slot, code);
            }
            code.aload(result);
        }
    }

    /// Describe a spliced body's own locals in the caller's debug table.
    ///
    /// They are real locals of the method that now contains them; the reference compiler names every
    /// one. `shift` is where the splice landed for a body laid out at offset 0 — a branchless splice
    /// is appended wherever the caller happens to be — and zero for one already laid out in place.
    fn record_spliced_locals(
        &mut self,
        locals: &[(u16, u16, u16, String, String)],
        inline_only: bool,
        shift: usize,
        code: &mut CodeBuilder,
    ) {
        // An `@InlineOnly` body contributes no debug locals either, for the same reason.
        if !self.record_locals || inline_only {
            return;
        }
        for (start, length, slot, name, descriptor) in locals {
            let Ok(start) = u16::try_from(*start as usize + shift) else {
                continue;
            };
            code.add_local_entry(start, Some(*length), *slot, name, descriptor);
        }
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

    /// Splice `owner.name` whose REAL (body-fetch) descriptor is `descriptor`, mapping the body's locals
    /// per `splice_desc`. For an ordinary static they are equal; for an INSTANCE inline method spliced
    /// through this path, `splice_desc` PREPENDS the receiver as the first parameter (`this` = local 0)
    /// and `args[0]` is that receiver — so the body's `aload_0`/`aload_1`/… map to receiver/params.
    fn try_inline_static_as(
        &mut self,
        call_expression: u32,
        target: InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        code: &mut CodeBuilder,
        reified: &crate::jvm::inline::ReifiedArguments,
    ) -> bool {
        let InlineStaticTarget {
            owner,
            name,
            descriptor,
            splice_desc,
            inline_only,
            allow_owner_bridge,
        } = target;
        crate::trace_compiler!(
            "splice",
            "inline target {owner}.{name}{descriptor} splice_descriptor={splice_desc} args={}",
            args.len()
        );
        let Some(body) = self.bodies.body(owner, name, descriptor) else {
            crate::trace_compiler!("splice", "no body for {owner}.{name}{descriptor}");
            return false;
        };
        // A body that references a PRIVATE member (its own facade's helper or backing field) runs
        // legally only inside the defining class — spliced into the caller, the reference is an
        // IllegalAccessError (kotlinc rewrites it to a synthetic `access$…` bridge, which krusty
        // does not model). Decline: the fallback emits a real call, which stays in the class.
        if crate::jvm::inline::references_private_member(
            &body.code,
            &body.source_cp,
            &body.bootstrap_methods,
            &mut |o, n, d| self.bodies.member_is_private(o, n, d),
            &mut |o, n, d| self.bodies.member_is_publicly_reachable(o, n, d),
            &mut |class| self.bodies.class_is_publicly_reachable(class),
        ) {
            crate::trace_compiler!(
                "splice",
                "inaccessible relocation dependency in {owner}.{name}{descriptor}"
            );
            return false;
        }
        if !allow_owner_bridge && owner != methodref_owner(&body, name, descriptor).unwrap_or(owner)
        {
            crate::trace_compiler!(
                "splice",
                "owner-bridge mismatch for {owner}.{name}{descriptor} (real owner {:?})",
                methodref_owner(&body, name, descriptor)
            );
            return false;
        }
        // Splice the body's locals above BOTH the slot allocator's next free slot and the code's
        // high-water mark, so the spliced temporaries can never collide with a caller local (live or
        // reserved-but-unstored).
        let base = self.frame.size().max(code.max_locals);
        let inline_call = bytecode_inline_call::ClasspathInlineCall {
            call_expression,
            target: &target,
            args,
            leading_non_argument_operands,
            body: &body,
            reified,
        };
        // Route (b): a literal lambda argument → splice its body at the host's `FunctionN.invoke` site
        // (the unified host+lambda splice handles both the branchy `require(c){m}` and the branchless
        // `let`/`also`/… shapes).
        let has_lambda_arg = args.iter().any(|&argument| {
            matches!(
                self.ir.expr(argument),
                IrExpr::Lambda {
                    inline_body: Some(_),
                    ..
                }
            )
        });
        if has_lambda_arg {
            let Some(materialized_roles) = self
                .ir
                .call_materialized_lambda_params
                .get(&call_expression)
            else {
                return false;
            };
            let substitutes_literal = args.iter().enumerate().any(|(index, &argument)| {
                matches!(
                    self.ir.expr(argument),
                    IrExpr::Lambda {
                        inline_body: Some(_),
                        ..
                    }
                ) && index
                    .checked_sub(leading_non_argument_operands)
                    .and_then(|parameter| materialized_roles.get(parameter))
                    == Some(&false)
            });
            // If the body INVOKES the lambda parameter (`FunctionN.invoke`), splice the lambda body at
            // those sites. If the lambda is used only as a VALUE — passed to a call/constructor, as in the
            // `Continuation(ctx){…}` fake-constructor's `new …$Continuation$1(ctx, resumeWith)` — there is
            // no invoke site to splice into, so fall through to MATERIALIZE the lambda as a `Function1`
            // object (`emit_operands`) and splice the body verbatim (the param slot binds to that object).
            let body_invokes_lambda =
                crate::jvm::inline::disassemble(&body.code).is_some_and(|insns| {
                    !crate::jvm::inline::function_invoke_sites(&insns, &body.source_cp).is_empty()
                });
            if body_invokes_lambda && substitutes_literal {
                let materialized_roles = materialized_roles.clone();
                let route = self.lambda_call_route(&inline_call, &materialized_roles, code);
                let reason = match route {
                    Ok(bytecode_inline_call::LambdaCallRoute::MethodInliner(callee)) => {
                        if let Err(reason) =
                            self.inline_classpath_lambda_call(&inline_call, &callee, code)
                        {
                            self.run.set_inline_bail(reason);
                        }
                        return true;
                    }
                    Ok(bytecode_inline_call::LambdaCallRoute::Splice(reason)) => reason,
                    Err(reason) => {
                        self.run.set_inline_bail(reason);
                        return true;
                    }
                };
                crate::trace_compiler!("splice", "literal-lambda call spliced: {reason:?}");
                return self.try_inline_unified(
                    call_expression,
                    name,
                    inline_only,
                    splice_desc,
                    args,
                    leading_non_argument_operands,
                    &body,
                    base,
                    code,
                    reified,
                );
            }
            // A literal lambda used as a value needs kotlinc's anonymous-object regeneration
            // before MethodNode can own it. Keep only that still-unmigrated shape on the byte
            // bridge; no-lambda calls never fall back to it.
            return self
                .try_inline_materialized_lambda_body(&inline_call, code)
                .is_some();
        }
        self.try_inline_classpath_body(&inline_call, code).is_some()
    }

    /// Emit a constructor's lowered initializer block while retaining the start pc and declared
    /// source line of each property store. Lowering represents both explicit constructor-parameter
    /// stores and body-property initializers as a pure `SetField` block when it can preserve this
    /// correspondence; a mixed block is emitted atomically and contributes no reconstructed lines.
    fn emit_constructor_init_body(
        &mut self,
        class: &crate::ir::IrClass,
        init_body: crate::ir::ExprId,
        code: &mut CodeBuilder,
        lines: &mut Vec<(u16, u32)>,
    ) {
        let Some(stores) =
            crate::jvm::constructor_debug::initializer_property_stores(self.ir, class, init_body)
        else {
            self.emit(init_body, code);
            return;
        };
        for store in stores {
            let pc = code.bytes.len() as u16;
            if let Some(line) = store.line {
                lines.push((pc, line));
            }
            self.emit(store.expression, code);
        }
    }

    fn emit(&mut self, e: u32, code: &mut CodeBuilder) {
        match self.ir.expr(e).clone() {
            IrExpr::Block { stmts, value } => {
                self.link_safe_call_chain(e, code);
                // Scope block-locals: restore the slot *map* after the block so a local declared
                // here doesn't leak into a later merge-point frame (its slot must read as `Top` once
                // out of scope — else a sibling branch that never initialized it fails verification).
                let saved = self.open_slot_scope();
                let terminal_target = self.terminal_statement_target.take();
                self.emit_open_block(stmts, value, terminal_target, code);
                self.close_scope_locals(code);
                self.block_depth -= 1;
                self.restore_slot_scope(saved);
            }
            IrExpr::Return(value) => self.emit_return_node(e, value, code),
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                // A mutable captured local is represented explicitly by a `RefNew` initializer.
                // Its source type remains `ty`, while the local SLOT stores the backend's holder.
                // Representation selection belongs here; common lowering never names `Ref$IntRef`.
                let jt = init
                    .filter(|initializer| {
                        matches!(self.ir.expr(*initializer), IrExpr::RefNew { .. })
                    })
                    .map(|initializer| self.value_ty(initializer))
                    // A local declaration always owns a value slot. In particular, semantic Unit
                    // is the `kotlin/Unit` singleton here; only a callable control-flow return uses
                    // the JVM void representation.
                    .unwrap_or_else(|| ir_ty_to_jvm(&stored_value_ty(ty)));
                // Reuse the slot if this value-index is already live with a compatible verification
                // type. A spilled local is declared twice — once by the dispatch loop-top restore,
                // once by its real in-body declaration in a resume state — for the SAME value-index.
                // They must share a slot: then the loop-top restore's assignment covers the fresh path
                // too, so the slot reads as definitely-assigned in later frames. A fresh slot per
                // declaration instead leaves the in-body slot `top` on the fresh edge to a `?: continue`
                // target — a StackMapTable VerifyError (ResAgg getAllResources/getResourceById). Reuse
                // only when the verification types agree: identical, or both reference types (the
                // restore reads an `Object` continuation field and the in-body decl may be a narrower
                // reference — the wider header type still verifies every subtype back-edge). Never
                // reuse across differing primitives (e.g. an `int` slot as a `float` — same width but a
                // different verification category would pin a wrong frame type).
                let is_ref = |t: Ty| matches!(t, Ty::String | Ty::Obj(..)) || t.is_array();
                let reuse = self
                    .slots
                    .get(&index)
                    .copied()
                    .filter(|(_, ejt)| *ejt == jt || (is_ref(*ejt) && is_ref(jt)))
                    .map(|(s, _)| s);
                // The slot is entered BEFORE the initializer, as kotlinc's `visitVariable` does, so
                // the initializer's own locals and temporaries sit above it. A call operand's holder
                // is entered after its value, which kotlinc keeps on the stack or stores then.
                let holds_operand = self.ir.call_operand_bindings.contains(&e);
                let entered = reuse.or_else(|| {
                    (!holds_operand).then(|| self.enter_unassigned_value(index, jt, false))
                });
                let slot = if let Some(i) = init {
                    self.emit_value(i, code);
                    let source = self.value_ty(i);
                    let semantic = self.ir.logical_types.get(&i).copied().unwrap_or(source);
                    self.adapt_physical_operand(source, semantic, Some(ty), jt, code);
                    // kotlinc's `visitVariable` marks the initializer's line, then the
                    // declaration's, before the store (after an inlined call, both are written).
                    debug_lines::mark_expression_start(self.ir, i, code);
                    debug_lines::mark_statement(self.ir, e, code);
                    let slot = entered
                        .unwrap_or_else(|| self.enter_unassigned_value(index, jt, holds_operand));
                    self.unassigned_values.remove(&index);
                    store(jt, slot, code);
                    slot
                } else {
                    self.unassigned_values.insert(index);
                    entered.unwrap_or_else(|| self.enter_unassigned_value(index, jt, holds_operand))
                };
                // A re-declared value takes its type from this declaration once it is initialized.
                self.slots.insert(index, (slot, jt));
                // A source local becomes visible after its initializing store. An uninitialized
                // one (`lateinit var`) still has a lexical lifetime: its declaration emits no
                // store, so its debug range opens here, and the later checked assignment only
                // initializes the already-live slot.
                if let Some(name) = self
                    .record_locals
                    .then(|| super::debug_local_names::name(self.ir, e))
                    .flatten()
                {
                    if code.bytes.len() <= u16::MAX as usize {
                        self.open_locals.push((
                            self.block_depth,
                            slot,
                            code.bytes.len() as u16,
                            name,
                            local_variable_desc(jt),
                        ));
                    }
                }
            }
            IrExpr::SetValue { var, value } => {
                let Some(&(slot, jt)) = self.slots.get(&var) else {
                    self.run.set_emit_error(
                        "assignment references a value slot that was never declared".to_string(),
                    );
                    return;
                };
                match local_updates::iinc_delta(self.ir, var, value, jt) {
                    Some(delta) => code.iinc(slot, delta),
                    _ => {
                        self.emit_value(value, code);
                        // Coerced to the slot's type as the initializer is: a value of another
                        // class is cast to the declared one, which is what a join of the two
                        // stores reads back.
                        self.adapt_physical_operand_for(value, self.value_ty(value), jt, code);
                        store(jt, slot, code);
                    }
                }
                self.unassigned_values.remove(&var);
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => self.emit_set_field(e, receiver, class, index, value, code),
            IrExpr::SetStatic { index, value } => {
                let s = &self.ir.statics[index as usize];
                let jt = jvm_declared_ty(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                self.emit_value(value, code);
                if self.diverges(value) {
                    return;
                }
                // Within the facade write the field directly; from another class go through `setX()` —
                // or, for a PRIVATE top-level property (no public setter), the `access$set<X>$p` bridge.
                let private = self.ir.statics[index as usize].visibility.is_private();
                // A static declaring an OWNER lives on that class (a companion property is a static
                // field on the outer class). Within the owner write the (private) field directly;
                // from another class — the companion's delegating setter — go through the owner's
                // PUBLIC synthetic `access$set<X>$cp` bridge (kotlinc's hoisted-companion shape).
                // A `@JvmField` static is a PUBLIC field with no bridges: every writer goes
                // `putstatic` directly.
                if let Some(owner) = self.ir.statics[index as usize].owner {
                    let owner_name = owner.render();
                    if self.owner == owner_name
                        || !self.ir.is_jvm_companion_hoisted_static(index)
                        || self.ir.is_jvm_field_static(index)
                    {
                        let fref = self.cw.fieldref(&owner_name, &name, &type_descriptor(jt));
                        code.putstatic(fref, slot_words(jt) as i32);
                    } else {
                        let m = self.cw.methodref(
                            &owner_name,
                            &format!("access${}$cp", property_setter_name(&name)),
                            &format!("({})V", type_descriptor(jt)),
                        );
                        code.invokestatic(m, slot_words(jt) as i32, 0);
                    }
                } else if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
                    code.putstatic(fref, slot_words(jt) as i32);
                } else {
                    let sname = if private {
                        format!("access${}$p", property_setter_name(&name))
                    } else {
                        property_setter_name(&name)
                    };
                    let m =
                        self.cw
                            .methodref(&facade, &sname, &format!("({})V", type_descriptor(jt)));
                    code.invokestatic(m, slot_words(jt) as i32, 0);
                }
            }
            IrExpr::Checked(_) => {
                unreachable!("checked operation passed jvm_can_emit without a JVM realization")
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => {
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.bind(start, code);
                // A pre-test loop checks the condition before the body; a `do…while` skips this and
                // tests at the bottom (`cont`), so the body always runs once.
                if !post_test && self.emit_cond_branch(cond, end, false, code) {
                    // `while (false)`: the jump-out is unconditional, so the body/update/back-edge
                    // that would follow are unreachable — emitting them leaves frameless dead code
                    // the verifier rejects. kotlinc emits no body for a never-entered loop either.
                    self.bind(end, code);
                    return;
                }
                // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
                //
                // A PRE-TEST loop with no update has nothing at the bottom but the back edge, so
                // `cont` would be a jump to a jump: `continue` reaches the condition either way, and
                // kotlinc branches to it directly. Using the condition itself as the continue target
                // removes the hop, and leaves the back edge below reachable only by falling out of
                // the body — where a body that always jumps makes it dead, which is what kotlinc
                // emits for a loop whose every path continues or breaks.
                let bottom = if post_test || update.is_some() {
                    cont
                } else {
                    start
                };
                self.loop_stack
                    .push((bottom, end, label.clone(), self.return_finalizers.len()));
                let enclosing_terminal_target = self.terminal_statement_target.replace(bottom);
                let retained_body_scope = if post_test {
                    match self.ir.expr(body).clone() {
                        // Kotlin's `do` body and bottom condition share one lexical scope. Keep the
                        // body's exact local-slot map alive until after the condition; closing the
                        // ordinary block here would make a legal `do { val x = ... } while (x ...)`
                        // read an undeclared value. `post_test` is the checked common-IR fact that
                        // authorizes this lifetime, so no source-shape lookup is involved.
                        IrExpr::Block { stmts, value } => {
                            let saved = self.open_slot_scope();
                            self.emit_open_block(stmts, value, Some(bottom), code);
                            Some(saved)
                        }
                        _ => {
                            self.emit(body, code);
                            None
                        }
                    }
                } else {
                    self.emit(body, code);
                    None
                };
                self.terminal_statement_target = enclosing_terminal_target;
                // A pre-test body's block has restored its slot map. A post-test body's block stays
                // open here because `continue` and the condition are inside that same Kotlin scope.
                if bottom == cont {
                    self.bind(cont, code);
                }
                // The update is part of the loop, so it keeps the `break`/`continue` scope active — the
                // non-overflowing counted loop puts its `if (i == end) break` here (before the increment)
                // so a `continue` lands on it too, instead of skipping straight to the wrapping `i++`.
                if let Some(u) = update {
                    self.emit(u, code);
                }
                self.loop_stack.pop();
                if post_test {
                    // `do…while`: loop back while the condition holds, then fall through to `end`.
                    // A `while (true)` back-edge IS unconditional, and the only thing after it is the
                    // `frame(end)`/`bind(end)` below — which is exactly what a dead-but-framed `end`
                    // needs, so the flag is deliberately ignored here. Anything emitted after this
                    // point in future would have to honour it.
                    let _ = self.emit_cond_branch(cond, start, true, code);
                    if let Some(saved) = retained_body_scope {
                        self.close_scope_locals(code);
                        self.block_depth -= 1;
                        self.restore_slot_scope(saved);
                    }
                } else {
                    code.goto(start);
                }
                self.bind(end, code);
            }
            IrExpr::Break { label } => self.emit_loop_transfer(&label, true, code),
            IrExpr::Continue { label } => self.emit_loop_transfer(&label, false, code),
            other => {
                self.emit_discarding_node(e, &other, code);
            }
        }
    }

    fn emit_value(&mut self, e: u32, code: &mut CodeBuilder) {
        debug_lines::mark_expression_start(self.ir, e, code);
        // A suspension whose machine emission owns: mark where it landed. The splice decides that
        // position, so an offset recorded before it would be worthless, whereas an instruction
        // travels with the code. Every marker is erased once its answers are read.
        let suspension = self.machine_before(e, code);
        self.open_transformed_suspension(e, code);
        let node = self.ir.expr(e).clone();
        self.emit_value_node(e, &node, code);
        self.machine_after(suspension, code);
        self.close_transformed_suspension(e, false, code);
    }

    /// Open a suspension this emission's machine owns, if `e` is one.
    ///
    /// Both the value and the discarding path go through this: a suspension whose result is thrown
    /// away — `api.stop(id)` as a statement — is a state of the machine like any other, and one
    /// that never spilled would resume into a frame the dispatch cannot produce.
    fn machine_before(&mut self, e: u32, code: &mut CodeBuilder) -> Option<usize> {
        if !self.machine_suspensions.contains(&e) {
            return None;
        }
        let ordinal = self.machine_next_ordinal;
        self.machine_next_ordinal += 1;
        match self.machine.is_some() {
            // Building the machine: spill first, then the call, then the check.
            true => self.emit_machine_spills(ordinal, code),
            // Discovering the frame: mark where the splice put this suspension. The discovery pass
            // reads everything else — the locals held here and what is on the stack under the
            // call — off the finished bytecode.
            false => {
                if let Ok(marker) = u16::try_from(ordinal) {
                    code.coroutine_marker(
                        crate::jvm::classfile::CoroutineMarker::Suspension,
                        marker,
                    );
                }
            }
        }
        Some(ordinal)
    }

    /// Close a suspension opened by [`Self::machine_before`].
    fn machine_after(&mut self, suspension: Option<usize>, code: &mut CodeBuilder) {
        if let (Some(ordinal), true) = (suspension, self.machine.is_some()) {
            self.emit_machine_check(ordinal, code);
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
            if let Some(access) = access {
                return self.emit_realized_property_read(
                    operation.expression,
                    operation.receiver,
                    access,
                    operation.ty,
                    code,
                );
            }
        }
        let array_realization = receiver_ty.and_then(|receiver_ty| {
            jvm_array_actual_realization(
                crate::types::type_name(operation.owner),
                operation.name,
                receiver_ty,
                &[],
                declaration_ty,
            )
        });
        crate::trace_compiler!(
            "emit",
            "property read owner={} name={} receiver={receiver_ty:?} declaration={:?} array_realization={array_realization:?}",
            operation.owner,
            operation.name,
            declaration_ty,
        );
        if array_realization == Some(JvmArrayActualRealization::Size) {
            self.emit_value(
                operation
                    .receiver
                    .expect("array property reads have a dispatch receiver"),
                code,
            );
            code.arraylength();
            return;
        }
        if let Some(access) = self
            .ir
            .property_external_accessors
            .get(&operation.expression)
            .and_then(|accessor| self.bodies.external_property_access(*accessor))
        {
            return self.emit_realized_property_read(
                operation.expression,
                operation.receiver,
                access,
                operation.ty,
                code,
            );
        }
        // A source declaration from this compilation supplies the invocation shape; a checker-selected
        // spelling refines only its otherwise-conventional accessor name.
        if let Some(access) = self.declared_property_read_access(
            operation.owner,
            operation.name,
            selected.map(|(name, _)| name.as_str()),
            operation.interface,
        ) {
            return self.emit_realized_property_read(
                operation.expression,
                operation.receiver,
                access,
                operation.ty,
                code,
            );
        }
        // A loaded classfile supplies the authoritative JVM realization, including static value-class
        // accessors. The selected spelling intentionally carries no duplicate invocation shape.
        if let Some(access) = self
            .bodies
            .property_read_access(operation.owner, operation.name)
        {
            return self.emit_realized_property_read(
                operation.expression,
                operation.receiver,
                access,
                operation.ty,
                code,
            );
        }
        let access = PropertyAccess::Accessor {
            owner: operation.owner.to_string(),
            // A sibling source class has no classfile in `bodies`, so this is the only realization
            // that cannot read the exact JVM accessor spelling from a declaration. Exact selected
            // spellings returned above; an unstamped ordinary property keeps Kotlin's convention.
            // The semantic IR node itself remains target-neutral.
            name: stamped
                .map(|(name, _)| name.clone())
                .or_else(|| selected.map(|(name, _)| name.clone()))
                .unwrap_or_else(|| crate::names::property_getter_name(operation.name)),
            // The logical property type is intentionally retained on the node for the surrounding
            // expression. Its call boundary instead uses the most specific declaration fact: a JVM
            // value-class realization when present, otherwise the semantic declaration type. Without
            // that split a generic `getA(): Object` can be called as `getA(): A`, or a mangled
            // `getId-…(): String` as `getId-…(): Id`; both are invalid descriptors.
            descriptor: method_descriptor(
                &[],
                ir_ty_to_jvm(&stored_value_ty(*physical.unwrap_or(operation.ty))),
            ),
            is_static: false,
            // Resolution carries source-module shape because a sibling class is not in `bodies`.
            // For classpath owners the body reader remains authoritative.
            is_interface: operation.interface || self.bodies.owner_is_interface(operation.owner),
        };
        self.emit_realized_property_read(
            operation.expression,
            operation.receiver,
            access,
            operation.ty,
            code,
        )
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
            .and_then(|realization| match realization {
                crate::jvm::property_realizations::PropertyRealization::Physical(access) => {
                    Some(access.clone())
                }
                crate::jvm::property_realizations::PropertyRealization::Local(target) => {
                    self.local_property_write_access(*target)
                }
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
                owner: operation.owner.to_string(),
                name: stamped
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| crate::names::property_setter_name(operation.name)),
                descriptor: method_descriptor(
                    &[ir_ty_to_jvm(physical.unwrap_or(operation.ty))],
                    Ty::Unit,
                ),
                is_static: false,
                is_interface: operation.interface
                    || self.bodies.owner_is_interface(operation.owner),
            });
        let access_owner = match &access {
            PropertyAccess::Field { owner, .. }
            | PropertyAccess::Accessor { owner, .. }
            | PropertyAccess::AccessBridge { owner, .. } => owner.clone(),
        };
        let takes_receiver = accessor_takes_receiver(&access);
        // Keep the reference compiler's evaluation layout for a branchy assigned value: spill BOTH
        // operands in source order (receiver, then value), then reload them. Spilling only the value
        // would reverse observable side effects. Final-body analysis could verify a live receiver
        // prefix, but changing this layout would lose bytecode parity.
        let spilled = if takes_receiver && self.emits_control_flow(value) {
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
            self.narrow_on_stack(receiver_ty, Ty::obj(&access_owner), code);
        } else if let Some(receiver) = operation.receiver {
            let receiver_ty = accessor_receiver_ty(&access, &access_owner);
            self.emit_property_receiver(
                receiver,
                &access_owner,
                takes_receiver,
                &receiver_ty,
                code,
            );
        } else if takes_receiver {
            self.run.set_emit_error(format!(
                "receiver-less property realization requires an instance receiver: {access_owner}"
            ));
            return;
        }
        // The assigned value is bridged to what the realization stores, the mirror of the read's bridge.
        let target = match &access {
            PropertyAccess::Field { descriptor, .. } => ty_from_field_descriptor(descriptor),
            PropertyAccess::Accessor { descriptor, .. } => {
                crate::jvm::names::parse_method_descriptor(descriptor)
                    .and_then(|(params, _)| params.first().map(|p| ty_from_field_descriptor(p)))
                    .unwrap_or_else(|| ir_ty_to_jvm(operation.ty))
            }
            PropertyAccess::AccessBridge { descriptor, .. } => {
                crate::jvm::names::parse_method_descriptor(descriptor)
                    .and_then(|(params, _)| params.last().map(|p| ty_from_field_descriptor(p)))
                    .unwrap_or_else(|| ir_ty_to_jvm(operation.ty))
            }
        };
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
            } => {
                let jt = ty_from_field_descriptor(&descriptor);
                let fref = self.cw.fieldref(&owner, &name, &descriptor);
                if is_static {
                    code.putstatic(fref, slot_words(jt) as i32);
                } else {
                    code.putfield(fref, slot_words(jt) as i32);
                }
            }
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static,
                is_interface,
            } => {
                let words = crate::jvm::names::parse_method_descriptor(&descriptor)
                    .map(|(params, _)| {
                        params
                            .iter()
                            .map(|p| slot_words(ty_from_field_descriptor(p)) as i32)
                            .sum()
                    })
                    .unwrap_or_else(|| slot_words(target) as i32);
                let m = if is_interface {
                    self.cw.interface_methodref(&owner, &name, &descriptor)
                } else {
                    self.cw.methodref(&owner, &name, &descriptor)
                };
                // The write through an ACCESSOR is a dispatch, so the assignment's own line returns
                // here, after the value expression has marked its. A write realized as a FIELD is
                // not one and marks nothing.
                self.mark_dispatch_line(operation.expression, code);
                if is_static {
                    code.invokestatic(m, words, 0);
                } else if is_interface {
                    code.invokeinterface(m, words, 0);
                } else {
                    code.invokevirtual(m, words, 0);
                }
            }
            PropertyAccess::AccessBridge {
                owner,
                name,
                descriptor,
            } => {
                // Receiver + value are both arguments of the synthetic static.
                let words = crate::jvm::names::parse_method_descriptor(&descriptor)
                    .map(|(params, _)| {
                        params
                            .iter()
                            .map(|p| slot_words(ty_from_field_descriptor(p)) as i32)
                            .sum()
                    })
                    .unwrap_or(2);
                let m = self.cw.methodref(&owner, &name, &descriptor);
                self.mark_dispatch_line(operation.expression, code);
                code.invokestatic(m, words, 0);
            }
        }
    }

    /// Choose the JVM storage for an already-resolved semantic singleton. Source declarations expose
    /// their semantic object/companion shape in IR; dependency layout comes from classfile metadata.
    fn singleton_storage(
        &self,
        classifier: crate::types::TypeName,
    ) -> Option<(crate::types::TypeName, String)> {
        if let Some(declaration) = self
            .ir
            .referenced_module_classifiers
            .get(&classifier)
            .copied()
        {
            if !declaration.singleton {
                return None;
            }
            return Some(if let Some(owner) = declaration.companion_owner {
                (owner, classifier.nested_segment_ref().to_owned())
            } else {
                (classifier, "INSTANCE".to_string())
            });
        }
        if let Some(class) = self
            .ir
            .classes
            .iter()
            .find(|class| class.fq_name == classifier && class.is_singleton())
        {
            return Some(if class.is_companion {
                (
                    classifier.nested_owner()?,
                    classifier.nested_segment_ref().to_owned(),
                )
            } else {
                (classifier, "INSTANCE".to_string())
            });
        }
        self.bodies.singleton_storage(classifier)
    }

    /// Emit the source receiver according to an already-selected property realization. Instance
    /// accessors/fields consume it; a static realization still evaluates and drops an effectful receiver.
    /// Reads and writes share this exact rule so `side().p` and `side().p = v` cannot diverge.
    fn emit_property_receiver(
        &mut self,
        receiver: crate::ir::ExprId,
        access_owner: &str,
        takes_receiver: bool,
        expected: &Ty,
        code: &mut CodeBuilder,
    ) {
        if takes_receiver {
            self.emit_value(receiver, code);
            self.narrow_on_stack(self.value_ty(receiver), *expected, code);
            return;
        }
        // A receiverless realization does not make the receiver expression disappear. Elide only an
        // expression that runs no code, or a singleton/static read of the very owner whose static access
        // initializes it anyway; every other receiver is evaluated and popped.
        let initializes_owner = match self.ir.expr(receiver) {
            IrExpr::SingletonValue { classifier } => self
                .singleton_storage(*classifier)
                .is_some_and(|(owner, _)| owner.matches(access_owner)),
            IrExpr::ExternalStaticField { owner, .. }
            | IrExpr::ExternalStaticInstance { owner, .. } => owner.matches(access_owner),
            IrExpr::StaticInstance { owner, .. } => self
                .ir
                .classes
                .get(*owner as usize)
                .is_some_and(|class| class.fq_name_matches(access_owner)),
            _ => false,
        };
        if !crate::ir::expr_runs_no_code(self.ir, receiver) && !initializes_owner {
            self.emit_value(receiver, code);
            code.pop();
        }
    }

    /// Select the JVM access for an active-source property by the exact declaration coordinate
    /// retained from checked FIR. Companion storage may already have been moved to the outer class;
    /// consume that backend table directly instead of recovering the property from its spelling.
    fn local_property_read_access(
        &self,
        target: crate::fir::PropertyId,
        selected_accessor: Option<&str>,
        selected_interface: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            property,
            name,
            ..
        } = self.ir.local_property_layouts.get(&target)?
        else {
            return None;
        };
        if let Some(access) = self.hoisted_jvm_field_access(*class, *property) {
            return Some(access);
        }
        debug_assert_eq!(self.ir.classes[*class as usize].fq_name, *owner);
        self.declared_property_read_access(
            &owner.render(),
            name,
            selected_accessor,
            selected_interface,
        )
    }

    fn local_property_write_access(
        &self,
        target: crate::fir::PropertyId,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            property,
            name,
            ..
        } = self.ir.local_property_layouts.get(&target)?
        else {
            return None;
        };
        if let Some(access) = self.hoisted_jvm_field_access(*class, *property) {
            return Some(access);
        }
        debug_assert_eq!(self.ir.classes[*class as usize].fq_name, *owner);
        self.declared_property_write_access(&owner.render(), name)
    }

    fn hoisted_jvm_field_access(
        &self,
        class: crate::ir::ClassId,
        property: u32,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let class = self.ir.classes.get(class as usize)?;
        let static_id = self
            .ir
            .jvm_companion_property_static(class.fq_name, property)?;
        if !self.ir.is_jvm_field_static(static_id) {
            return None;
        }
        let field = self.ir.statics.get(static_id as usize)?;
        Some(crate::jvm::inline::PropertyAccess::Field {
            owner: field.owner?.render(),
            name: field.name.clone(),
            descriptor: type_descriptor(jvm_declared_ty(&field.ty)),
            is_static: true,
        })
    }

    /// The write analogue of [`Self::declared_property_read_access`].
    fn declared_property_write_access(
        &self,
        owner: &str,
        name: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        let class = self.ir.classes.iter().find(|c| c.fq_name_matches(owner))?;
        // The write analogue: a declared setter is user code and must not be bypassed.
        let declared = class.properties.iter().find(|p| p.name == name);
        let direct_field = self.direct_field_access(class, declared, true);
        if let Some(declared) = declared.filter(|p| p.needs_access_bridge && self.owner != owner) {
            let ty = declared
                .backing_field
                .and_then(|i| class.fields.get(i as usize))
                .map_or(declared.ty, |f| f.ty);
            let d = type_descriptor(jvm_declared_ty(&ty));
            return Some(PropertyAccess::AccessBridge {
                owner: owner.to_string(),
                name: format!("access${}$p", crate::names::property_setter_name(name)),
                descriptor: format!("(L{owner};{d})V"),
            });
        }
        if let Some(setter) = declared.and_then(|p| p.setter) {
            let f = &self.ir.functions[setter as usize];
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: f.name.clone(),
                descriptor: method_descriptor(&[jvm_declared_ty(&f.params[0])], Ty::Unit),
                is_static: false,
                is_interface: is_jvm_interface(class),
            });
        }
        let field = class
            .fields
            .iter()
            .find(|f| f.name == name)
            .filter(|_| declared.is_none_or(|p| p.backing_field.is_some()));
        let setter_name = declared
            .and_then(|p| p.setter_jvm_name.clone())
            .unwrap_or_else(|| crate::names::property_setter_name(name));
        let setter = class.methods.iter().find_map(|&fid| {
            let f = &self.ir.functions[fid as usize];
            let named = f.name == setter_name
                || f.name
                    .strip_prefix(&setter_name)
                    .is_some_and(|rest| rest.starts_with('-'));
            (named && f.params.len() == 1).then_some(f)
        });
        // Outside the declaring class the backing field is private, so the write goes through the setter
        // — and a property with no backing field at all (a custom setter, a delegated one) is written
        // through it from anywhere.
        let accessor = |f: &crate::ir::IrFunction| PropertyAccess::Accessor {
            owner: owner.to_string(),
            name: f.name.clone(),
            descriptor: method_descriptor(&[jvm_declared_ty(&f.params[0])], Ty::Unit),
            is_static: false,
            is_interface: is_jvm_interface(class),
        };
        if let Some(setter) = setter.filter(|_| !direct_field || field.is_none()) {
            return Some(accessor(setter));
        }
        let field = field?;
        if !direct_field {
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: setter_name,
                descriptor: method_descriptor(&[jvm_declared_ty(&field.ty)], Ty::Unit),
                is_static: false,
                is_interface: is_jvm_interface(class),
            });
        }
        Some(PropertyAccess::Field {
            owner: owner.to_string(),
            name: instance_field_jvm_name(self.ir, class, field),
            descriptor: type_descriptor(jvm_declared_ty(&field.ty)),
            // A static-storage object's backing fields are JVM statics (kotlinc's shape).
            is_static: static_storage(self.ir, class),
        })
    }

    /// How to read property `name` of a class THIS compilation declares — there is no class file to ask,
    /// the IR is the declaration. Inside the declaring class the private backing field is loaded directly,
    /// which is what kotlinc emits there; from outside, the read goes through the accessor. `None` when
    /// `owner` is not a class of this file, or declares no such property.
    fn declared_property_read_access(
        &self,
        owner: &str,
        name: &str,
        selected_accessor: Option<&str>,
        selected_interface: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        let class = self.ir.classes.iter().find(|c| c.fq_name_matches(owner))?;
        let interface = is_jvm_interface(class) || selected_interface;
        // A property that DECLARES an accessor (computed, delegated, or `field`-using) is always read
        // through it — the accessor is user code, and a direct field load would skip it. Only a plain
        // backing-field property may be read directly, and only from inside the declaring class.
        let declared = class.properties.iter().find(|p| p.name == name);
        let direct_field = self.direct_field_access(class, declared, false);
        if let Some(getter) = declared.and_then(|p| p.getter) {
            let f = &self.ir.functions[getter as usize];
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: if class.is_annotation {
                    name.to_string()
                } else {
                    f.name.clone()
                },
                descriptor: ir_method_desc(&f.params, &f.ret),
                is_static: f.is_static,
                is_interface: interface,
            });
        }
        let field = class
            .fields
            .iter()
            .find(|f| f.name == name)
            .filter(|_| declared.is_none_or(|p| p.backing_field.is_some()));
        // A declaration-specified JVM name wins; otherwise the checker's selected accessor identity
        // refines the naming convention. Backend value-class mangling lives in a different table and
        // therefore cannot overwrite an inherited generic declaration here.
        let accessor_name = if class.is_annotation {
            name.to_string()
        } else {
            declared
                .and_then(|p| p.getter_jvm_name.clone())
                .or_else(|| selected_accessor.map(str::to_string))
                .unwrap_or_else(|| crate::names::property_getter_name(name))
        };
        // The accessor's descriptor comes from the accessor ITSELF, not from the field: an accessor may
        // return something the field's declared type does not spell (an erased generic, a value class's
        // underlying), and a descriptor built from the wrong one is a `NoSuchMethodError` at run time.
        // A value-class-typed property's accessor is `@JvmName`-mangled (`getId-<hash>`), so match the
        // mangled spelling too — the alternative is falling through to a private backing field, which is
        // an `IllegalAccessError` from anywhere but the declaring class.
        let accessor = class.methods.iter().find_map(|&fid| {
            let f = &self.ir.functions[fid as usize];
            let named = f.name == accessor_name
                || f.name
                    .strip_prefix(&accessor_name)
                    .is_some_and(|rest| rest.starts_with('-'));
            let getter_shape = f.params.is_empty()
                || (f.is_static && f.name.ends_with("-impl") && f.params.len() == 1);
            (named && getter_shape).then_some(f)
        });
        if let Some(accessor) = accessor.filter(|_| !direct_field || field.is_none()) {
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: accessor.name.clone(),
                descriptor: ir_method_desc(&accessor.params, &accessor.ret),
                is_static: accessor.is_static,
                is_interface: interface,
            });
        }
        // A private property reached from outside its class goes through the synthetic bridge; there is no
        // accessor and the field itself is unreachable.
        if let Some(declared) = declared.filter(|p| p.needs_access_bridge && self.owner != owner) {
            let ty = declared
                .backing_field
                .and_then(|i| class.fields.get(i as usize))
                .map_or(declared.ty, |f| f.ty);
            return Some(PropertyAccess::AccessBridge {
                owner: owner.to_string(),
                name: format!("access${}$p", crate::names::property_getter_name(name)),
                descriptor: format!("(L{owner};){}", type_descriptor(jvm_declared_ty(&ty))),
            });
        }
        // A class of THIS compilation is answered from its declaration, always — never by falling through
        // to the naming-convention guess, which has no class file to ask and would mistake an interface
        // for a class (`invokevirtual` on an interface is an `IncompatibleClassChangeError`).
        let Some(field) = field else {
            let ty = declared.map(|p| p.ty)?;
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: accessor_name,
                descriptor: ir_method_desc(&[], &stored_value_ty(ty)),
                is_static: false,
                is_interface: interface,
            });
        };
        // Outside the declaring class the backing field is private, so the read goes through the
        // accessor — the one synthesized for this declaration, which carries no IR method of its own.
        if !direct_field {
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: accessor_name,
                descriptor: method_descriptor(
                    &[],
                    declared
                        .map(|property| declared_property_accessor_jvm(self.ir, property, field))
                        .unwrap_or_else(|| jvm_declared_ty(&field.ty)),
                ),
                is_static: false,
                is_interface: interface,
            });
        }
        Some(PropertyAccess::Field {
            owner: owner.to_string(),
            name: instance_field_jvm_name(self.ir, class, field),
            descriptor: type_descriptor(jvm_declared_ty(&field.ty)),
            // A static-storage object's backing fields are JVM statics (kotlinc's shape).
            is_static: static_storage(self.ir, class),
        })
    }

    /// Whether a property of `owner` may be reached as its raw backing FIELD from the class currently
    /// being emitted. Only inside the declaring class (the field is private everywhere else) — and only
    /// for a FINAL property. An `open`/`override` property is redeclared by subclasses, which replace its
    /// ACCESSOR, not the base's own private storage: a `getfield` from a base method would read the
    /// base's field and silently bypass the override. kotlinc emits `invokevirtual get<Name>()` inside
    /// the class for exactly that reason, so the accessor is the only correct realization here.
    ///
    /// Two exemptions, both because the accessor an `open` property would be reached through does not
    /// exist:
    ///
    /// * a PRIVATE property has no synthesized accessor at all (kotlinc reads it directly in-class).
    ///   `private open` is not valid Kotlin — kotlinc reports "'open' is incompatible with 'private'"
    ///   — so this only decides what an input krusty accepts but kotlinc rejects compiles to, and the
    ///   raw field is the realization that at least links.
    /// * a `val` has no SETTER, so a `writable` access to one can only be the deferred initialization
    ///   Kotlin permits in a constructor/`init` block, which kotlinc also emits as a `putfield`.
    ///
    /// A `@JvmField` property is reachable this way from ANY class: it has no accessor to call, and
    /// its field carries the declaration's own visibility rather than Kotlin's default `private`.
    fn direct_field_access(
        &self,
        class: &crate::ir::IrClass,
        declared: Option<&crate::ir::IrProperty>,
        writable: bool,
    ) -> bool {
        if declared.is_some_and(|p| is_jvm_field(class, &p.name)) {
            return true;
        }
        class.fq_name_matches(&self.owner)
            && !declared.is_some_and(|p| p.is_open && !p.is_private && (!writable || p.is_var))
    }

    /// Select the property-read realization available from declarations emitted by this compilation.
    /// `None` deliberately means the external bytecode-provider path must decide.
    fn selected_local_property_read_access(
        &self,
        owner: &str,
        name: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        self.declared_property_read_access(owner, name, None, false)
    }

    /// Is `owner.name` a `lateinit` backing field of a class THIS compilation is emitting? Only such a
    /// field carries the inline uninitialized guard, so the read emission and [`Self::emits_control_flow`]
    /// must answer this one question the same way — a disagreement is a `VerifyError` at link time.
    fn is_lateinit_field(&self, owner: &str, name: &str) -> bool {
        self.ir
            .classes
            .iter()
            .find(|c| c.fq_name_matches(owner))
            .and_then(|c| c.fields.iter().find(|f| f.name == name))
            .is_some_and(|f| f.is_lateinit())
    }

    /// Does this property read realize as a DIRECT FIELD load of a `lateinit` backing field — the one
    /// read shape that emits the guard INLINE rather than hiding it inside a getter body? Mirrors the
    /// realization [`Self::emit_property_read`] picks through
    /// [`Self::selected_local_property_read_access`]. Everything past that helper is an external-provider
    /// property, whose owner is not a class being emitted here.
    fn lateinit_direct_field_read(&self, owner: &str, name: &str) -> bool {
        use crate::jvm::inline::PropertyAccess;
        let Some(access) = self.selected_local_property_read_access(owner, name) else {
            return false;
        };
        matches!(&access, PropertyAccess::Field { owner, name, .. }
            if self.is_lateinit_field(owner, name))
    }

    fn emit_value_node(&mut self, e: u32, node: &IrExpr, code: &mut CodeBuilder) {
        match node {
            IrExpr::BottomValue { producer, .. } => {
                let baseline = code.stack_height();
                self.emit_value(*producer, code);
                bottom_values::finish(self.cw, code, baseline, true);
            }
            // `break`/`continue` are `Nothing`-typed: in value position (e.g. `x ?: break`) they diverge
            // — emit the jump and push nothing; the consuming branch is dead past this point.
            IrExpr::Break { label } => {
                self.emit_loop_transfer(label, true, code);
            }
            IrExpr::Continue { label } => {
                self.emit_loop_transfer(label, false, code);
            }
            IrExpr::Const(c) => match c {
                IrConst::Boolean(b) => code.push_int(if *b { 1 } else { 0 }, self.cw),
                IrConst::Int(v) => code.push_int(*v, self.cw),
                // THE representation decision for a narrow unsigned constant, and it is this
                // backend's to make. `UByte` is a value class over `Byte`, so the JVM carries
                // it in a `B`: the value 200 is pushed as the byte -56, which is what kotlinc
                // emits (`bipush -56`) and what a `(B)` parameter and `constructor-impl` both
                // expect. Pushing the untruncated 200 made two equal `UByte` values compare
                // unequal, because only one side had been through a narrowing.
                //
                // Common IR hands over the VALUE and the unsigned identity; the carrier is
                // chosen here, and another backend is free to choose differently.
                IrConst::UByte(v) => code.push_int(i32::from(*v as i8), self.cw),
                IrConst::UShort(v) => code.push_int(i32::from(*v as i16), self.cw),
                IrConst::UInt(v) => code.push_int(*v as i32, self.cw),
                IrConst::ULong(v) => code.push_long(*v as i64, self.cw),
                IrConst::Short(v) => code.push_int(*v as i32, self.cw),
                IrConst::Byte(v) => code.push_int(*v as i32, self.cw),
                IrConst::Char(v) => code.push_int(*v as i32, self.cw),
                IrConst::Long(v) => code.push_long(*v, self.cw),
                IrConst::Double(v) => code.push_double(*v, self.cw),
                IrConst::Float(v) => code.push_float(*v, self.cw),
                IrConst::String(s) => code.push_string_kt(s, self.cw),
                IrConst::Null => code.aconst_null(),
            },
            IrExpr::ClassConst { internal } => {
                let name = internal
                    .as_ref()
                    .map_or_else(|| self.facade.clone(), |name| name.render());
                code.ldc_class(&name, self.cw);
            }
            IrExpr::KClassLiteral { classifier, value } => {
                match (classifier, value) {
                    (Some(classifier), None) => {
                        let descriptor = boxed_descriptor(*classifier);
                        let internal = descriptor
                            .strip_prefix('L')
                            .and_then(|descriptor| descriptor.strip_suffix(';'))
                            .unwrap_or(&descriptor);
                        code.ldc_class(internal, self.cw);
                    }
                    (None, Some(value)) => {
                        self.emit_value(*value, code);
                        self.box_scalar_operand(self.value_ty(*value), code);
                        let get_class = self.cw.methodref(
                            "java/lang/Object",
                            "getClass",
                            "()Ljava/lang/Class;",
                        );
                        code.invokevirtual(get_class, 0, 1);
                    }
                    _ => {
                        self.run.set_emit_error(
                            "checked class literal has an invalid operand shape".to_string(),
                        );
                        return;
                    }
                }
                let reflection = self.cw.methodref(
                    "kotlin/jvm/internal/Reflection",
                    "getOrCreateKotlinClass",
                    "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
                );
                code.invokestatic(reflection, 1, 1);
            }
            IrExpr::GetValue(i) => {
                // A slot that was never allocated means the lowering produced malformed IR (e.g. an
                // unsupported suspend shape). Don't panic — flag the file unemittable and skip it.
                let Some(&(slot, jt)) = self.slots.get(i) else {
                    crate::trace_compiler!(
                        "suspend",
                        "EMIT_BAIL GetValue unallocated slot i={i} owner={} known={:?}",
                        self.owner,
                        self.slots.keys().collect::<Vec<_>>()
                    );
                    self.run.set_emit_error(
                        "value read references a slot that was never declared".to_string(),
                    );
                    return;
                };
                load(jt, slot, code);
            }
            IrExpr::PropertyRead {
                receiver,
                owner,
                name,
                ty,
                interface,
                operation,
            } => {
                let (receiver, owner, name, ty, interface, operation) = (
                    *receiver,
                    owner.render(),
                    name.clone(),
                    *ty,
                    *interface,
                    operation.unwrap_or(e),
                );
                self.emit_property_read(
                    PropertyOperation {
                        expression: operation,
                        receiver,
                        owner: &owner,
                        name: &name,
                        ty: &ty,
                        interface,
                    },
                    code,
                );
            }
            IrExpr::PropertyWrite {
                receiver,
                owner,
                name,
                value,
                ty,
                interface,
                operation,
            } => {
                let (receiver, owner, name, value, ty, interface, operation) = (
                    *receiver,
                    owner.render(),
                    name.clone(),
                    *value,
                    *ty,
                    *interface,
                    operation.unwrap_or(e),
                );
                self.emit_property_write(
                    PropertyOperation {
                        expression: operation,
                        receiver,
                        owner: &owner,
                        name: &name,
                        ty: &ty,
                        interface,
                    },
                    value,
                    code,
                );
            }
            IrExpr::EnclosingInstance {
                receiver,
                inner,
                outer,
            } => {
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(
                    &inner.render(),
                    "this$0",
                    &type_descriptor(Ty::obj_name(*outer)),
                );
                code.getfield(fref, 1);
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                let c = &self.ir.classes[*class as usize];
                let source_name = c.fields[*index as usize].name.clone();
                let name = instance_field_jvm_name(self.ir, c, &c.fields[*index as usize]);
                let fty = c.fields[*index as usize].ty;
                let jt = jvm_declared_ty(&fty);
                let owner = c.fq_name();
                let is_lateinit = c.fields[*index as usize].is_lateinit();
                if static_storage(self.ir, c) {
                    // A static-storage object field: no instance operand. The receiver is `this`
                    // (or the INSTANCE read) — evaluate it only if it could have effects.
                    if !matches!(self.ir.expr(*receiver), crate::ir::IrExpr::GetValue(_)) {
                        self.emit_value(*receiver, code);
                        code.pop();
                    }
                    let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    self.emit_value(*receiver, code);
                    let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                    code.getfield(fref, slot_words(jt) as i32);
                }
                // A `lateinit var` read throws `UninitializedPropertyAccessException` while the field is
                // still null (kotlinc inserts this at every access): `dup; ifnonnull L; ldc name;
                // invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:`.
                if is_lateinit {
                    code.dup();
                    let lbl = code.new_label();
                    code.ifnonnull(lbl);
                    code.push_string(&source_name, self.cw);
                    let m = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(m, 1, 0);
                    self.bind(lbl, code);
                }
            }
            IrExpr::LateinitInitialized {
                receiver,
                class,
                index,
            } => {
                // The RAW field read — no throw-if-null guard, which is the whole point: this node
                // exists so `::prop.isInitialized` can TEST the field a normal read would reject.
                // The null comparison itself is built in lowering from the ordinary comparison node,
                // so the branch/stackmap shape stays the one every other comparison uses.
                let c = &self.ir.classes[*class as usize];
                let name = instance_field_jvm_name(self.ir, c, &c.fields[*index as usize]);
                let fty = c.fields[*index as usize].ty;
                let jt = jvm_declared_ty(&fty);
                let owner = c.fq_name();
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                code.getfield(fref, slot_words(jt) as i32);
            }
            IrExpr::GetStatic(i) => {
                let s = &self.ir.statics[*i as usize];
                let jt = jvm_declared_ty(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                // A static declaring an OWNER lives on that class, not the facade. Within the owner
                // read the (private) field directly; from any other class — the companion's
                // delegating accessors — go through the owner's PUBLIC synthetic `access$get<X>$cp`
                // bridge, kotlinc's hoisted-companion-property access shape. A `@JvmField` static
                // is a PUBLIC field with no bridges: every reader goes `getstatic` directly.
                if let Some(owner) = self.ir.statics[*i as usize].owner {
                    let owner_name = owner.render();
                    if self.owner == owner_name
                        || !self.ir.is_jvm_companion_hoisted_static(*i)
                        || self.ir.is_jvm_field_static(*i)
                    {
                        let fref = self.cw.fieldref(&owner_name, &name, &type_descriptor(jt));
                        code.getstatic(fref, slot_words(jt) as i32);
                    } else {
                        let m = self.cw.methodref(
                            &owner_name,
                            &format!("access${}$cp", property_getter_name(&name)),
                            &format!("(){}", type_descriptor(jt)),
                        );
                        code.invokestatic(m, 0, slot_words(jt) as i32);
                    }
                }
                // Within the facade (or a `const val`, which is public) read the field directly; from
                // another class a plain top-level property is private, so go through `getX()` — kotlinc's
                // cross-file property-access compilation.
                else if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    // A PRIVATE top-level property has no public getter; cross-class reads inside the
                    // file go through kotlinc's `access$get<X>$p` bridge.
                    let gname = if self.ir.statics[*i as usize].visibility.is_private() {
                        format!("access${}$p", property_getter_name(&name))
                    } else {
                        property_getter_name(&name)
                    };
                    let m =
                        self.cw
                            .methodref(&facade, &gname, &format!("(){}", type_descriptor(jt)));
                    code.invokestatic(m, 0, slot_words(jt) as i32);
                }
            }
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ctor_desc,
                external_target: _,
                defaults: default_parameters,
                default_prefix_count,
            } => {
                let owner = internal.render();
                let args = args.clone();
                // The constructor descriptor + its argument-word count come from ONE source, identified by
                // the owner NAME (no same-file/other-file/classpath control-flow split):
                //  - a verbatim descriptor (`ctor_desc`) for a classpath ctor whose signature isn't modeled
                //    as `Ty`s — arg words come from each argument's own value type; OR
                //  - the known parameter types: the node's `ctor_params`, else the named in-IR class's
                //    primary-ctor field types.
                let (desc, use_accessor, base_parameter_count, source_parameter_count) =
                    if let Some(d) = ctor_desc {
                        debug_assert!(default_parameters.is_empty());
                        (d.clone(), false, args.len(), args.len())
                    } else {
                        let mut field_tys: Vec<Ty> = match ctor_params {
                            Some(ps) => jvm_tys(ps),
                            None => self
                                .ir
                                .class_id_by_name(*internal)
                                .map(|c| class_ctor_jvm_tys(&self.ir.classes[c as usize]))
                                .unwrap_or_default(),
                        };
                        // A class whose primary ctor takes a value-class param has a PRIVATE primary + a
                        // PUBLIC|SYNTHETIC accessor `(…args, DefaultConstructorMarker)`. Every construction
                        // routes through the accessor (a trailing `null`) — from the class itself too
                        // (`copy`, a member building a sibling instance): kotlinc leaves only the
                        // accessor calling the private primary. A selected secondary constructor uses
                        // the declaration fact recorded on this exact expression before erasure.
                        // Another class constructing through a private constructor does the same.
                        // A call supplying defaults targets the public `$default` overload instead,
                        // which reaches the accessor itself.
                        let use_accessor = default_parameters.is_empty()
                            && ((ctor_params.is_none() && self.ir.has_value_param_ctor(&owner))
                                || self.ir.has_value_class_parameter_construction(e)
                                || self.ir.construction_targets.get(&e).is_some_and(|target| {
                                    constructor_accessors::reached_through_accessor(
                                        *target,
                                        self.ir.expression_owners.get(&e).copied(),
                                        *internal,
                                    )
                                }));
                        let base_parameter_count = field_tys.len();
                        let source_parameter_count = base_parameter_count
                            .checked_sub(*default_prefix_count as usize)
                            .expect("constructor default prefix exceeds its parameters");
                        if use_accessor {
                            field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                        }
                        if !default_parameters.is_empty() {
                            field_tys.extend(std::iter::repeat_n(
                                Ty::Int,
                                default_mask_count(source_parameter_count),
                            ));
                            field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                        }
                        (
                            method_descriptor(&field_tys, Ty::Unit),
                            use_accessor,
                            base_parameter_count,
                            source_parameter_count,
                        )
                    };
                let physical_params =
                    parse_descriptor_params(&desc).expect("constructor descriptor must be valid");
                let aw = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                if args.iter().any(|&a| self.emits_control_flow(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack — its merge frame
                    // would omit them. Evaluate all args into temps first (clean stack), then build.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    let mut supplied = temps.iter().zip(args.iter());
                    // A defaulted construction takes the same rule as a defaulted call: the
                    // operands its `$default` ABI invents — a placeholder for an omitted argument,
                    // and the trailing marker/mask/marker group — belong to the CONSTRUCTION, so
                    // its own line goes back into effect at the start of each such run rather than
                    // only at the `invokespecial` after all of them.
                    let mut inside_run = false;
                    for (parameter, physical) in physical_params
                        .iter()
                        .copied()
                        .take(base_parameter_count)
                        .enumerate()
                    {
                        let omitted = parameter >= *default_prefix_count as usize
                            && default_parameters.contains(
                                &u32::try_from(parameter - *default_prefix_count as usize)
                                    .expect("too many constructor parameters"),
                            );
                        self.mark_synthesized_run_start(e, omitted, &mut inside_run, code);
                        if omitted {
                            push_zero(physical, code, self.cw);
                        } else {
                            let ((slot, ty, _), argument) = supplied
                                .next()
                                .unwrap_or_else(|| {
                                    panic!(
                                        "checked constructor supplied-argument count: owner={owner} \
                                         supplied={} physical={base_parameter_count} defaults={default_parameters:?} \
                                         prefix={default_prefix_count} expression={e} origin={:?}",
                                        args.len(),
                                        self.ir.fir_origins.get(&e),
                                    )
                                });
                            load(*ty, *slot, code);
                            self.adapt_physical_constructor_operand_for(
                                e, parameter, *argument, *ty, physical, code,
                            );
                        }
                    }
                    self.release_operand_spills(&temps);
                    if use_accessor || !default_parameters.is_empty() {
                        self.mark_synthesized_run_start(e, true, &mut inside_run, code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    emit_constructor_default_arguments(
                        default_parameters,
                        source_parameter_count,
                        code,
                        self.cw,
                    );
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    self.mark_call_start(e, code);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    let mut supplied = args.iter().copied();
                    let mut inside_run = false;
                    for (parameter, physical) in physical_params
                        .iter()
                        .copied()
                        .take(base_parameter_count)
                        .enumerate()
                    {
                        let omitted = parameter >= *default_prefix_count as usize
                            && default_parameters.contains(
                                &u32::try_from(parameter - *default_prefix_count as usize)
                                    .expect("too many constructor parameters"),
                            );
                        self.mark_synthesized_run_start(e, omitted, &mut inside_run, code);
                        if omitted {
                            push_zero(physical, code, self.cw);
                        } else {
                            let argument = supplied
                                .next()
                                .unwrap_or_else(|| {
                                    panic!(
                                        "checked constructor supplied-argument count: owner={owner} \
                                         supplied={} physical={base_parameter_count} defaults={default_parameters:?} \
                                         prefix={default_prefix_count} expression={e} origin={:?}",
                                        args.len(),
                                        self.ir.fir_origins.get(&e),
                                    )
                                });
                            self.emit_value(argument, code);
                            self.adapt_physical_constructor_operand_for(
                                e,
                                parameter,
                                argument,
                                self.value_ty(argument),
                                physical,
                                code,
                            );
                        }
                    }
                    if use_accessor || !default_parameters.is_empty() {
                        self.mark_synthesized_run_start(e, true, &mut inside_run, code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    emit_constructor_default_arguments(
                        default_parameters,
                        source_parameter_count,
                        code,
                        self.cw,
                    );
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    self.mark_call_start(e, code);
                    code.invokespecial(m, aw, 0);
                }
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
                let c = &self.ir.classes[*class as usize];
                let fid = c.methods[*index as usize];
                let f = &self.ir.functions[fid as usize];
                let param_tys = jvm_function_params(self.ir, fid);
                let ret = jvm_declared_ty(&f.ret);
                let name = f.name.clone();
                let owner = c.fq_name();
                let is_iface = c.is_interface;
                if args.iter().any(|a| a.is_none()) {
                    // Some arguments are omitted — invoke the `<name>$default(self, params…, mask, marker)`
                    // stub: receiver, each provided arg (or a zero placeholder for an omitted one with its
                    // mask bit set), the mask, then a null marker. A nullable-underlying value-class param
                    // is BOXED in the stub signature (matching `emit_default_stub`), so a provided arg is
                    // `box-impl`d and the placeholder/descriptor use the boxed type.
                    let boxed: HashMap<usize, Ty> = self
                        .ir
                        .default_stub_boxed_params
                        .get(&fid)
                        .map(|v| v.iter().copied().collect())
                        .unwrap_or_default();
                    let stub_param_tys: Vec<Ty> = param_tys
                        .iter()
                        .enumerate()
                        .map(|(i, t)| boxed.get(&i).copied().unwrap_or(*t))
                        .collect();
                    let args = args.clone();
                    self.emit_value(*receiver, code);
                    // Mask bits are LOGICAL (kotlinc numbers them over the declared value
                    // parameters) — a member EXTENSION's physical receiver at params[0] does not
                    // shift them, and is never omitted.
                    let recv_offset = usize::from(self.ir.extension_receiver_fns.contains(&fid));
                    let logical_param_count = param_tys
                        .len()
                        .checked_sub(recv_offset)
                        .expect("an extension receiver is a leading physical parameter");
                    let mut masks = vec![0i32; default_mask_count(logical_param_count)];
                    // Every operand the CALL synthesizes — a placeholder standing in for an omitted
                    // argument, and the trailing mask/marker group — carries the call's own line, so
                    // an argument supplied between two of them puts its own line in effect and the
                    // next group restores the call's. A call that omits nothing synthesizes nothing,
                    // which is why an ordinary call's dispatch mark sits at its invoke.
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            Some(a) => {
                                self.emit_value(*a, code);
                                if let Some(vc) = boxed.get(&i) {
                                    emit_box_impl(self.ir, self.cw, vc, code);
                                }
                            }
                            None => {
                                self.mark_call_start(e, code);
                                push_zero(stub_param_tys[i], code, self.cw);
                                let li = i
                                    .checked_sub(recv_offset)
                                    .expect("an extension receiver cannot be omitted");
                                masks[li / 32] |= default_mask_bit(li);
                            }
                        }
                    }
                    self.mark_call_start(e, code);
                    for mask in masks {
                        code.push_int(mask, self.cw);
                    }
                    code.aconst_null();
                    let mut stub_params = vec![Ty::obj(&owner)];
                    stub_params.extend(stub_param_tys.iter().copied());
                    stub_params.extend(std::iter::repeat_n(
                        Ty::Int,
                        default_mask_count(logical_param_count),
                    ));
                    stub_params.push(Ty::obj("java/lang/Object"));
                    let aw: i32 = stub_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let stub_desc = method_descriptor(&stub_params, ret);
                    let stub_name = format!("{name}$default");
                    // The `$default` stub of an INTERFACE method is a STATIC interface method —
                    // referenced via an `InterfaceMethodref` constant (a plain `Methodref` is an
                    // `IncompatibleClassChangeError`), still invoked with `invokestatic`. Under
                    // `enable`/`no-compatibility` kotlinc puts that stub on the interface and call
                    // sites use it; under `disable` the interface holds nothing executable and the
                    // stub exists only on `<Iface>$DefaultImpls`, so a call site aimed at the
                    // interface would link to a method that was never emitted.
                    let holder;
                    let (stub_owner, stub_on_interface) =
                        if is_iface && self.jvm_default == JvmDefaultMode::Disable {
                            holder = format!("{owner}$DefaultImpls");
                            (&holder, false)
                        } else {
                            (&owner, is_iface)
                        };
                    let m = if stub_on_interface {
                        self.cw
                            .interface_methodref(stub_owner, &stub_name, &stub_desc)
                    } else {
                        self.cw.methodref(stub_owner, &stub_name, &stub_desc)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                    return;
                }
                let call_args: Vec<u32> = args.iter().map(|a| a.unwrap()).collect();
                // An argument-count/descriptor mismatch can only come from a pass that rewrote the
                // callee's ABI without fixing this call site (a suspend call the coroutine flattener
                // failed to thread a continuation into — an unmodeled shape). Never emit the
                // unverifiable call: the operand contract refuses, this arm bails the file (the gate
                // SKIPS it) and pushes a typed zero so the dead code that follows still assembles.
                let desc = method_descriptor(&param_tys, ret);
                if let Err(mismatch) = self.emit_descriptor_virtual_operands(
                    e,
                    crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                        owner: &owner,
                        name: &name,
                        descriptor: &desc,
                    },
                    *receiver,
                    &call_args,
                    &param_tys,
                    code,
                ) {
                    self.bail_descriptor_arity(&mismatch, ret, code);
                    return;
                }
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                crate::trace_compiler!(
                    "resolve",
                    "emit MethodCall {}.{} fid={fid} private={} iface={is_iface}",
                    owner,
                    name,
                    self.ir.private_methods.contains(&fid)
                );
                if self.ir.private_methods.contains(&fid) {
                    // A PRIVATE method is non-virtual — `invokespecial` (an interface private method uses an
                    // `InterfaceMethodref`), so it never dispatches to a same-named override. Under
                    // `disable` the body moved to the holder, and an `invokespecial` naming the
                    // interface from another class is not even verifiable.
                    if self.owner != owner
                        && self
                            .run
                            .private_member_access_bridges
                            .borrow()
                            .contains(&fid)
                    {
                        let mut bridge_params = Vec::with_capacity(param_tys.len() + 1);
                        bridge_params.push(Ty::obj(&owner));
                        bridge_params.extend(param_tys.iter().copied());
                        let bridge_desc = method_descriptor(&bridge_params, ret);
                        let bridge_name = format!("access${name}");
                        let method = if is_iface {
                            self.cw
                                .interface_methodref(&owner, &bridge_name, &bridge_desc)
                        } else {
                            self.cw.methodref(&owner, &bridge_name, &bridge_desc)
                        };
                        self.mark_call_start(e, code);
                        code.invokestatic(method, aw + 1, physical_call_result_words(ret));
                    } else if let Some((holder, holder_desc)) = is_iface
                        .then(|| self.holder_call(&owner, &desc, true))
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw + 1, physical_call_result_words(ret));
                    } else {
                        let m = if is_iface {
                            self.cw.interface_methodref(&owner, &name, &desc)
                        } else {
                            self.cw.methodref(&owner, &name, &desc)
                        };
                        self.mark_call_start(e, code);
                        code.invokespecial(m, aw, physical_call_result_words(ret));
                    }
                } else if is_iface {
                    // Dispatch through an interface — `invokeinterface I.m`.
                    let m = self.cw.interface_methodref(&owner, &name, &desc);
                    self.mark_call_start(e, code);
                    code.invokeinterface(m, aw, physical_call_result_words(ret));
                } else {
                    let m = self.cw.methodref(&owner, &name, &desc);
                    self.mark_call_start(e, code);
                    code.invokevirtual(m, aw, physical_call_result_words(ret));
                }
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => match callee {
                // `jvm::module_calls::realize` rewrites every `super` call into
                // `Callee::Special` before emission.
                Callee::Super { .. } => {
                    unreachable!("a super call must be realized before JVM emission")
                }
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys = jvm_function_params(self.ir, *fid);
                    let ret = jvm_declared_ty(&f.ret);
                    // A PRIVATE facade function can't be invoked from another class (a lambda impl on
                    // its enclosing class, a continuation class, any class member) — kotlinc routes
                    // those callers through the `access$<name>` bridge (emitted by `emit_pass` when
                    // referenced; see `facade_access_bridges`).
                    let name = if self.owner != self.facade && self.ir.private_methods.contains(fid)
                    {
                        format!("access${}", f.name)
                    } else {
                        f.name.clone()
                    };
                    let args = args.clone();
                    // Same arity/descriptor contract as `MethodCall` above: an unthreaded suspend
                    // call must bail the file, never emit an unverifiable invocation.
                    if let Err(mismatch) =
                        self.emit_source_call_operands(e, 0, &args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                }
                Callee::ClassStatic { owner, function } => {
                    let f = &self.ir.functions[*function as usize];
                    let param_tys = jvm_function_params(self.ir, *function);
                    let ret = jvm_declared_ty(&f.ret);
                    if let Err(mismatch) =
                        self.emit_source_call_operands(e, 0, args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let argument_words: i32 =
                        param_tys.iter().map(|ty| slot_words(*ty) as i32).sum();
                    let descriptor = method_descriptor(&param_tys, ret);
                    let source_owner_is_interface = self.ir.classes.iter().any(|candidate| {
                        candidate.is_interface && candidate.fq_name_id() == *owner
                    });
                    let owner = owner.render();
                    // `owner_is_interface` answers from the CLASSPATH; a static declared on an
                    // interface being compiled right now is not there. An `invokestatic` naming an
                    // interface must use an InterfaceMethodref, so the file's own classes answer
                    // too.
                    let owner_is_interface =
                        source_owner_is_interface || self.bodies.owner_is_interface(&owner);
                    let method = if owner_is_interface {
                        self.cw.interface_methodref(&owner, &f.name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &f.name, &descriptor)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(method, argument_words, physical_call_result_words(ret));
                }
                Callee::ClassStaticDefault { owner, function } => {
                    let f = &self.ir.functions[*function as usize];
                    let param_tys =
                        static_default_stub_params(self.ir, *function, Ty::obj("java/lang/Object"));
                    let ret = jvm_declared_ty(&f.ret);
                    if let Err(mismatch) =
                        self.emit_source_default_call_operands(e, args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let argument_words: i32 =
                        param_tys.iter().map(|ty| slot_words(*ty) as i32).sum();
                    let descriptor = method_descriptor(&param_tys, ret);
                    let owner = owner.render();
                    let method =
                        self.cw
                            .methodref(&owner, &format!("{}$default", f.name), &descriptor);
                    self.mark_call_start(e, code);
                    code.invokestatic(method, argument_words, physical_call_result_words(ret));
                }
                Callee::LocalDefault(fid) => {
                    // The `foo$default(realparams, mask..., Object marker)` synthetic on the self facade
                    // (emitted by `emit_facade_default_stub`). Args already include mask words + marker.
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys =
                        static_default_stub_params(self.ir, *fid, Ty::obj("java/lang/Object"));
                    let ret = jvm_declared_ty(&f.ret);
                    let name = format!("{}$default", f.name);
                    let args = args.clone();
                    if let Err(mismatch) =
                        self.emit_source_default_call_operands(e, &args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                }
                Callee::Intrinsic { operation, .. } => match operation {
                    crate::ir::IrIntrinsic::Assert { mode } => {
                        self.emit_assertion(*mode, args, code)
                    }
                    crate::ir::IrIntrinsic::TypeOf { ty } => {
                        let parameters = super::type_of::TypeParameters::new(self.ir, &self.facade);
                        let mut instructions = Vec::new();
                        match super::type_of::generate(*ty, &parameters, &mut instructions) {
                            Ok(()) => super::type_of::encode(&instructions, code, self.cw),
                            Err(error) => self.run.set_emit_error(error.to_string()),
                        }
                    }
                    crate::ir::IrIntrinsic::ArrayGet => {
                        self.emit_array_get(dispatch_receiver.unwrap(), args[0], code)
                    }
                    crate::ir::IrIntrinsic::ArraySet => {
                        self.emit_array_set(dispatch_receiver.unwrap(), args[0], args[1], code)
                    }
                    crate::ir::IrIntrinsic::ArraySize => {
                        self.emit_value(dispatch_receiver.unwrap(), code);
                        code.arraylength();
                    }
                    crate::ir::IrIntrinsic::StringGet => {
                        self.emit_value(dispatch_receiver.unwrap(), code);
                        self.emit_value(args[0], code);
                        let method = self.cw.methodref("java/lang/String", "charAt", "(I)C");
                        code.invokevirtual(method, 1, 1);
                    }
                    crate::ir::IrIntrinsic::StringLength => {
                        self.emit_value(dispatch_receiver.unwrap(), code);
                        let method = self.cw.methodref("java/lang/String", "length", "()I");
                        code.invokevirtual(method, 0, 1);
                    }
                    crate::ir::IrIntrinsic::StringPlus => {
                        // kotlinc flattens a `plus` chain (and any template inside it) into ONE
                        // concatenation before codegen; every part below is a leaf operand.
                        let mut parts = Vec::new();
                        self.flatten_concat_parts(dispatch_receiver.unwrap(), &mut parts);
                        self.flatten_concat_parts(args[0], &mut parts);
                        self.emit_string_plus_parts(&parts, code)
                    }
                    crate::ir::IrIntrinsic::NullableAnyToString => {
                        let receiver = dispatch_receiver.unwrap();
                        let ty = self.value_ty(receiver);
                        self.emit_value(receiver, code);
                        let descriptor = match ty {
                            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
                            Ty::Long => "(J)Ljava/lang/String;",
                            Ty::Boolean => "(Z)Ljava/lang/String;",
                            Ty::Char => "(C)Ljava/lang/String;",
                            Ty::Double => "(D)Ljava/lang/String;",
                            Ty::Float => "(F)Ljava/lang/String;",
                            _ => "(Ljava/lang/Object;)Ljava/lang/String;",
                        };
                        let method = self.cw.methodref("java/lang/String", "valueOf", descriptor);
                        code.invokestatic(method, slot_words(ty) as i32, 1);
                    }
                    crate::ir::IrIntrinsic::EnumValueOf { classifier } => {
                        // `enumValueOf<E>` is the stdlib's reified INLINE template, so what follows
                        // is an expansion of it rather than a call on this line. kotlinc marks the
                        // call site here and lets the argument's own line join it at the same
                        // offset; the `Enum.valueOf` this ends with belongs to the expansion.
                        self.mark_inline_call_site_line(e, code);
                        match classifier.non_null() {
                            Ty::TyParam(identity, _) => {
                                // `enumValueOf` is not `@InlineOnly`, so kotlinc's inliner stores
                                // its argument once, into the parameter's slot, before the body
                                // runs; the body then reads that slot.
                                self.emit_value(args[0], code);
                                let argument =
                                    self.frame.enter_temp(TempRole::InlineArgument, Ty::String);
                                let name = argument.slot();
                                store(Ty::String, name, code);
                                let lease = self.lease_frame_temporary(argument, Ty::String);
                                // Kotlin's public inline template keeps the reified classifier as
                                // the standard mode-5 marker plus a null Class placeholder. A
                                // consuming compiler replaces that placeholder at the call site.
                                code.push_int(5, self.cw);
                                code.push_string(
                                    crate::types::type_parameter_source_name(identity),
                                    self.cw,
                                );
                                let marker = self.cw.methodref(
                                    "kotlin/jvm/internal/Intrinsics",
                                    "reifiedOperationMarker",
                                    "(ILjava/lang/String;)V",
                                );
                                code.invokestatic(marker, 2, 0);
                                code.aconst_null();
                                load(Ty::String, name, code);
                                self.release_temporary(lease);
                                let method = self.cw.methodref(
                                    "java/lang/Enum",
                                    "valueOf",
                                    "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
                                );
                                code.invokestatic(method, 2, 1);
                            }
                            Ty::Obj(classifier, _) => {
                                self.emit_value(args[0], code);
                                let owner = classifier.render();
                                let descriptor = format!("(Ljava/lang/String;)L{owner};");
                                let method = self.cw.methodref(&owner, "valueOf", &descriptor);
                                code.invokestatic(method, 1, 1);
                            }
                            classifier => unreachable!(
                                "checked enumValueOf classifier is enum or reified: {classifier:?}"
                            ),
                        }
                    }
                    crate::ir::IrIntrinsic::PrimitiveCompare { operand, .. } => {
                        let receiver = dispatch_receiver
                            .expect("checked primitive compare has a dispatch receiver");
                        let [argument] = args.as_slice() else {
                            unreachable!("checked primitive compare has one argument")
                        };
                        self.emit_value(receiver, code);
                        self.emit_value(*argument, code);
                        let (owner, descriptor) = match *operand {
                            Ty::Int => ("java/lang/Integer", "(II)I"),
                            Ty::Long => ("java/lang/Long", "(JJ)I"),
                            Ty::Float => ("java/lang/Float", "(FF)I"),
                            Ty::Double => ("java/lang/Double", "(DD)I"),
                            _ => unreachable!(
                                "checked primitive compare carries a scalar comparison operand"
                            ),
                        };
                        let method = self.cw.methodref(owner, "compare", descriptor);
                        // This intrinsic's lowering IS a call, so it takes the dispatch rule: the
                        // source call's line returns at the `invokestatic`, after the operands have
                        // marked theirs. Intrinsics lowered to an instruction instead — an array
                        // read, an arithmetic op — deliberately do not.
                        self.mark_dispatch_line(e, code);
                        code.invokestatic(method, (slot_words(*operand) * 2) as i32, 1);
                    }
                    crate::ir::IrIntrinsic::CoroutineContext => {
                        unreachable!(
                            "CoroutineContext intrinsic must be realized by the CPS pass before emit"
                        )
                    }
                    crate::ir::IrIntrinsic::UnsignedToString { source } => {
                        let receiver = dispatch_receiver.expect("unsigned toString has a receiver");
                        self.emit_value(receiver, code);
                        let (owner, name, descriptor) = match *source {
                            Ty::UByte | Ty::UShort => {
                                code.push_int(
                                    source
                                        .unsigned_widen_mask()
                                        .expect("narrow unsigned value has a mask"),
                                    self.cw,
                                );
                                code.iand();
                                ("java/lang/Integer", "toString", "(I)Ljava/lang/String;")
                            }
                            Ty::UInt => (
                                "java/lang/Integer",
                                "toUnsignedString",
                                "(I)Ljava/lang/String;",
                            ),
                            Ty::ULong => (
                                "java/lang/Long",
                                "toUnsignedString",
                                "(J)Ljava/lang/String;",
                            ),
                            _ => unreachable!("checked unsigned conversion carries unsigned type"),
                        };
                        let method = self.cw.methodref(owner, name, descriptor);
                        code.invokestatic(
                            method,
                            slot_words(source.int_arithmetic_repr()) as i32,
                            1,
                        );
                    }
                    crate::ir::IrIntrinsic::PrimitiveArrayNew { element } => {
                        self.emit_value(args[0], code);
                        code.newarray(prim_newarray_atype(*element));
                    }
                    crate::ir::IrIntrinsic::DataClassFieldEquals { ty } => {
                        let left = args[0];
                        let right = args[1];
                        if self.emit_data_class_value_equals(*ty, left, right, code) {
                            return;
                        }
                        self.emit_value(left, code);
                        self.box_scalar_operand(self.value_ty(left), code);
                        self.emit_value(right, code);
                        self.box_scalar_operand(self.value_ty(right), code);
                        let method = self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "areEqual",
                            "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                        );
                        code.invokestatic(method, 2, 1);
                    }
                    crate::ir::IrIntrinsic::DataClassFieldHash { ty } => {
                        let value = args[0];
                        if self.emit_data_class_value_hash(*ty, value, code) {
                            return;
                        }
                        if ty.is_array() {
                            self.emit_value(value, code);
                            let descriptor = format!("({})I", type_descriptor(*ty));
                            let method =
                                self.cw
                                    .methodref("java/util/Arrays", "hashCode", &descriptor);
                            code.invokestatic(method, 1, 1);
                        } else if ty.non_null().is_jvm_scalar() && !ty.is_nullable() {
                            let scalar = ty.non_null();
                            self.emit_value(value, code);
                            let (owner, descriptor) = match scalar {
                                Ty::Int => ("java/lang/Integer", "(I)I"),
                                Ty::Short => ("java/lang/Short", "(S)I"),
                                Ty::Byte => ("java/lang/Byte", "(B)I"),
                                Ty::Char => ("java/lang/Character", "(C)I"),
                                Ty::Boolean => ("java/lang/Boolean", "(Z)I"),
                                Ty::Long => ("java/lang/Long", "(J)I"),
                                Ty::Double => ("java/lang/Double", "(D)I"),
                                Ty::Float => ("java/lang/Float", "(F)I"),
                                _ => unreachable!("scalar data hash"),
                            };
                            let method = self.cw.methodref(owner, "hashCode", descriptor);
                            code.invokestatic(method, slot_words(scalar) as i32, 1);
                        } else {
                            self.emit_value(value, code);
                            let owner = data_class_hashcode_owner(self.ir, self.bodies, *ty)
                                .expect("checked reference data-class field has a JVM hash owner");
                            let method = self.cw.methodref(&owner, "hashCode", "()I");
                            code.invokevirtual(method, 0, 1);
                        }
                    }
                    crate::ir::IrIntrinsic::DataClassArrayToString { ty } => {
                        self.emit_value(args[0], code);
                        let descriptor =
                            crate::jvm::array_representation::arrays_to_string_descriptor(*ty);
                        let method = self
                            .cw
                            .methodref("java/util/Arrays", "toString", &descriptor);
                        code.invokestatic(method, 1, 1);
                    }
                },
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                    module_target,
                    ..
                } => {
                    // A top-level function from another file → `invokestatic <facade>.<name>(desc)`.
                    let param_tys = jvm_tys(params);
                    let ret = jvm_declared_ty(ret);
                    let owner_is_interface = super::module_calls::static_owner_is_jvm_interface(
                        self.ir,
                        *facade,
                        *module_target,
                    );
                    let (facade, name) = (facade.render(), name.clone());
                    let args = args.clone();
                    if let Err(mismatch) =
                        self.emit_call_descriptor_operands(e, 0, &args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    // `-jvm-default=disable` puts the `$default` synthetic on the holder, not on the
                    // interface, so a call site aimed at the interface links to a method that was
                    // never emitted (`NoSuchMethodError` on the first defaulted call).
                    let holder;
                    let (target_owner, owner_is_interface) = if self.jvm_default
                        == JvmDefaultMode::Disable
                        && owner_is_interface
                        && name.ends_with("$default")
                    {
                        holder = format!("{facade}$DefaultImpls");
                        (&holder, false)
                    } else {
                        (&facade, owner_is_interface)
                    };
                    let m = if owner_is_interface {
                        self.cw.interface_methodref(target_owner, &name, &desc)
                    } else {
                        self.cw.methodref(target_owner, &name, &desc)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                }
                Callee::Module { .. }
                | Callee::ModuleWithDefaults { .. }
                | Callee::LocalWithDefaults { .. }
                | Callee::ClassStaticWithDefaults { .. }
                | Callee::External { .. } => {
                    unreachable!("stable calls must be realized before JVM emission")
                }
                Callee::Static {
                    owner,
                    name,
                    descriptor,
                    inline,
                } => {
                    let (owner, name, descriptor, inline) =
                        (owner.render(), name.clone(), descriptor.clone(), *inline);
                    let args = args.clone();
                    crate::trace_compiler!(
                        "resolve",
                        "emit static {owner}.{name}{descriptor} inline={inline:?}"
                    );
                    let reified =
                        crate::jvm::reified_operations::splice_arguments(self.ir, e, &self.facade);
                    // `@InlineOnly`/non-public inline functions must splice. Public inline functions have
                    // callable bytecode, so a failed optional splice can fall back to a real call. An
                    // ordinary `$default` synthetic is an ABI dispatcher whose mask prologue must run
                    // as emitted. Only a metadata-normalized splice-only declaration may bypass it.
                    if inline.can_inline() && (!name.ends_with("$default") || inline.must_inline())
                    {
                        let call_frame = self.frame.mark();
                        let spliced = if let Some(&recv) = dispatch_receiver.as_ref() {
                            let recv_desc = type_descriptor(self.value_ty(recv));
                            let splice_desc = format!("({}{}", recv_desc, &descriptor[1..]);
                            let mut all = Vec::with_capacity(args.len() + 1);
                            all.push(recv);
                            all.extend(args.iter().copied());
                            let target = InlineStaticTarget {
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                                splice_desc: &splice_desc,
                                inline_only: inline.must_inline(),
                                allow_owner_bridge: true,
                            };
                            self.try_inline_static_as(e, target, &all, 1, code, &reified)
                        } else {
                            let has_lambda_arg = args.iter().any(|&a| {
                                matches!(self.ir.expr(a), IrExpr::Lambda { .. })
                                    || self.function_ref_class_and_captures(a).is_some()
                                    || self.property_ref_class_and_captures(a).is_some()
                            });
                            let target = InlineStaticTarget {
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                                splice_desc: &descriptor,
                                inline_only: inline.must_inline(),
                                allow_owner_bridge: inline.must_inline() || has_lambda_arg,
                            };
                            self.try_inline_static_as(e, target, &args, 0, code, &reified)
                        };
                        // The call's temporaries go with its frame, as kotlinc's `leaveTemps` does.
                        self.frame.drop_to(call_frame);
                        if spliced {
                            return;
                        }
                        // The selected declaration already owns fallback legality. `MustInline`
                        // includes both inaccessible `@InlineOnly` bodies and metadata-declared
                        // reified functions; a substitution map is only a specialization operand.
                        if inline.must_inline() {
                            crate::trace_compiler!(
                                "emit",
                                "inline splice failed for {owner}.{name}{descriptor}"
                            );
                            self.run.set_inline_bail("inline splice failed");
                        }
                    }
                    let physical_params = parse_descriptor_params(&descriptor)
                        .expect("static call descriptor must be valid");
                    let (physical_args, leading_non_argument_operands) =
                        match dispatch_receiver.as_ref() {
                            Some(&recv) if physical_params.len() == args.len() + 1 => {
                                let mut all = Vec::with_capacity(args.len() + 1);
                                all.push(recv);
                                all.extend(args.iter().copied());
                                (all, 1)
                            }
                            _ => (args, 0),
                        };
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if let Err(mismatch) = self.emit_call_descriptor_operands(
                        e,
                        leading_non_argument_operands,
                        &physical_args,
                        &physical_params,
                        code,
                    ) {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    // A static method DECLARED ON AN INTERFACE (a Kotlin interface's `foo$default` synthetic,
                    // reached when a call omits an interface-declared default) must be an `InterfaceMethodref`
                    // even for `invokestatic` — else the JVM throws `IncompatibleClassChangeError`. Classes
                    // (stdlib facades, the common case) stay `Methodref`.
                    let owner_is_interface = self.bodies.owner_is_interface(&owner);
                    let m = if owner_is_interface {
                        self.cw.interface_methodref(&owner, &name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &name, &descriptor)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Virtual {
                    owner,
                    name,
                    descriptor,
                    params,
                    interface,
                } => {
                    let recv = dispatch_receiver.expect("virtual call needs a receiver");
                    let semantic_receiver = self.value_ty(recv);
                    let interface = *interface || declared_jvm_interface(self.ir, *owner);
                    if semantic_receiver.is_array() {
                        if let Some((declared_params, declared_ret)) = params {
                            let realization = jvm_array_actual_realization(
                                *owner,
                                name,
                                semantic_receiver,
                                declared_params,
                                *declared_ret,
                            );
                            crate::trace_compiler!(
                                "emit",
                                "array actual candidate owner={} name={} receiver={:?} params={:?} ret={:?} realization={:?}",
                                owner,
                                name,
                                semantic_receiver,
                                declared_params,
                                declared_ret,
                                realization,
                            );
                            if let Some(realization) = realization {
                                match realization {
                                    JvmArrayActualRealization::Get => {
                                        self.emit_array_get(recv, args[0], code)
                                    }
                                    JvmArrayActualRealization::Set => {
                                        self.emit_array_set(recv, args[0], args[1], code)
                                    }
                                    JvmArrayActualRealization::Size => {
                                        self.emit_value(recv, code);
                                        code.arraylength();
                                    }
                                }
                                return;
                            }
                        }
                        let erased_descriptor = |ty: Ty| {
                            if ty.is_array() {
                                arrays_param_desc(ty)
                            } else {
                                type_descriptor(ty)
                            }
                        };
                        let mut expected = String::from("(");
                        expected.push_str(&erased_descriptor(semantic_receiver));
                        for &argument in args {
                            expected.push_str(&erased_descriptor(self.value_ty(argument)));
                        }
                        expected.push(')');
                        let semantic_ret = self.value_ty(e);
                        expected.push_str(&erased_descriptor(semantic_ret));
                        if let Some(realization) =
                            self.bodies.static_array_member_realization(name, &expected)
                        {
                            let physical_params = parse_descriptor_params(&realization.descriptor)
                                .expect("selected array-member realization descriptor");
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(recv);
                            operands.extend(args.iter().copied());
                            let physical_ret = ty_from_descriptor_ret(&realization.descriptor);
                            if let Err(mismatch) = self.emit_call_descriptor_operands(
                                e,
                                1,
                                &operands,
                                &physical_params,
                                code,
                            ) {
                                self.bail_descriptor_arity(&mismatch, physical_ret, code);
                                return;
                            }
                            let argument_words: i32 = physical_params
                                .iter()
                                .map(|ty| slot_words(*ty) as i32)
                                .sum();
                            let method = self.cw.methodref(
                                &realization.owner,
                                &realization.name,
                                &realization.descriptor,
                            );
                            crate::trace_compiler!(
                                "emit",
                                "array member {}.{} -> {}.{}{}",
                                owner,
                                name,
                                realization.owner,
                                realization.name,
                                realization.descriptor,
                            );
                            self.mark_call_start(e, code);
                            code.invokestatic(
                                method,
                                argument_words,
                                slot_words(physical_ret) as i32,
                            );
                            return;
                        }
                    }
                    // A sibling-file user method carries its signature as `Ty`s (`params`): build the
                    // descriptor and emit a plain virtual/interface call. The classpath-operator
                    // special-casing below only applies to the `descriptor` form (a classpath receiver).
                    if let Some((param_tys, ret_ty)) = params {
                        let owner = owner.render();
                        let name = name.clone();
                        let ptys = jvm_tys(param_tys);
                        let ret = jvm_declared_ty(ret_ty);
                        let descriptor = method_descriptor(&ptys, ret);
                        let mut ops = vec![recv];
                        ops.extend(args.iter().copied());
                        // The receiver is already of the class the call names; only the arguments
                        // are materialized at the parameter types.
                        let mut physical = vec![self.value_ty(recv)];
                        physical.extend(ptys.iter().copied());
                        if let Err(mismatch) =
                            self.emit_source_call_operands(e, 1, &ops, &physical, code)
                        {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = ptys.iter().map(|t| slot_words(*t) as i32).sum();
                        if interface {
                            let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                            self.mark_call_start(e, code);
                            code.invokeinterface(m, aw, physical_call_result_words(ret));
                        } else {
                            let m = self.cw.methodref(&owner, &name, &descriptor);
                            self.mark_call_start(e, code);
                            code.invokevirtual(m, aw, physical_call_result_words(ret));
                        }
                        return;
                    }
                    let (owner, name, descriptor) =
                        (owner.render(), name.clone(), descriptor.clone());
                    let args = args.clone();
                    if self.emit_primitive_inc_dec_virtual(
                        &owner,
                        &name,
                        &descriptor,
                        recv,
                        &args,
                        code,
                    ) {
                        return;
                    }
                    if self.emit_unsigned_compare_to_virtual(&owner, &name, recv, &args, code) {
                        return;
                    }
                    // A `@JvmStatic` member of an `object`/companion (`Dispatchers.IO`): an ordinary
                    // member call in the language — resolved and lowered with a receiver — that kotlinc
                    // emits as a static taking none. Drop the receiver and `invokestatic`. The receiver is
                    // still EVALUATED when it can have an effect; kotlinc emits the same `…; pop;
                    // invokestatic` and elides a bare singleton/local read entirely.
                    if self.bodies.method_is_static(&owner, &name, &descriptor)
                        && parse_descriptor_params(&descriptor)
                            .is_some_and(|params| params.len() == args.len())
                    {
                        // The receiver is dropped, not skipped: it is still an expression the source
                        // program evaluates. Elide it only when it can run no code, or when it merely
                        // reads a static of the very class this `invokestatic` initializes anyway (the
                        // `Obj.INSTANCE` singleton read) — which is exactly what kotlinc elides.
                        let initializes_owner = matches!(
                            self.ir.expr(recv),
                            IrExpr::ExternalStaticField { owner: field_owner, .. }
                                if field_owner.matches(&owner)
                        );
                        if !crate::ir::expr_runs_no_code(self.ir, recv) && !initializes_owner {
                            self.emit_value(recv, code);
                            code.pop();
                        }
                        let physical_params = parse_descriptor_params(&descriptor)
                            .expect("static method descriptor must be valid");
                        let ret = ty_from_descriptor_ret(&descriptor);
                        if let Err(mismatch) =
                            self.emit_call_descriptor_operands(e, 0, &args, &physical_params, code)
                        {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw, slot_words(ret) as i32);
                        return;
                    }
                    if parse_descriptor_params(&descriptor)
                        .is_some_and(|params| params.len() == args.len() + 1)
                    {
                        let mut physical_args = Vec::with_capacity(args.len() + 1);
                        physical_args.push(recv);
                        physical_args.extend(args.iter().copied());
                        let physical_params = parse_descriptor_params(&descriptor)
                            .expect("static extension descriptor must be valid");
                        let ret = ty_from_descriptor_ret(&descriptor);
                        if let Err(mismatch) = self.emit_call_descriptor_operands(
                            e,
                            1,
                            &physical_args,
                            &physical_params,
                            code,
                        ) {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw, slot_words(ret) as i32);
                        return;
                    }
                    let physical_params = parse_descriptor_params(&descriptor)
                        .expect("virtual call descriptor must be valid");
                    crate::trace_compiler!(
                        "emit",
                        "virtual {owner}.{name}{descriptor} receiver={recv} {:?} receiver_ty={:?} args={args:?}",
                        self.ir.expr(recv),
                        self.value_ty(recv),
                    );
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let jvm_name = mapped_builtin_virtual_name(&owner, &name, &descriptor);
                    if let Err(mismatch) = self.emit_descriptor_virtual_operands(
                        e,
                        crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                            owner: &owner,
                            name: jvm_name,
                            descriptor: &descriptor,
                        },
                        recv,
                        &args,
                        &physical_params,
                        code,
                    ) {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    if interface {
                        let m = self.cw.interface_methodref(&owner, jvm_name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, jvm_name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                    interface,
                    source_member,
                    source,
                } => {
                    let (owner, name, descriptor, interface) =
                        (owner.render(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("special call needs a receiver");
                    let args = args.clone();
                    let physical_params = parse_descriptor_params(&descriptor)
                        .unwrap_or_else(|| panic!("special call descriptor must be valid: owner={owner} name={name} desc={descriptor:?}"));
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if let Err(mismatch) = self.emit_descriptor_virtual_operands(
                        e,
                        crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                            owner: &owner,
                            name: &name,
                            descriptor: &descriptor,
                        },
                        recv,
                        &args,
                        &physical_params,
                        code,
                    ) {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    // A diamond `super.f()` to a superinterface DEFAULT method: `invokespecial` on an
                    // `InterfaceMethodref` (JVM allows a direct-superinterface default this way) —
                    // unless `disable` moved that body to the holder, where it is a plain static.
                    if let Some((holder, holder_desc)) = interface
                        .then(|| {
                            self.holder_call(
                                &owner,
                                &descriptor,
                                source.is_some() || source_member.is_some(),
                            )
                        })
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw + 1, slot_words(ret) as i32);
                    } else {
                        let m = if interface {
                            self.cw.interface_methodref(&owner, &name, &descriptor)
                        } else {
                            self.cw.methodref(&owner, &name, &descriptor)
                        };
                        self.mark_call_start(e, code);
                        code.invokespecial(m, aw, slot_words(ret) as i32);
                    }
                }
            },
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => self.emit_type_operation(*op, *arg, *type_operand, code),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(e, *op, *lhs, *rhs, code),
            IrExpr::PrimitiveNeg { operand, ty } => {
                self.emit_value(*operand, code);
                match ir_ty_to_jvm(ty) {
                    Ty::Long => code.lneg(),
                    Ty::Float => code.fneg(),
                    Ty::Double => code.dneg(),
                    _ => code.ineg(),
                }
            }
            IrExpr::StringConcat(parts) => {
                let parts = parts.clone();
                if parts.len() == 1 {
                    let p = parts[0];
                    if matches!(self.ir.expr(p), IrExpr::Const(IrConst::String(_))) {
                        // A lone string constant is already a `String`.
                        self.emit_value(p, code);
                    } else {
                        // A single interpolation `"$x"` → `String.valueOf(x)` (kotlinc's form).
                        let pty = self.value_ty(p);
                        self.emit_value(p, code);
                        let m = self
                            .cw
                            .methodref("java/lang/String", "valueOf", valueof_desc(pty));
                        code.invokestatic(m, slot_words(pty) as i32, 1);
                    }
                } else if self.try_emit_indy_concat(&parts, code) {
                    // Emitted `invokedynamic makeConcatWithConstants` (kotlinc's Java-9+ form).
                } else {
                    let sb = self.cw.class_ref("java/lang/StringBuilder");
                    let init = self
                        .cw
                        .methodref("java/lang/StringBuilder", "<init>", "()V");
                    // A branchy part (`"${when{…}}"`) records merge frames that would omit the
                    // StringBuilder on the stack — spill every part to a temp first, then build.
                    if parts.iter().any(|&p| self.emits_control_flow(p)) {
                        let temps = self.spill_to_temps(&parts, code);
                        code.new_obj(sb);
                        code.dup();
                        code.invokespecial(init, 0, 0);
                        for &(slot, t, _) in &temps {
                            load(t, slot, code);
                            self.append_top(t, code);
                        }
                        self.release_operand_spills(&temps);
                    } else {
                        code.new_obj(sb);
                        code.dup();
                        code.invokespecial(init, 0, 0);
                        for &p in &parts {
                            self.append_part(p, code);
                        }
                    }
                    let ts = self.cw.methodref(
                        "java/lang/StringBuilder",
                        "toString",
                        "()Ljava/lang/String;",
                    );
                    code.invokevirtual(ts, 0, 1);
                }
            }
            IrExpr::EnumEntry { classifier, name } => {
                let fq_name = classifier.render();
                let desc = format!("L{fq_name};");
                let f = self.cw.fieldref(&fq_name, name, &desc);
                code.getstatic(f, 1);
            }
            IrExpr::StaticInstance { owner, ty, field } => {
                let owner_fq = self.ir.classes[*owner as usize].fq_name();
                let ty_fq = self.ir.classes[*ty as usize].fq_name();
                let f = self.cw.fieldref(&owner_fq, field, &format!("L{ty_fq};"));
                code.getstatic(f, 1);
            }
            IrExpr::SingletonValue { classifier } => {
                let Some((owner, field)) = self.singleton_storage(*classifier) else {
                    *self.run.emit_error.borrow_mut() = Some(format!(
                        "missing JVM storage for singleton {}",
                        classifier.render()
                    ));
                    return;
                };
                let owner = owner.render();
                let ty = classifier.render();
                let f = self.cw.fieldref(&owner, &field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticInstance { owner, ty, field } => {
                let owner = crate::jvm::names::classfile_internal_name(&owner.render());
                let ty = crate::jvm::names::classfile_internal_name(&ty.render());
                let f = self.cw.fieldref(&owner, field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticField {
                owner,
                name,
                descriptor,
            } => {
                let owner = owner.render();
                let f = self.cw.fieldref(&owner, name, descriptor);
                let words = if descriptor == "J" || descriptor == "D" {
                    2
                } else {
                    1
                };
                code.getstatic(f, words);
            }
            IrExpr::EnumValues { classifier } => {
                let fq = classifier.render();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::EnumEntries { classifier } => {
                let fq = classifier.render();
                let m = self
                    .cw
                    .methodref(&fq, "getEntries", "()Lkotlin/enums/EnumEntries;");
                code.invokestatic(m, 0, 1);
            }
            IrExpr::ReifiedClassMarker {
                name,
                erased,
                kclass,
            } => {
                // kotlinc's reified placeholder: `reifiedOperationMarker(4, "T")` then the erased
                // class constant — a splicer patches the pair with the call-site class.
                code.push_int(4, self.cw);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
                code.ldc_class(&erased.render(), self.cw);
                if *kclass {
                    let reflection = self.cw.methodref(
                        "kotlin/jvm/internal/Reflection",
                        "getOrCreateKotlinClass",
                        "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
                    );
                    code.invokestatic(reflection, 1, 1);
                }
            }
            IrExpr::ReifiedTypeOp {
                cast,
                negated,
                arg,
                name,
                erased,
            } => {
                self.emit_value(*arg, code);
                // kotlinc's reified is/as placeholder: marker(3) + instanceof, marker(1) + checkcast.
                code.push_int(if *cast { 1 } else { 3 }, self.cw);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
                let ci = self.cw.class_ref(&erased.render());
                if *cast {
                    code.checkcast(ci);
                } else {
                    code.instance_of(ci);
                    if *negated {
                        code.push_int(1, self.cw);
                        code.ixor();
                    }
                }
            }
            IrExpr::EnumValueOf {
                classifier,
                arg,
                declaration,
            } => {
                let fq = classifier.render();
                if *declaration == crate::ir::EnumValueOfDeclaration::StandardLibraryTopLevel {
                    self.mark_inline_call_site_line(e, code);
                }
                self.emit_value(*arg, code);
                let m = self
                    .cw
                    .methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                // Both declarations reach the same `E.valueOf`, and they take opposite line
                // rules: the classifier's own MEMBER is an ordinary dispatch, so the call's line
                // returns at the invoke; the standard library's top-level `enumValueOf<E>` is
                // INLINE, so what follows is its expansion and kotlinc marks the call site
                // instead. That is why the selected declaration is recorded rather than inferred.
                match declaration {
                    crate::ir::EnumValueOfDeclaration::Member => self.mark_dispatch_line(e, code),
                    crate::ir::EnumValueOfDeclaration::StandardLibraryTopLevel => {}
                }
                code.invokestatic(m, 1, 1);
            }
            IrExpr::When { branches } => {
                if !self.emit_short_circuit_value(e, code) {
                    self.emit_when(e, branches, false, code);
                }
            }
            // Block in value position: run its statements for effect, leave the trailing value on the
            // stack. Scope block-locals (restore the slot map) so they don't leak into outer frames.
            IrExpr::Block { stmts, value } => {
                self.link_safe_call_chain(e, code);
                let enclosing_statement_line = self.statement_line;
                let saved = self.open_slot_scope();
                self.block_depth += 1;
                let mut dead = false;
                for s in stmts {
                    self.mark_statement_line(*s, code);
                    // A statement nets zero on the operand stack (its value is stored/discarded). Reset
                    // the tracked height to that baseline afterward: raw spliced control flow is opaque
                    // to the builder's linear counter and can leave `cur_stack` drifted above the real,
                    // verified-balanced height. Later emission still relies on accurate physical stack
                    // accounting even though final-body analysis owns verifier frames.
                    let base = code.stack_height();
                    self.emit(*s, code);
                    if self.discarding_diverges(*s) {
                        dead = true;
                        break;
                    }
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        self.mark_statement_line(*v, code);
                        self.emit_value(*v, code);
                    }
                }
                self.close_scope_locals(code);
                self.block_depth -= 1;
                self.restore_slot_scope(saved);
                self.statement_line = enclosing_statement_line;
            }
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } => {
                // This lambda becomes a real closure. Record the implementation so the dead-lambda
                // pass keeps it; separately record only the indy realization for target-version checks.
                self.run.used_lambdas.borrow_mut().insert(*impl_fn);
                let function_adapter = sam.as_ref().is_some_and(|target| target.function_adapter);
                let lambda_mode =
                    if function_adapter || (sam.is_none() && is_high_arity_function(*arity)) {
                        LambdaMode::Class
                    } else {
                        self.lambda_modes.for_sam(sam.is_some())
                    };
                if lambda_mode == LambdaMode::Indy {
                    self.run.used_indy_lambdas.borrow_mut().insert(*impl_fn);
                }
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params = jvm_function_params(self.ir, *impl_fn);
                let impl_ret = jvm_declared_ty(&f.ret);
                // The impl method's parameters are the captured variables (bound at the call site)
                // followed by the lambda's own parameters. Only the latter form the SAM/instantiated
                // method types; the captures parameterize the `invokedynamic` itself.
                // The IR carries the exact capture list. Do not reconstruct this boundary from the
                // source arity: a suspend implementation has an additional physical Continuation
                // parameter, which belongs to the SAM side of the boundary rather than the captures.
                let n_cap = captures.len();
                if impl_params.len() < n_cap {
                    self.run.set_emit_error(
                        "lambda implementation has fewer parameters than captured values"
                            .to_string(),
                    );
                    return;
                }
                let (cap_tys, lam_tys) = impl_params.split_at(n_cap);
                let impl_desc = method_descriptor(&impl_params, impl_ret);
                // For a Kotlin lambda the target is `FunctionN.invoke` (samMethodType erased to
                // `(Object,…)Object`, instantiatedMethodType the boxed actuals); for a user SAM
                // conversion the target is the interface's single method, whose descriptor is the
                // lambda's concrete signature (no erasure/boxing).
                let (iface, sam_method, sam_desc, inst_desc) = match sam {
                    Some(target) => {
                        // `samMethodType` is the INTERFACE method's (erased) descriptor — NOT the
                        // lambda's — so a SAM with parameters (or a generic SAM erased to `Object`)
                        // matches the abstract method the metafactory must implement.
                        let (sam_parameters, sam_result) = self
                            .ir
                            .lambda_sam_jvm_signature
                            .get(impl_fn)
                            .map(|(parameters, result)| (parameters.as_slice(), *result))
                            .unwrap_or((
                                target.declared_parameters.as_slice(),
                                target.declared_result,
                            ));
                        // A suspend SAM method has the ordinary Kotlin semantic signature carried
                        // by FIR/common IR, but its JVM interface slot is CPS-shaped just like a
                        // suspend declaration: one trailing Continuation and Object return.  The
                        // suspend pass has already applied the same transformation to `impl_fn`, so
                        // realize the selected target at this platform boundary before comparing
                        // arities.  Do not put the synthetic parameter into FirSamConversion — it is
                        // not a Kotlin parameter and other backends need not share this ABI.
                        let mut sam_parameters = jvm_tys(sam_parameters);
                        let sam_result = if target.suspend {
                            sam_parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
                            Ty::obj("java/lang/Object")
                        } else {
                            jvm_declared_ty(&sam_result)
                        };
                        let sam_desc = method_descriptor(&sam_parameters, sam_result);
                        // `instantiatedMethodType` describes the specialization of the ERASED SAM
                        // method, not merely the lifted implementation's primitive signature. A
                        // generic interface slot can erase to `Object` while checking substitutes a
                        // scalar (`Comparator<in Int>` is the standard example). Advertising `int`
                        // for that reference slot asks LambdaMetafactory to specialize a reference
                        // parameter as a primitive and fails while the bootstrap is linked. Keep the
                        // lambda body's semantic scalar — the implementation handle may still accept
                        // it and the metafactory supplies the ordinary wrapper adapter — but spell the
                        // instantiated boundary with the wrapper wherever the SAM descriptor says the
                        // physical slot is a reference. This reads only descriptor SHAPE, so source,
                        // sibling-module, and dependency interfaces all take the same path.
                        let Some((sam_params, sam_ret)) =
                            crate::jvm::names::parse_method_descriptor(&sam_desc)
                        else {
                            self.run
                                .set_emit_error("selected SAM descriptor is malformed".to_string());
                            return;
                        };
                        crate::trace_compiler!(
                            "emit",
                            "selected SAM impl={impl_name} own={lam_tys:?} target={}.{}{} physical_params={sam_params:?}",
                            target.classifier,
                            target.method,
                            sam_desc,
                        );
                        if sam_params.len() != lam_tys.len() {
                            self.run.set_emit_error(
                                "selected SAM descriptor has the wrong parameter count".to_string(),
                            );
                            return;
                        }
                        let params: String = lam_tys
                            .iter()
                            .zip(sam_params)
                            .map(|(&logical, physical)| {
                                if descriptor_is_reference(physical) {
                                    boxed_descriptor(logical)
                                } else {
                                    type_descriptor(logical)
                                }
                            })
                            .collect();
                        let ret = if sam_ret == "V" {
                            // Kotlin models a Java `void` SAM result as `Unit`, and a checked lambda
                            // body can therefore materialize `kotlin.Unit`. The instantiated method
                            // type still exposes the interface's physical `void` boundary; the
                            // metafactory discards any implementation value.
                            "V".to_string()
                        } else if descriptor_is_reference(sam_ret) {
                            boxed_descriptor(impl_ret)
                        } else {
                            type_descriptor(impl_ret)
                        };
                        let inst_desc = format!("({params}){ret}");
                        (
                            target.classifier.render(),
                            target.method.clone(),
                            sam_desc,
                            inst_desc,
                        )
                    }
                    None => {
                        let iface = jvm_function_interface(*arity);
                        let inst_params: Vec<String> =
                            lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc =
                            format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (
                            iface,
                            "invoke".to_string(),
                            jvm_function_invoke_descriptor(*arity),
                            inst_desc,
                        )
                    }
                };
                crate::trace_compiler!(
                    "emit",
                    "lambda indy impl={impl_name}{impl_desc} captures={cap_tys:?} own={lam_tys:?} target={iface}.{sam_method}{sam_desc} instantiated={inst_desc}"
                );
                // The impl method lives on whichever class owns it (a class-member lambda's impl is a
                // method of the enclosing class, so it can access that class's privates); top-level
                // lambdas keep theirs on the file facade.
                let impl_class = self.ir.classes.iter().find(|c| c.methods.contains(impl_fn));
                let impl_owner_is_interface = impl_class.is_some_and(|c| c.is_interface);
                let impl_owner = impl_class
                    .map(|c| c.fq_name())
                    .unwrap_or_else(|| self.facade.clone());
                if lambda_mode == LambdaMode::Class {
                    // Common lowering records the source lambda's stable lexical origin. Consume that
                    // edge directly: a source lambda lowered into multiple constructors keeps one name
                    // and identity, and no generated method spelling or value table is searched here.
                    let origin = self.ir.lambda_origins.get(impl_fn);
                    let (internal, identity) = if let Some(origin) = origin {
                        let ordinal = origin.ordinal + 1;
                        // A class-initialization origin (a property initializer or an init block)
                        // has the EMPTY enclosing name — kotlinc emits no function segment there
                        // (`C$prop$1`, `C$local$1`, `C$1`), so an empty segment is dropped, never
                        // printed as `C$$1`.
                        let mut internal = impl_owner.clone();
                        for segment in [
                            Some(origin.enclosing_name.as_str()),
                            origin.binding_name.as_deref(),
                        ]
                        .into_iter()
                        .flatten()
                        .filter(|segment| !segment.is_empty())
                        {
                            internal.push('$');
                            internal.push_str(segment);
                        }
                        internal.push('$');
                        internal.push_str(&ordinal.to_string());
                        (internal, LambdaClassIdentity::Source(origin.identity))
                    } else {
                        // Backend-synthesized lambdas have no source expression. Their implementation
                        // id is already the exact stable identity; its generated name is serialization
                        // input only for this JVM artifact boundary.
                        let (enclosing, index) = impl_name
                            .split_once("$lambda$")
                            .map(|(head, tail)| {
                                (head.to_string(), tail.parse::<u32>().unwrap_or(0))
                            })
                            .unwrap_or_else(|| (impl_name.clone(), 0));
                        (
                            format!("{impl_owner}${enclosing}${}", index + 1),
                            LambdaClassIdentity::Synthetic(*impl_fn),
                        )
                    };
                    self.run.lambda_classes.borrow_mut().push(LambdaClassPlan {
                        internal: internal.clone(),
                        iface: iface.clone(),
                        sam_method: sam_method.clone(),
                        sam_desc: sam_desc.clone(),
                        impl_owner: impl_owner.clone(),
                        impl_name: impl_name.clone(),
                        impl_desc: impl_desc.clone(),
                        captures: cap_tys.to_vec(),
                        arity: *arity as u32,
                        kotlin_function: sam.is_none(),
                        function_adapter,
                        identity,
                        owner_is_interface: impl_owner_is_interface,
                    });
                    if captures.is_empty() {
                        // Nothing captured, so every evaluation yields the same instance — kotlinc
                        // holds it in a static and the call site just reads it.
                        let field =
                            self.cw
                                .fieldref(&internal, "INSTANCE", &format!("L{internal};"));
                        code.getstatic(field, 1);
                        let target = self.cw.class_ref(&iface);
                        code.checkcast(target);
                    } else {
                        let class_index = self.cw.class_ref(&internal);
                        code.new_obj(class_index);
                        code.dup();
                        for &c in captures {
                            self.emit_value(c, code);
                        }
                        let cap_descs: String =
                            cap_tys.iter().map(|t| type_descriptor(*t)).collect();
                        let cap_words: i32 = cap_tys.iter().map(|t| slot_words(*t) as i32).sum();
                        let ctor =
                            self.cw
                                .methodref(&internal, "<init>", &format!("({cap_descs})V"));
                        // `arg_words` excludes the receiver, which `invokespecial` accounts for
                        // itself; counting it here leaves the constructed lambda invisible to
                        // `max_stack` for the rest of the enclosing method.
                        code.invokespecial(ctor, cap_words, 0);
                    }
                    return;
                }
                // kotlinc interns the BOOTSTRAP ARGUMENTS first (erased SAM MethodType, the impl
                // MethodHandle with its `$lambda$N` refs, the instantiated MethodType), and the
                // LambdaMetafactory handle only after them — pool order follows that visit.
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_ref = if impl_owner_is_interface {
                    self.cw
                        .interface_methodref(&impl_owner, &impl_name, &impl_desc)
                } else {
                    self.cw.methodref(&impl_owner, &impl_name, &impl_desc)
                };
                let impl_mh = self.cw.method_handle_ref(6, impl_ref);
                let inst_mt = self.cw.method_type(&inst_desc);
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory",
                    "metafactory",
                    LMF_METAFACTORY_DESC,
                );
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                // The `invokedynamic` takes the captured values and yields the interface instance.
                let cap_descs: String = cap_tys.iter().map(|t| type_descriptor(*t)).collect();
                let indy =
                    self.cw
                        .invoke_dynamic(bsm, &sam_method, &format!("({cap_descs})L{iface};"));
                let cap_words: i32 = cap_tys.iter().map(|t| slot_words(*t) as i32).sum();
                for &c in captures {
                    self.emit_value(c, code);
                }
                code.invokedynamic(indy, cap_words, 1);
            }
            IrExpr::UnitInstance => {
                let f = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            }
            IrExpr::CurrentContinuation => {
                // The CPS pass rewrites this to a `GetValue` of the continuation slot for every
                // function whose machine it owns. It leaves the node in place for a function whose
                // machine EMISSION owns — a suspension inside a body this emitter splices — because
                // which slot holds the continuation is then an emission decision: the `$completion`
                // parameter while discovering the frame, the machine's own local while building it.
                let Some(slot) = self.continuation_slot else {
                    unreachable!("CurrentContinuation outside a suspend function reaches emit")
                };
                code.aload(slot);
            }
            IrExpr::NotNullAssert { operand, message } => {
                self.emit_value(*operand, code);
                // Flow typing can prove a stable nullable scalar non-null before a later, redundant
                // `!!`. Its checked FIR assertion remains visible, but the selected smart-cast
                // conversion means the operand is already a physical JVM scalar by this point. A
                // scalar cannot be null, and passing its stack word to Intrinsics.checkNotNull(Object)
                // is invalid bytecode. The ordinary nullable-scalar shape is different: the assert
                // sees the boxed wrapper and an enclosing coercion unboxes only after this check.
                if self.value_ty(*operand).is_jvm_scalar() {
                    return;
                }
                code.dup();
                // A platform value narrowed to a declared non-null type names the checked expression
                // in its failure (`getenv(...) must not be null`); `x!!` has no such name and uses the
                // one-argument form. Both consume the duplicate and leave the value in place.
                let m = match message {
                    Some(message) => {
                        code.push_string(message, self.cw);
                        self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNullExpressionValue",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        )
                    }
                    None => self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "checkNotNull",
                        "(Ljava/lang/Object;)V",
                    ),
                };
                code.invokestatic(m, if message.is_some() { 2 } else { 1 }, 0);
            }
            IrExpr::LateinitCheck { operand, name } => {
                // A `lateinit var` local read: throw `UninitializedPropertyAccessException` while the slot
                // is still null. Same guard as the member-field lateinit read (`dup; ifnonnull L; ldc
                // name; invokestatic throwUninitializedPropertyAccessException; L:`).
                self.emit_value(*operand, code);
                code.dup();
                let lbl = code.new_label();
                code.ifnonnull(lbl);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "throwUninitializedPropertyAccessException",
                    "(Ljava/lang/String;)V",
                );
                code.invokestatic(m, 1, 0);
                // `value_ty` already yields the JVM type of the operand (a reference here); the surviving
                // (non-null) value is on the stack at the branch target.
                self.bind(lbl, code);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            // `return v` in value position (`x ?: return v`): emit the return; control transfers away, so
            // (like `throw`) nothing is left for the surrounding merge.
            IrExpr::Return(value) => self.emit_return_node(e, *value, code),
            IrExpr::Vararg {
                array_type,
                elements,
                spreads,
            } => vararg::emit(self, array_type, elements, spreads, code),
            IrExpr::NewArray { array_type, size } => {
                let et = array_jvm_element(array_type);
                self.emit_value(*size, code);
                if et.is_jvm_scalar() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    // Peel a nullable element's `?`: `Array<Int?>` = `Integer[]`, so the `anewarray` class
                    // is `java/lang/Integer` (the `?` only tells `Array.get`/`.set` to keep it boxed).
                    let ci = self
                        .cw
                        .class_ref(&crate::jvm::names::instanceof_internal_name(et.non_null()));
                    code.anewarray(ci);
                }
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                let catches = catches.clone();
                let parts = try_emission::TryParts {
                    body: *body,
                    catches: &catches,
                    finally: *finally,
                    result: *result,
                };
                self.emit_try(e, parts, false, code);
            }
            IrExpr::RefNew { elem, init } => {
                let (cls, fdesc) = ref_class(elem);
                let ew = slot_words(ir_ty_to_jvm(elem)) as i32;
                // A branchy initializer can't run with `[holder, holder]` on the stack — spill it.
                if self.emits_control_flow(*init) {
                    let temps = self.spill_to_temps(&[*init], code);
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    self.release_operand_spills(&temps);
                } else {
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    self.emit_value(*init, code);
                }
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, ew);
            }
            IrExpr::RefGet { holder, elem } => {
                self.emit_value(*holder, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                let ejvm = ir_ty_to_jvm(elem);
                code.getfield(f, slot_words(ejvm) as i32);
                // An `ObjectRef.element` is typed `Object`; narrow to the boxed value's reference type.
                if ejvm.is_reference()
                    && crate::jvm::names::instanceof_internal_name(ejvm) != "java/lang/Object"
                {
                    let cc = self
                        .cw
                        .class_ref(&crate::jvm::names::instanceof_internal_name(ejvm));
                    code.checkcast(cc);
                }
            }
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => {
                // A branchy value (`msg = x ?: "?"`) can't run with the holder on the stack — its
                // branch frames assume a clean stack. Spill it first (mirrors `RefNew`).
                if self.emits_control_flow(*value) {
                    let temps = self.spill_to_temps(&[*value], code);
                    self.emit_value(*holder, code);
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    self.release_operand_spills(&temps);
                } else {
                    self.emit_value(*holder, code);
                    self.emit_value(*value, code);
                }
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, slot_words(ir_ty_to_jvm(elem)) as i32);
            }
            IrExpr::InvokeFunction {
                func,
                args,
                params,
                ret,
            } => {
                let n = args.len();
                let high_arity = is_high_arity_function(n as u8);
                if args.iter().any(|&a| self.emits_control_flow(a)) {
                    // A branchy argument can't run with the function value on the stack — its merge
                    // frame would omit it. Evaluate the function + args into temps first (in order),
                    // then load and box.
                    let mut all = vec![*func];
                    all.extend(args.iter().copied());
                    let temps = self.spill_to_temps(&all, code);
                    load(temps[0].1, temps[0].0, code);
                    if high_arity {
                        code.push_int(n as i32, self.cw);
                        let object = self.cw.class_ref("java/lang/Object");
                        code.anewarray(object);
                    }
                    for (i, &(slot, t, _)) in temps[1..].iter().enumerate() {
                        if high_arity {
                            code.dup();
                            code.push_int(i as i32, self.cw);
                        }
                        load(t, slot, code);
                        let semantic = params.get(i).copied().unwrap_or(t);
                        box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, t));
                        if high_arity {
                            code.array_store(0x53, 1); // aastore
                        }
                    }
                    self.release_operand_spills(&temps);
                } else {
                    self.emit_value(*func, code);
                    if high_arity {
                        code.push_int(n as i32, self.cw);
                        let object = self.cw.class_ref("java/lang/Object");
                        code.anewarray(object);
                    }
                    for (i, &arg) in args.iter().enumerate() {
                        if high_arity {
                            code.dup();
                            code.push_int(i as i32, self.cw);
                        }
                        self.emit_value(arg, code);
                        let at = self.value_ty(arg);
                        let semantic = params.get(i).copied().unwrap_or(at);
                        // `FunctionN` parameters are erased `Object`, but wrapper selection is a
                        // semantic operation. Retaining `params` on the IR node prevents an unsigned
                        // argument from being boxed as the signed wrapper of its shared carrier.
                        box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, at));
                        if high_arity {
                            code.array_store(0x53, 1); // aastore
                        }
                    }
                }
                let iface = jvm_function_interface(n as u8);
                let m = self.cw.interface_methodref(
                    &iface,
                    "invoke",
                    &jvm_function_invoke_descriptor(n as u8),
                );
                // A function VALUE's invocation is a dispatch like any other: after the operands
                // have each marked their own line, the call's own line returns at the `invoke`.
                self.mark_dispatch_line(e, code);
                code.invokeinterface(m, if high_arity { 1 } else { n as i32 }, 1);
                // The interface returns `Object`; cast/unbox to the function's declared return type.
                // Select a scalar adapter from that semantic return before `ir_ty_to_jvm` reduces an
                // unsigned type to its signed carrier. This is the common consumer for real lambdas,
                // callable references, and property references, independent of which producer object
                // supplied the `FunctionN` implementation.
                let rt = ir_ty_to_jvm(ret);
                if rt.is_jvm_scalar() {
                    unbox_prim_from(
                        self.cw,
                        code,
                        Ty::obj("java/lang/Object"),
                        semantic_scalar_adapter(*ret, rt),
                    );
                } else {
                    match rt {
                        Ty::Unit | Ty::Nothing => code.pop(),
                        Ty::String => {
                            let ci = self.cw.class_ref("java/lang/String");
                            code.checkcast(ci);
                        }
                        _ if rt.is_array() => {
                            let ci = self.cw.class_ref(&type_descriptor(rt));
                            code.checkcast(ci);
                        }
                        Ty::Obj(internal, _) => {
                            let ci = self.cw.class_ref(&internal.render());
                            code.checkcast(ci);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn emit_array_get(&mut self, array: u32, index: u32, code: &mut CodeBuilder) {
        let element = self.array_elem(array);
        let reference_array = self.value_ty(array).is_reference_array();
        if self.must_spill_across(index) {
            self.emit_operands(&[array, index], code);
        } else {
            self.emit_value(array, code);
            self.emit_value(index, code);
        }
        let (operation, words) = array_load_op(element, reference_array);
        code.array_load(operation, words);
        if let Some(primitive) = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten()
        {
            let array_descriptor = type_descriptor(self.value_ty(array));
            let component = array_descriptor
                .strip_prefix('[')
                .unwrap_or("Ljava/lang/Object;");
            unbox_prim_from_descriptor(self.cw, code, component, primitive);
        }
    }

    fn emit_array_set(&mut self, array: u32, index: u32, value: u32, code: &mut CodeBuilder) {
        let element = self.array_elem(array);
        let reference_array = self.value_ty(array).is_reference_array();
        if self.must_spill_across(index) || self.must_spill_across(value) {
            self.emit_operands(&[array, index, value], code);
        } else {
            self.emit_value(array, code);
            self.emit_value(index, code);
            self.emit_value(value, code);
        }
        if let Some(primitive) = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten()
        {
            box_prim_free(self.cw, code, primitive);
        }
        let (operation, words) = array_store_op(element, reference_array);
        code.array_store(operation, words);
    }

    /// The operand a concatenation part really contributes. A part boxed only to satisfy
    /// `String.plus(Any?)` (`"…" + port` wraps the `Int` through a reference coercion) is
    /// concatenated as the PRIMITIVE — kotlinc's `append(I)` / indy `I` argument, never `valueOf`
    /// + `append(Object)`. The box is concat-invisible, so see through it.
    fn concat_operand(&self, e: u32) -> u32 {
        match self.ir.expr(e) {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => {
                let inner = *arg;
                let inner_ty = self.value_ty(inner);
                // An UNSIGNED value's box is NOT concat-invisible: its carrier prints in signed
                // decimal, the box's own `toString` in unsigned — keep the box.
                let unsigned_inner = self
                    .ir
                    .logical_types
                    .get(&inner)
                    .is_some_and(|t| t.is_unsigned())
                    || inner_ty.is_unsigned();
                let outer_reference = ir_ty_to_jvm(&self.value_ty(e)).is_reference();
                let inner_reference = ir_ty_to_jvm(&inner_ty).is_reference();
                // A reference operand widened to `Any?` for the `plus` parameter is the same value
                // under its own static type: kotlinc concatenates `b: String` as `String`
                // (`append(String)`, indy `Ljava/lang/String;`) and folds a widened literal into
                // the recipe. The upcast is concat-invisible too.
                if outer_reference && (inner_reference || !unsigned_inner) {
                    inner
                } else {
                    e
                }
            }
            _ => e,
        }
    }

    /// Flatten a `String.plus` chain into its leaf operands, left to right: a nested `plus` on
    /// either side and a string template (`IrExpr::StringConcat`) contribute their own parts,
    /// exactly as kotlinc's `FlattenStringConcatenationLowering` folds them into one
    /// concatenation. A boxed primitive operand contributes the primitive
    /// ([`Self::concat_operand`]).
    fn flatten_concat_parts(&self, e: u32, out: &mut Vec<u32>) {
        let e = self.concat_operand(e);
        match self.ir.expr(e) {
            IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::StringPlus,
                        ..
                    },
                dispatch_receiver: Some(receiver),
                args,
                ..
            } if args.len() == 1 => {
                let (receiver, arg) = (*receiver, args[0]);
                self.flatten_concat_parts(receiver, out);
                self.flatten_concat_parts(arg, out);
            }
            IrExpr::StringConcat(parts) => {
                for part in parts.clone() {
                    self.flatten_concat_parts(part, out);
                }
            }
            _ => out.push(e),
        }
    }

    fn append(&mut self, e: u32, code: &mut CodeBuilder) {
        let e = self.concat_operand(e);
        let ty = self.value_ty(e);
        let semantic = self.ir.logical_types.get(&e).copied().unwrap_or(ty);
        self.emit_value(e, code);
        if !semantic.is_nullable() {
            if let Some((owner, carrier)) = native_unsigned_impl_target(semantic) {
                let descriptor = method_descriptor(&[carrier], Ty::String);
                let method = self
                    .cw
                    .methodref(&owner.render(), "toString-impl", &descriptor);
                code.invokestatic(method, slot_words(carrier) as i32, 1);
                self.append_top(Ty::String, code);
                return;
            }
            // A value class rendered through its `toString-impl` is appended at its own static type,
            // which selects `append(Object)` as kotlinc does, although the rendered text is a String.
            if self.is_value_class_ty(&semantic) {
                self.append_top(Ty::obj("java/lang/Object"), code);
                return;
            }
        }
        self.append_top(ty, code);
    }

    /// One concatenation over flattened `plus` parts: `invokedynamic makeConcatWithConstants` on
    /// JVM 9+ (kotlinc's default there), else a single `StringBuilder` with one `append` per part.
    fn emit_string_plus_parts(&mut self, parts: &[u32], code: &mut CodeBuilder) {
        if self.try_emit_indy_concat(parts, code) {
            return;
        }
        let sb = self.cw.class_ref("java/lang/StringBuilder");
        // A branchy operand (`when`/`try`) can't be emitted with the `StringBuilder` on the stack — its
        // merge frames would omit it. Spill such operands to temps first.
        if parts.iter().any(|&part| self.emits_control_flow(part)) {
            let temps = self.spill_to_temps(parts, code);
            code.new_obj(sb);
            code.dup();
            let init = self
                .cw
                .methodref("java/lang/StringBuilder", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            for &(slot, t, _) in &temps {
                load(t, slot, code);
                self.append_top(t, code);
            }
            self.release_operand_spills(&temps);
        } else {
            code.new_obj(sb);
            code.dup();
            let init = self
                .cw
                .methodref("java/lang/StringBuilder", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            for &part in parts {
                self.append_part(part, code);
            }
        }
        let ts = self.cw.methodref(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        );
        code.invokevirtual(ts, 0, 1);
    }

    /// kotlinc compiles a multi-part string template (and a synthesized `toString`) to a single
    /// `invokedynamic makeConcatWithConstants` when targeting JVM 9+ — a `StringConcatFactory`
    /// bootstrap with a recipe string, `` marking each dynamic argument and literal text inline.
    /// Below JVM 9 (and for the branchy-operand shape, whose frame handling this doesn't model yet)
    /// returns `false` so the caller keeps the `StringBuilder` form.
    fn try_emit_indy_concat(&mut self, parts: &[u32], code: &mut CodeBuilder) -> bool {
        const TAG_ARG: char = '\u{1}';
        const TAG_CONST: char = '\u{2}';
        // JVM 9 = major 53; kotlinc's `-Xstring-concat` default flips to `indy-with-constants` there.
        if self.cw.major() < 53 {
            return false;
        }
        // A branchy part records a merge frame mid-build; matching kotlinc's operand-stack shape across
        // that is the same open problem as elsewhere, so leave those on the StringBuilder path.
        if parts.iter().any(|&p| self.emits_control_flow(p)) {
            return false;
        }
        if parts.iter().any(|part| {
            self.ir
                .logical_types
                .get(part)
                .is_some_and(|ty| !ty.is_nullable() && ty.is_unsigned())
        }) {
            return false;
        }
        // The recipe is itself a string CONSTANT, so it carries whatever code units the literal
        // parts hold — including an unpaired surrogate, which no Rust `String` can spell.
        let mut recipe = KtStringBuf::new();
        let mut arg_parts: Vec<u32> = Vec::new();
        for &p in parts {
            if let IrExpr::Const(IrConst::String(s)) = self.ir.expr(p) {
                // A literal carrying a recipe tag would have to move to the constants array — rare;
                // fall back rather than encode it wrong.
                if s.units()
                    .any(|u| u == TAG_ARG as u16 || u == TAG_CONST as u16)
                {
                    return false;
                }
                recipe.push_kt(s);
            } else {
                recipe.push(TAG_ARG);
                arg_parts.push(p);
            }
        }
        let recipe = recipe.finish();
        let arg_descs: String = arg_parts
            .iter()
            .map(|&p| type_descriptor(self.value_ty(p)))
            .collect();
        // kotlinc interns the recipe (the bootstrap's static argument) BEFORE the bootstrap method
        // handle, so intern in that order to match its constant-pool layout.
        let recipe_const = self.cw.const_string_kt(&recipe);
        let mh = self.cw.method_handle_static(
            "java/lang/invoke/StringConcatFactory",
            "makeConcatWithConstants",
            "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;\
             Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/invoke/CallSite;",
        );
        let bsm = self.cw.add_bootstrap(mh, vec![recipe_const]);
        let indy = self.cw.invoke_dynamic(
            bsm,
            "makeConcatWithConstants",
            &format!("({arg_descs})Ljava/lang/String;"),
        );
        let mut arg_words = 0i32;
        for &p in &arg_parts {
            let ty = self.value_ty(p);
            self.emit_value(p, code);
            arg_words += slot_words(ty) as i32;
        }
        code.invokedynamic(indy, arg_words, 1);
        true
    }

    /// Append one string-template part to the `StringBuilder` beneath it. A single-character string
    /// constant appends as a `char` (kotlinc emits `append(C)` with the char constant, not `append(String)`).
    fn append_part(&mut self, p: u32, code: &mut CodeBuilder) {
        // "single character" is one UTF-16 code UNIT — the width of a `Char` — so a supplementary
        // character (two units) stays on the `append(String)` path, as it must.
        let single_unit = if let IrExpr::Const(IrConst::String(s)) = self.ir.expr(p) {
            s.single_unit()
        } else {
            None
        };
        if let Some(unit) = single_unit {
            code.push_int(unit as i32, self.cw);
            self.append_top(Ty::Char, code);
        } else {
            self.append(p, code);
        }
    }

    /// Append a value already on the operand stack (of type `ty`) to a `StringBuilder` beneath it.
    fn append_top(&mut self, ty: Ty, code: &mut CodeBuilder) {
        // A `String` value reaches here either as `Ty::String` or as `Ty::Obj("java/lang/String")` —
        // the latter when its type was parsed from a method-return descriptor (e.g. a classpath call
        // or the data-class `Arrays.toString(field)` wrapper). Both must pick the `append(String)`
        // overload kotlinc uses, not the less-specific `append(Object)`.
        let is_string = matches!(ty, Ty::String)
            || matches!(ty, Ty::Obj(n, _) if n == "java/lang/String" || n == "kotlin/String");
        let desc = match ty {
            _ if is_string => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/StringBuilder;",
            Ty::Long => "(J)Ljava/lang/StringBuilder;",
            Ty::Boolean => "(Z)Ljava/lang/StringBuilder;",
            Ty::Char => "(C)Ljava/lang/StringBuilder;",
            Ty::Double => "(D)Ljava/lang/StringBuilder;",
            Ty::Float => "(F)Ljava/lang/StringBuilder;",
            _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
        };
        let m = self.cw.methodref("java/lang/StringBuilder", "append", desc);
        code.invokevirtual(m, slot_words(ty) as i32, 1);
    }

    /// Whether an operand held on the stack BELOW `e` must be spilled to a temp instead
    /// a `try` in the subtree — the JVM clears the operand stack on handler entry, so a held value
    /// would be lost — a transfer to a loop target outside the subtree, or a suspension whose state
    /// machine cannot carry an unrecorded JVM operand prefix across resumption. An ordinary inline
    /// branch is not such a boundary: final-body dataflow carries the live prefix through its edges.
    /// Conservative: the spill path is always correct, only byte-parity with kotlinc is deferred.
    fn must_spill_across(&self, e: u32) -> bool {
        if self.machine_suspensions.contains(&e) {
            return true;
        }
        match self.ir.expr(e) {
            IrExpr::Try { .. } | IrExpr::Break { .. } | IrExpr::Continue { .. } => true,
            IrExpr::Call {
                callee:
                    Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline,
                    },
                dispatch_receiver,
                args,
            } if inline.can_inline() => {
                self.bodies
                    .body(&owner.render(), name, descriptor)
                    .is_some_and(|body| !body.handlers.is_empty())
                    || dispatch_receiver.is_some_and(|receiver| self.must_spill_across(receiver))
                    || args
                        .iter()
                        .any(|&argument| self.must_spill_across(argument))
            }
            _ => {
                let mut spill = false;
                crate::ir::for_each_child(&self.ir.exprs, e, &mut |c| {
                    spill = spill || self.must_spill_across(c);
                });
                spill
            }
        }
    }

    /// Whether emitting `e` introduces non-linear JVM control flow anywhere in its subtree. Operand
    /// sequences use this physical fact to keep an earlier value off the stack while nested branches,
    /// handlers, or inline splices execute. Stack-map frames themselves are computed from the final body.
    fn emits_control_flow(&self, e: u32) -> bool {
        use IrBinOp::*;
        match self.ir.expr(e) {
            IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. } => true,
            // The multi-part `StringConcat` itself spills branchy parts internally, so as a whole it
            // leaves only its `String` result — but a parent operand sequence still must treat it as
            // branchy if any part is (it builds the StringBuilder mid-stack otherwise).
            IrExpr::StringConcat(parts) => parts.iter().any(|&p| self.emits_control_flow(p)),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_jvm_scalar())
                    // `===`/`!==` always emits a branch+merge frame — the `if_acmp*` path (references)
                    // and the value-compare path it remaps to for primitives both do.
                    || matches!(op, RefEq | RefNe)
                    // `x == null`/`x != null` emits an `ifnull`/`ifnonnull` branch+merge frame.
                    || (matches!(op, Eq | Ne)
                        && (matches!(self.ir.expr(*lhs), IrExpr::Const(IrConst::Null))
                            || matches!(self.ir.expr(*rhs), IrExpr::Const(IrConst::Null))))
                    || self.emits_control_flow(*lhs) || self.emits_control_flow(*rhs)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => {
                // Preserve the reference compiler's temp layout when a spliced host or lambda branches.
                // This is no longer a verifier requirement: final-body dataflow can carry a live prefix,
                // and `must_spill_across` separately identifies handlers/external transfers that cannot.
                let splice_branches = match callee {
                    Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline,
                    } if inline.can_inline() => {
                        args.iter().any(|&a| {
                            matches!(self.ir.expr(a),
                                IrExpr::Lambda { inline_body: Some(b), .. } if self.emits_control_flow(*b))
                        }) || self
                            .bodies
                            .body(&owner.render(), name, descriptor)
                            .and_then(|b| crate::jvm::inline::disassemble(&b.code))
                            .is_some_and(|ins| {
                                ins.iter()
                                    .any(|i| !matches!(i, crate::jvm::inline::Insn::Plain { .. }))
                            })
                    }
                    _ => false,
                };
                matches!(
                    callee,
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::Assert { .. },
                        ..
                    }
                ) || splice_branches
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
            IrExpr::New { args, .. } => args.iter().any(|&a| self.emits_control_flow(a)),
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
                    || self.lateinit_direct_field_read(&owner.render(), name)
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
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.emits_control_flow(*value)
            }
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
            IrExpr::RefNew { init, .. } => self.emits_control_flow(*init),
            IrExpr::Throw { operand } => self.emits_control_flow(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.emits_control_flow(a)),
            IrExpr::NewArray { size, .. } => self.emits_control_flow(*size),
            IrExpr::Return(v) => v.is_some_and(|x| self.emits_control_flow(x)),
            IrExpr::Variable { init, .. } => init.is_some_and(|i| self.emits_control_flow(i)),
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| self.emits_control_flow(s))
                    || value.is_some_and(|v| self.emits_control_flow(v))
            }
            _ => false, // Const, GetValue, GetStatic, EnumEntry, EnumValues — straight-line
        }
    }

    /// Push `ops` onto the stack in order. If any later op introduces control flow, evaluate all ops
    /// into temps first, then load them, keeping the operand baseline empty across nested branches.
    fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        self.emit_operands_adapted(None, ops, code, |_, _, _| {});
    }

    /// Emit one checked static initializer through its declared JVM storage boundary. Facade,
    /// class/object, interface, and enum `<clinit>` paths share this operation so boxing and carrier
    /// selection cannot diverge by owner shape.
    fn emit_static_initializer_store(
        &mut self,
        owner: &str,
        field: &crate::ir::IrStatic,
        code: &mut CodeBuilder,
    ) {
        self.emit_value(field.init, code);
        if self.diverges(field.init) {
            return;
        }
        let physical = jvm_declared_ty(&field.ty);
        self.adapt_physical_operand_for(field.init, self.value_ty(field.init), physical, code);
        let reference = self
            .cw
            .fieldref(owner, &field.name, &type_descriptor(physical));
        code.putstatic(reference, slot_words(physical) as i32);
    }

    /// Frame-safe operand sequencing with one representation adapter applied immediately after each
    /// value is pushed. Keeping the adapter inside the shared spill/load loop is essential for
    /// category-changing bridges such as primitive boxing: a wide left operand cannot be repaired
    /// after a right operand has landed above it, and a branchy right operand still requires both
    /// source expressions to be evaluated with an empty stack. Consumers supply only the boundary
    /// adapter; evaluation order, frame safety, temporary ownership, and cleanup remain centralized.
    fn emit_operands_adapted<F>(
        &mut self,
        default_plan: Option<(
            u32,
            &[crate::jvm::default_call_operands::DefaultOperandOrigin],
        )>,
        ops: &[u32],
        code: &mut CodeBuilder,
        mut adapt: F,
    ) where
        F: FnMut(&mut Self, Ty, &mut CodeBuilder),
    {
        let mut inside_run = false;
        if ops.iter().skip(1).any(|&o| self.emits_control_flow(o)) {
            let temps = self.spill_to_temps(ops, code);
            for (operand_index, (&(slot, t, _), _)) in temps.iter().zip(ops).enumerate() {
                self.mark_synthesized_operand_run(
                    default_plan,
                    operand_index,
                    &mut inside_run,
                    code,
                );
                load(t, slot, code);
                adapt(self, t, code);
            }
            self.release_operand_spills(&temps);
        } else {
            for (operand_index, &o) in ops.iter().enumerate() {
                self.mark_synthesized_operand_run(
                    default_plan,
                    operand_index,
                    &mut inside_run,
                    code,
                );
                self.emit_value(o, code);
                adapt(self, self.value_ty(o), code);
            }
        }
    }

    /// Adapter for an operand that must occupy an erased/reference comparison slot. Reference values
    /// are already in the required representation; [`box_prim_free`] changes only JVM scalars.
    fn box_scalar_operand(&mut self, ty: Ty, code: &mut CodeBuilder) {
        box_prim_free(self.cw, code, ty);
    }

    /// Push the two operands of a referential `===`/`!==` that compares object refs, BOXING whichever
    /// side is a primitive right where it lands — kotlinc's shape for a mixed pair (`aload_0; iload_1;
    /// Integer.valueOf; if_acmpne`), which it accepts with only an "identity equality … can be unstable
    /// because of implicit boxing" warning. Boxing has to happen per operand rather than once at the
    /// end: a `Long`/`Double` left operand occupies two stack words, so a boxed right operand cannot be
    /// swapped past it. The shared adapted-operand path owns evaluation order, frame-aware spilling,
    /// and temporary cleanup; identity supplies only the primitive-to-reference adapter.
    fn emit_identity_operands(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        self.emit_operands_adapted(None, &[lhs, rhs], code, Self::box_scalar_operand);
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

    fn emit_unsigned_compare_to_virtual(
        &mut self,
        owner: &str,
        name: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        if name != "compareTo" || args.len() != 1 {
            return false;
        }
        // `UByte`/`UShort` compare like kotlinc does: zero-extend both sides into an `int` and use the
        // `UInt` comparator (they have no `compareUnsigned` of their own on the JDK side).
        let (logical, jdk_owner, prim_desc, repr) = match owner {
            "kotlin/UByte" => (Ty::UByte, "java/lang/Integer", "I", Ty::Int),
            "kotlin/UShort" => (Ty::UShort, "java/lang/Integer", "I", Ty::Int),
            "kotlin/UInt" => (Ty::UInt, "java/lang/Integer", "I", Ty::Int),
            "kotlin/ULong" => (Ty::ULong, "java/lang/Long", "J", Ty::Long),
            _ => return false,
        };
        self.emit_unsigned_operand(recv, logical, repr, code);
        self.emit_unsigned_operand(args[0], logical, repr, code);
        let m = self.cw.methodref(
            jdk_owner,
            "compareUnsigned",
            &format!("({prim_desc}{prim_desc})I"),
        );
        code.invokestatic(m, (slot_words(repr) * 2) as i32, 1);
        true
    }

    fn emit_unsigned_operand(&mut self, expr: u32, logical: Ty, repr: Ty, code: &mut CodeBuilder) {
        let from = self.value_ty(expr);
        self.emit_value(expr, code);
        if from.is_reference() {
            let Some(owner) = logical.kotlin_class_internal().map(|n| n.render()) else {
                return;
            };
            let desc = format!("(){}", type_descriptor(logical));
            let cls = self.cw.class_ref(&owner);
            code.checkcast(cls);
            let m = self.cw.methodref(&owner, "unbox-impl", &desc);
            code.invokevirtual(m, 0, slot_words(logical) as i32);
        } else {
            emit_num_conv(from, logical.scalar_value_repr().unwrap_or(repr), code);
        }
        // A `UByte`/`UShort` now sits on the stack sign-extended from its `byte`/`short`; mask it into
        // the unsigned value the comparator expects.
        if let Some(mask) = logical.unsigned_widen_mask() {
            code.push_int(mask, self.cw);
            code.iand();
        }
    }

    fn emit_binop(
        &mut self,
        expression: u32,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                // A branchy RHS (`result*31 + <nullable-field hashCode ternary>`): keep the numeric LHS on
                // the operand stack across the RHS's branch — matching kotlinc. Final-bytecode analysis
                // carries that prefix through every edge, so emitter-side frame annotation is unnecessary. A
                // non-branchy RHS (or a branchy LHS) keeps the ordinary `emit_operands` path (spill only if
                // needed) — bytecode unchanged for the common case. NOT applicable when the RHS can enter
                // an exception handler, suspend, or transfer to an enclosing loop (`must_spill_across`):
                // those boundaries cannot carry the held LHS to this operation.
                if self.emits_control_flow(rhs)
                    && !self.emits_control_flow(lhs)
                    && !self.must_spill_across(rhs)
                {
                    self.emit_value(lhs, code);
                    self.emit_value(rhs, code);
                } else {
                    self.emit_operands(&[lhs, rhs], code);
                }
                match lt {
                    Ty::Long => match op {
                        Add => code.ladd(),
                        Sub => code.lsub(),
                        Mul => code.lmul(),
                        Div => code.ldiv(),
                        Rem => code.lrem(),
                        _ => unreachable!(),
                    },
                    Ty::Double => match op {
                        Add => code.dadd(),
                        Sub => code.dsub(),
                        Mul => code.dmul(),
                        Div => code.ddiv(),
                        Rem => code.drem(),
                        _ => unreachable!(),
                    },
                    Ty::Float => match op {
                        Add => code.fadd(),
                        Sub => code.fsub(),
                        Mul => code.fmul(),
                        Div => code.fdiv(),
                        Rem => code.frem(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Add => code.iadd(),
                        Sub => code.isub(),
                        Mul => code.imul(),
                        Div => code.idiv(),
                        Rem => code.irem(),
                        _ => unreachable!(),
                    },
                }
                // JVM int-category arithmetic always leaves a full `int`, even when both inputs
                // use the Char/Byte/Short carrier. Checked FIR's result type remains authoritative:
                // normalize only after the operation (`Char + Int` wraps modulo 2^16), while an
                // ordinary `Char - Char : Int` remains untouched.
                let arithmetic_result = match lt {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    other => other,
                };
                let semantic_result = self
                    .ir
                    .logical_types
                    .get(&expression)
                    .copied()
                    .unwrap_or(arithmetic_result);
                emit_num_conv(
                    arithmetic_result,
                    ir_ty_to_jvm(&semantic_result.non_null()),
                    code,
                );
            }
            And | Or => {
                // Evaluate lhs, hold it in a temp while a branchy rhs is emitted, then combine. The
                // temp is dead afterwards, so release it so it doesn't leak into later merge frames.
                // Without this, a `false`/`else` path that never assigned the temp reaches a merge
                // whose frame claims it's defined → VerifyError.
                self.emit_value(lhs, code);
                let temp = self.frame.enter_temp(TempRole::BooleanOperand, Ty::Boolean);
                let tmp = temp.slot();
                let lease = self.lease_frame_temporary(temp, Ty::Boolean);
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And {
                    code.iand()
                } else {
                    code.ior()
                }
                self.release_temporary(lease);
            }
            BitAnd | BitOr | BitXor => {
                self.emit_binary_operands_with_live_prefix(lhs, rhs, code);
                match lt {
                    Ty::Long => match op {
                        BitAnd => code.land(),
                        BitOr => code.lor(),
                        BitXor => code.lxor(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        BitAnd => code.iand(),
                        BitOr => code.ior(),
                        BitXor => code.ixor(),
                        _ => unreachable!(),
                    },
                }
            }
            Shl | Shr | Ushr => {
                self.emit_operands(&[lhs, rhs], code); // shift amount is an `Int`
                match lt {
                    Ty::Long => match op {
                        Shl => code.lshl(),
                        Shr => code.lshr(),
                        Ushr => code.lushr(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Shl => code.ishl(),
                        Shr => code.ishr(),
                        Ushr => code.iushr(),
                        _ => unreachable!(),
                    },
                }
            }
            Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let f = code.new_label();
        // Every comparison that needs a conditional branch goes through the same classifier and
        // operand emitter used by `if`/`while`/`when`. Value position merely supplies a false target
        // and materializes the resulting 0/1. This is intentionally one semantic path: keeping separate
        // null/reference/numeric case tables here previously let zero-left ordering acquire a different
        // node-shape rule depending on whether the comparison happened to be an `if` condition.
        if self.emit_non_structural_compare_branch(op, lhs, rhs, f, false, code) {
            self.materialize_cmp_bool(f, code);
            return;
        }

        // The shared emitter returns false only for structural equality between two non-null
        // references. `Intrinsics.areEqual` already produces the Boolean value kotlinc returns in value
        // position, so branching merely to reconstruct it would be longer and less faithful.
        self.emit_structural_equality(lhs, rhs, code);
        if op == IrBinOp::Ne {
            code.push_int(1, self.cw);
            code.ixor();
        }
    }

    /// Tail of a value-position comparison: the caller has emitted a conditional branch to `f` taken
    /// exactly when the comparison is FALSE. Fall through to `iconst_1`, jump over the `iconst_0` the
    /// `f` arm pushes — kotlinc's polarity (`if_icmpne; iconst_1; goto; iconst_0`), which keeps the
    /// null, referential and numeric arms byte-identical to it at no extra instruction cost.
    fn materialize_cmp_bool(&mut self, f: Label, code: &mut CodeBuilder) {
        // The branch popped its operands — this is the height on BOTH merge paths (the `f` branch and
        // the fall-through). The 0/1 booleans below each leave exactly one value, so the tracker must be
        // reset to this height at `bind(f)`; otherwise the linear counter carries the fall-through's
        // `push 1` past the `goto`, drifting `cur_stack` +1 (harmless for max_stack, but it makes
        // `stack_height()` over-report, which the branchy-inline baseline check relies on).
        let merged = code.stack_height().max(0) as u16;
        let end = code.new_label();
        code.push_int(1, self.cw);
        code.goto(end);
        self.bind(f, code);
        code.set_stack(merged);
        code.push_int(0, self.cw);
        self.bind(end, code);
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

    /// Emit the comparison `lhs <op> rhs` directly as a single conditional jump to `target`, taken when
    /// the comparison's result equals `jt` — no 0/1 boolean is materialized. Mirrors `emit_compare`'s
    /// operand/3-way/null/ref handling but ends in one fused branch with the right polarity.
    fn emit_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        if self.emit_non_structural_compare_branch(op, lhs, rhs, target, jt, code) {
            return;
        }

        // The shared classifier leaves only non-null structural `==`/`!=` here. Unlike value position,
        // a condition must consume `Intrinsics.areEqual` with one final branch; the comparison's
        // requested polarity determines whether equality means taking or skipping the target.
        debug_assert!(matches!(op, IrBinOp::Eq | IrBinOp::Ne));
        self.emit_structural_equality(lhs, rhs, code);
        if (op == IrBinOp::Eq) == jt {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
    }

    /// Emit every comparison except non-null structural reference equality as a branch.
    ///
    /// Returning `false` is a deliberately narrow contract: both operands are non-null references and
    /// `op` is `==`/`!=`, so the caller must emit `Intrinsics.areEqual` in the form appropriate to its
    /// consumer. All null, identity and numeric classification lives here so comparison semantics cannot
    /// drift based on whether an identical IR node is consumed as a Boolean value or as control flow.
    fn emit_non_structural_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        // `x == null` / `x != null` / `x === null` / `x !== null` → single-operand `ifnull`/`ifnonnull`
        // (kotlinc's form), NOT `aconst_null; if_acmp*`. Computed up front so the referential-identity
        // path below doesn't claim a null comparison (a `null` literal's type is a reference).
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        // Referential identity (`===`/`!==`) on two non-null references — or on a mixed reference/
        // primitive pair, whose primitive side boxes first — → `if_acmpeq`/`if_acmpne`.
        if matches!(op, RefEq | RefNe)
            && identity_compares_refs(lt, self.value_ty(rhs))
            && !lhs_null
            && !rhs_null
        {
            self.emit_identity_operands(lhs, rhs, code);
            if (op == RefEq) == jt {
                code.if_acmpeq(target);
            } else {
                code.if_acmpne(target);
            }
            return true;
        }
        let op = match op {
            RefEq => Eq,
            RefNe => Ne,
            o => o,
        };
        if matches!(op, Eq | Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            // A physical primitive arises here only for identity (`x === null`/`x !== null`): kotlinc
            // accepts that with an always-false/true warning, whereas structural `x == null` is
            // rejected by the front end. Use the same adapted-operand primitive as mixed identity so
            // the `ifnull` reference slot receives a box; reference structural operands are a no-op.
            self.emit_operands_adapted(None, &[operand], code, Self::box_scalar_operand);
            if (op == Eq) == jt {
                code.ifnull(target);
            } else {
                code.ifnonnull(target);
            }
            return true;
        }
        // Structural equality's value result has different optimal consumers: value position can use it
        // directly, while control flow branches on it. Tell the caller to select that final operation;
        // the semantic classification itself still occurs once, here.
        if matches!(op, Eq | Ne) && !(lt.is_jvm_scalar() && self.value_ty(rhs).is_jvm_scalar()) {
            return false;
        }
        self.emit_numeric_compare_branch(op, lhs, rhs, target, jt, code);
        true
    }

    /// Put the null-safe structural equality result for two references on the operand stack.
    fn emit_structural_equality(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        // Spill if rhs is branchy (`x == when { ... }`) so lhs is not live across its merge frames.
        self.emit_operands_adapted(None, &[lhs, rhs], code, Self::box_scalar_operand);
        let m = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "areEqual",
            "(Ljava/lang/Object;Ljava/lang/Object;)Z",
        );
        code.invokestatic(m, 2, 1);
    }

    /// The two operands of a `compare(a, b) <op> 0`, with the comparison to apply to them directly.
    ///
    /// `None` unless one side is the primitive three-way comparison and the other is the integer
    /// literal `0` — the only shape in which that result is meaningful. With the zero on the LEFT
    /// the comparison reverses (`0 < compare(a, b)` is `a > b`), so the operator is flipped rather
    /// than the operands, which keeps evaluation order.
    fn primitive_compare_operands(
        &self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
    ) -> Option<(u32, u32, IrBinOp)> {
        use IrBinOp::*;
        if !matches!(op, Lt | Le | Gt | Ge | Eq | Ne) {
            return None;
        }
        let zero = |e: u32| matches!(self.ir.expr(e), IrExpr::Const(IrConst::Int(0)));
        let (compared, direct) = if zero(rhs) {
            (lhs, op)
        } else if zero(lhs) {
            let flipped = match op {
                Lt => Gt,
                Le => Ge,
                Gt => Lt,
                Ge => Le,
                same => same,
            };
            (rhs, flipped)
        } else {
            return None;
        };
        let IrExpr::Call {
            callee:
                Callee::Intrinsic {
                    operation:
                        crate::ir::IrIntrinsic::PrimitiveCompare {
                            relational_operator: true,
                            ..
                        },
                    ..
                },
            dispatch_receiver: Some(receiver),
            args,
            ..
        } = self.ir.expr(compared)
        else {
            return None;
        };
        let [argument] = args.as_slice() else {
            return None;
        };
        Some((*receiver, *argument, direct))
    }

    /// Emit numeric comparison operands and the final branch for both value and branch consumers.
    /// Centralizing the zero-literal rule here is important: operand syntax must not select a different
    /// optimization merely because the surrounding node consumes a Boolean instead of control flow.
    fn emit_numeric_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        // `compare(a, b) <op> 0` IS `a <op> b`. Kotlin's `<`/`<=`/`>`/`>=` are the `compareTo`
        // operator, so the front end models them as a three-way comparison tested against zero —
        // but that result exists only to be tested, and kotlinc emits the direct comparison
        // (`if_icmpge`, or `lcmp` + `ifge`). Unwrapping HERE keeps the operand rules below (the
        // zero-literal form, the int category) as the single place comparisons are shaped.
        if let Some((left, right, direct)) = self.primitive_compare_operands(op, lhs, rhs) {
            self.emit_numeric_compare_branch(direct, left, right, target, jt, code);
            return;
        }
        let lt = self.value_ty(lhs);
        let rt = self.value_ty(rhs);
        if !lt.is_jvm_scalar() || !rt.is_jvm_scalar() {
            crate::trace_compiler!(
                "emit",
                "non-scalar numeric comparison lhs={lhs} {:?} ty={lt:?}, rhs={rhs} {:?} ty={rt:?}",
                self.ir.expr(lhs),
                self.ir.expr(rhs),
            );
        }
        // Numeric. A comparison against the integer literal `0` uses the single-operand compare-to-zero
        // branch (`ifeq`/`iflt`/… — kotlinc's form), saving the `iconst_0`. Only the int category; the
        // others compare 3-way through `lcmp`/`dcmp*`/`fcmp*`, which already tests the result vs 0.
        let int_cat = numeric_cmp_int_category(lt, rt);
        let zero = |e: u32| matches!(self.ir.expr(e), IrExpr::Const(IrConst::Int(0)));
        let cmp0_int = if int_cat && zero(rhs) {
            self.emit_value(lhs, code);
            Some(op)
        } else if int_cat && zero(lhs) && matches!(op, Eq | Ne) {
            // Equality is symmetric, so dropping the left zero preserves kotlinc's bytecode. Ordering
            // deliberately keeps both operands: kotlinc does not rewrite `0 < x` as `x > 0`, and doing
            // so only in branch position was the positional special case this shared path removes.
            self.emit_value(rhs, code);
            Some(op)
        } else {
            self.emit_operands(&[lhs, rhs], code);
            None
        };
        // An operand that carried its OWN source line leaves that line in effect. The comparison
        // belongs to the statement around it, so its instruction is marked back to the statement's
        // line — the same "return to the statement's line" the `putfield` of a field store gets.
        if self.statement_line.is_some()
            && [lhs, rhs]
                .iter()
                .any(|operand| self.ir.expr_source_lines.contains_key(operand))
        {
            if let Some(line) = self.statement_line {
                code.mark_line(line);
            }
        }
        if !int_cat {
            // `>`/`>=` use the `*l` float-compare variant, `<`/`<=` the `*g` — so NaN yields false
            // (kotlinc). Long has no NaN distinction but shares the three-way-result branch below.
            let nan_l = matches!(op, Gt | Ge);
            match lt {
                Ty::Long => code.lcmp(),
                Ty::Double => {
                    if nan_l {
                        code.dcmpl()
                    } else {
                        code.dcmpg()
                    }
                }
                Ty::Float => {
                    if nan_l {
                        code.fcmpl()
                    } else {
                        code.fcmpg()
                    }
                }
                _ => unreachable!("int_cat is false only for Long/Double/Float"),
            }
        }
        match cmp0_int {
            Some(o) => cmp0_branch(o, jt, target, code),
            None if !int_cat => cmp0_branch(op, jt, target, code),
            None => icmp_branch(op, jt, target, code),
        }
    }

    /// The loop label a branch body jumps to when it does nothing else.
    ///
    /// `None` unless the body IS a `break` or `continue` — one that also computed something would
    /// have to emit that first, and the jump could then not be fused into the condition. Leaving a
    /// `try` counts as computing something: the transfer runs every `finally` it leaves, so a fused
    /// jump would skip them.
    fn loop_jump_target(&self, body: u32) -> Option<Label> {
        let mut node = self.ir.expr(body);
        if let IrExpr::Block { stmts, value } = node {
            let [only] = stmts.as_slice() else {
                return None;
            };
            if value.is_some() {
                return None;
            }
            node = self.ir.expr(*only);
        }
        let (label, brk) = match node {
            IrExpr::Break { label } => (label, true),
            IrExpr::Continue { label } => (label, false),
            _ => return None,
        };
        let (cont, end, depth) = self.loop_transfer_target(label)?;
        if self.return_finalizers.len() > depth {
            return None;
        }
        Some(if brk { end } else { cont })
    }

    fn emit_when(
        &mut self,
        expression: u32,
        branches: &[(Option<u32>, u32)],
        discarded: bool,
        code: &mut CodeBuilder,
    ) {
        let end = code.new_label();
        // Each branch starts at the pre-condition height, not the linear counter left by the prior
        // body. Resetting at `next` prevents a phantom operand in later branch bodies.
        let entry_height = code.stack_height().max(0) as u16;
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        let exhaustive_result = self.ir.exhaustive_whens.get(&expression).copied();
        // A no-`else` or `Unit` `when` is a statement, so nothing reaches the stack at `end`.
        // Exhaustive results use their checked JVM erasure; other joins derive it from branch values.
        let result_ty = exhaustive_result
            .map(|result| ir_ty_to_jvm(&result))
            .unwrap_or_else(|| self.value_ty_of_when(branches));
        let is_stmt =
            (!has_else && exhaustive_result.is_none()) || result_ty == Ty::Unit || discarded;
        let enclosing_terminal_target = self.terminal_statement_target.take();
        let terminal_target = is_stmt.then_some(enclosing_terminal_target).flatten();
        if self.emit_safe_call_when(
            expression,
            branches,
            when::Emission::new(is_stmt, result_ty, entry_height, end, None),
            code,
        ) {
            return;
        }
        // A `when` comparing ONE Int local against constants is a JVM switch in kotlinc, not a chain
        // of comparisons. Everything above (the result type, the statement/value decision, the entry
        // height) applies unchanged; only the dispatch differs.
        if let Some(plan) = self.int_switch_plan(branches) {
            self.emit_int_switch(
                &plan,
                when::Emission::new(is_stmt, result_ty, entry_height, end, terminal_target),
                code,
            );
            return;
        }
        for (index, (cond, body)) in branches.iter().enumerate() {
            match cond {
                Some(c) => {
                    // A branch whose body is nothing but `break`/`continue` needs no branch AROUND
                    // it: the condition can jump straight to the loop label. Otherwise the shape is
                    // `if !cond -> next; goto target; next:`, a branch over a jump where kotlinc
                    // writes one inverted branch.
                    if is_stmt {
                        if let Some(jump) = self.loop_jump_target(*body) {
                            let unconditional = self
                                .in_condition(|this| this.emit_cond_branch(*c, jump, true, code));
                            code.set_stack(entry_height);
                            // A constant-true guard emitted an unconditional jump. Emitting any
                            // later arm after it would leave dead bytecode without a stack-map
                            // frame, which the verifier rejects. A constant-false guard emits no
                            // jump and must keep scanning the remaining arms.
                            if unconditional {
                                break;
                            }
                            continue;
                        }
                    }
                    // Skip to the next branch when this condition is false (fused comparison branch).
                    let next = code.new_label();
                    // A constant-false condition emits `goto next`; do not lay down its unreachable,
                    // unframed body. Suspend flattening produces this shape for some do-while loops.
                    if self.in_condition(|this| this.emit_cond_branch(*c, next, false, code)) {
                        self.bind(next, code);
                        code.set_stack(entry_height);
                        continue;
                    }
                    if is_stmt {
                        // Statement emission handles both physical-void operations and explicit
                        // `Unit.INSTANCE`; semantic `Ty::Unit` alone cannot distinguish them.
                        self.emit(*body, code);
                    } else {
                        self.emit_value(*body, code);
                        self.adapt_physical_operand_for(
                            *body,
                            self.value_ty(*body),
                            result_ty,
                            code,
                        );
                    }
                    let body_diverges = if is_stmt {
                        self.discarding_diverges(*body)
                    } else {
                        self.diverges(*body)
                    };
                    if !body_diverges {
                        // Fall through only across empty ELSE branches. An empty conditional branch
                        // still evaluates its condition, which the selected arm must skip.
                        let nothing_follows = branches[index + 1..]
                            .iter()
                            .all(|(condition, rest)| {
                                condition.is_none()
                                    && matches!(self.ir.expr(*rest), IrExpr::Block { stmts, value } if stmts.is_empty() && value.is_none())
                            })
                            && (has_else || exhaustive_result.is_none());
                        let falls_into_end = is_stmt && nothing_follows;
                        if !falls_into_end {
                            code.goto(end);
                        }
                    }
                    self.bind(next, code);
                    // `next` is reached only via the conditional jump above, where the stack is back at the
                    // pre-branch baseline — reset the linear counter (the just-emitted branch body left its
                    // value on the counter, but not on this control path).
                    code.set_stack(entry_height);
                }
                None => {
                    if is_stmt {
                        self.emit(*body, code);
                    } else {
                        self.emit_value(*body, code);
                        self.adapt_physical_operand_for(
                            *body,
                            self.value_ty(*body),
                            result_ty,
                            code,
                        );
                    }
                    // The else is last — it falls through to `end` (no goto needed).
                }
            }
        }
        if !has_else && exhaustive_result.is_some() {
            let exception = self.cw.class_ref("kotlin/NoWhenBranchMatchedException");
            code.new_obj(exception);
            code.dup();
            let constructor =
                self.cw
                    .methodref("kotlin/NoWhenBranchMatchedException", "<init>", "()V");
            code.invokespecial(constructor, 0, 0);
            code.athrow();
        }
        self.bind(end, code);
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
        match ty {
            t if is_jvm_int_category(t) => VerifType::Integer,
            Ty::Long => VerifType::Long,
            Ty::Double => VerifType::Double,
            Ty::Float => VerifType::Float,
            Ty::String => VerifType::ObjectName("java/lang/String".to_string()),
            // An array's verification type is an `Object` whose class name is its descriptor (`[I`).
            t if t.is_array() => VerifType::ObjectName(type_descriptor(ty)),
            Ty::Obj(n, _) => {
                VerifType::ObjectName(crate::jvm::names::classfile_internal_name(&n.render()))
            }
            Ty::Nullable(_) | Ty::PlatformNullable(_) => {
                VerifType::ObjectName(crate::jvm::names::instanceof_internal_name(ty))
            }
            Ty::Null => VerifType::Null,
            _ => VerifType::Top,
        }
    }

    fn value_ty(&self, e: u32) -> Ty {
        if let Some(result) = self.transformed_result(e) {
            return jvm_declared_ty(&result);
        }
        if matches!(self.ir.expr(e), IrExpr::When { .. }) {
            if let Some(result) = self.ir.exhaustive_whens.get(&e) {
                return ir_ty_to_jvm(result);
            }
        }
        match self.ir.expr(e) {
            IrExpr::StringConcat(_) => Ty::String,
            // A class literal `T::class` is a `java/lang/Class` constant — a reference, so `==`/`!=` on
            // two class literals routes to reference equality, not the primitive `if_icmpeq`.
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
                .slots
                .get(i)
                .map(|(_, t)| *t)
                .or_else(|| self.var_types.get(i).copied())
                .unwrap_or(Ty::Error),
            IrExpr::EnclosingInstance { outer, .. } => Ty::obj_name(*outer),
            IrExpr::GetField { class, index, .. } => {
                ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].ty)
            }
            IrExpr::PropertyRead { ty, .. } => {
                // A read keeps its LOGICAL type in the IR; a value-class property's accessor returns
                // the carrier, which the value-class pass records beside it. The stack holds that.
                if let Some(physical) = self.ir.physical_types.get(&e) {
                    return ir_ty_to_jvm(physical);
                }
                // A property read always yields a stored value. `Unit` therefore occupies the
                // `kotlin/Unit` reference slot; only a function's control-flow return uses `V`.
                ir_ty_to_jvm(&stored_value_ty(*ty))
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
                    match callee {
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
                // An arithmetic/bitwise op leaves a PRIMITIVE on the stack — the emitter unboxes each
                // operand first. So the result type is the UNBOXED primitive of the lhs, even when the lhs
                // value is a boxed wrapper (`it + 100` where `it` is an `Integer` from a `Map` get). Using
                // the boxed `value_ty(lhs)` here made a caller (e.g. the safe-call/elvis boxing coercion)
                // believe the result was already a reference and skip its `valueOf` → an `int`/`Integer`
                // stackmap mismatch once the masking spill was removed.
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
            IrExpr::StaticInstance { ty, .. } => Ty::obj(&self.ir.classes[*ty as usize].fq_name()),
            IrExpr::SingletonValue { classifier } => Ty::obj_name(*classifier),
            IrExpr::ExternalStaticInstance { ty, .. } => Ty::obj_name(*ty),
            IrExpr::ExternalStaticField { descriptor, .. } => {
                // The static field's JVM type, from its descriptor (an object `L…;` for an `object`'s
                // INSTANCE; primitives for the rare const-field case).
                match descriptor.as_str() {
                    "J" => Ty::Long,
                    "D" => Ty::Double,
                    "I" => Ty::Int,
                    "Z" => Ty::Boolean,
                    d => d
                        .strip_prefix('L')
                        .and_then(|s| s.strip_suffix(';'))
                        .map(Ty::obj)
                        .unwrap_or(Ty::obj("java/lang/Object")),
                }
            }
            IrExpr::RefNew { elem, .. } => Ty::obj(ref_class(elem).0),
            IrExpr::RefGet { elem, .. } => ir_ty_to_jvm(elem),
            IrExpr::RefSet { .. } => Ty::Unit,
            IrExpr::EnumValues { classifier } => Ty::array(Ty::obj_name(*classifier)),
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
            IrExpr::BottomValue { .. } => Ty::Nothing,
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
            IrExpr::Throw { .. } | IrExpr::Break { .. } | IrExpr::Continue { .. } => Ty::Nothing,
            IrExpr::Vararg { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::NewArray { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
            IrExpr::CurrentContinuation => Ty::obj("kotlin/coroutines/Continuation"),
            IrExpr::Try { result, .. } => ir_ty_to_jvm(result),
            _ => Ty::Error,
        }
    }
}

/// The `LambdaMetafactory.metafactory` bootstrap-method descriptor (the standard non-altmetafactory form).
const LMF_METAFACTORY_DESC: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;\
Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;\
Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";

/// A JVM method descriptor `(p1p2…)R` from parameter/return `Ty`s.
/// The erased SAM descriptor `(Ljava/lang/Object;…)Ljava/lang/Object;` for `FunctionN.invoke`.
fn sam_descriptor(arity: u8) -> String {
    let mut s = String::from("(");
    for _ in 0..arity {
        s.push_str("Ljava/lang/Object;");
    }
    s.push_str(")Ljava/lang/Object;");
    s
}

fn is_high_arity_function(arity: u8) -> bool {
    crate::jvm::names::uses_function_n(usize::from(arity))
}

fn jvm_function_interface(arity: u8) -> String {
    crate::jvm::names::function_interface_internal_name(usize::from(arity))
}

fn jvm_function_invoke_descriptor(arity: u8) -> String {
    if is_high_arity_function(arity) {
        "([Ljava/lang/Object;)Ljava/lang/Object;".to_string()
    } else {
        sam_descriptor(arity)
    }
}

/// The boxed (wrapper) descriptor for a `Ty` — primitives map to their wrapper, references unchanged.
fn boxed_descriptor(t: Ty) -> String {
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
    let ret = desc.rsplit(')').next().unwrap_or("V");
    ty_from_field_descriptor(ret)
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

/// Parse a single JVM field/type descriptor into a `Ty`.
///
/// Suspend operand materialization also consumes exact field descriptors already present in IR. Keep
/// that pass on this canonical parser instead of growing a second primitive/object/array branch table.
pub(crate) fn ty_from_field_descriptor(d: &str) -> Ty {
    match d.as_bytes().first() {
        Some(b'I') => Ty::Int,
        Some(b'J') => Ty::Long,
        Some(b'Z') => Ty::Boolean,
        Some(b'B') => Ty::Byte,
        Some(b'C') => Ty::Char,
        Some(b'S') => Ty::Short,
        Some(b'F') => Ty::Float,
        Some(b'D') => Ty::Double,
        Some(b'V') => Ty::Unit,
        Some(b'L') => Ty::obj(
            d.strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .unwrap_or(d),
        ),
        Some(b'[') => Ty::array(ty_from_field_descriptor(&d[1..])),
        _ => Ty::Error,
    }
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
/// If `t` is the boxed-reference form of a primitive (the element of a `Array<Int>` etc., carried as
/// `Obj("kotlin/Int")`), the underlying primitive `Ty`. Used to insert box/unbox at the boxed-array
/// element boundary (`a[i]` yields an unboxed `Int`; `a[i] = v` boxes the `Int`).
fn boxed_prim_of(t: Ty) -> Option<Ty> {
    t.unboxed_primitive()
}

/// Semantic scalar adapter for a JVM reference-array element. Unsigned values share a primitive
/// carrier with signed values but their array stores must call the Kotlin value-class box adapter.
fn reference_array_scalar_adapter(element: Ty) -> Option<Ty> {
    element
        .non_null()
        .is_unsigned()
        .then_some(element.non_null())
        .or_else(|| boxed_prim_of(element))
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
/// that fuse to `if_icmp*`/compare-to-zero, `false` for `Long`/`Double`/`Float`, which compare 3-way
/// through `lcmp`/`dcmp*`/`fcmp*` first.
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
    !matches!(lt, Ty::Long | Ty::Double | Ty::Float)
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
    match &t {
        Ty::Obj(n, _)
            if crate::jvm::jvm_class_map::type_name_maps_to_jvm_internal(*n, "java/lang/Void") =>
        {
            Ty::Nothing
        }
        _ => t,
    }
}

/// Physical element stored by a Kotlin reference `Array<T>`. Boxing is selected from the semantic
/// scalar before ordinary JVM erasure collapses unsigned values onto signed carriers; otherwise
/// `Array<UInt>` incorrectly becomes `Integer[]` instead of `kotlin.UInt[]`.
fn jvm_reference_array_element(semantic: Ty) -> Ty {
    let nullable = semantic.is_nullable();
    let semantic = semantic.non_null();
    let boxed = if semantic.is_unsigned() {
        // A bare unsigned `Ty` denotes its primitive carrier. Its nullable spelling is the existing
        // JVM-reference marker for the unsigned wrapper (`UInt?` -> `Lkotlin/UInt;`), and prevents
        // `Ty::array` from selecting the specialized `UIntArray`/`[I` representation here.
        Ty::nullable(semantic)
    } else {
        reference_array_element(ir_ty_to_jvm(&semantic))
    };
    if nullable {
        Ty::nullable(boxed)
    } else {
        boxed
    }
}

pub fn ir_ty_to_jvm(t: &Ty) -> Ty {
    // A nullable PRIMITIVE is a JVM reference — its boxed wrapper (`Int?` → `java/lang/Integer`, a
    // 1-slot reference), NOT the unboxed scalar. Map it before peeling `?`, so descriptors, slots and
    // stackmap frames all see the reference. A nullable REFERENCE keeps its descriptor (peel below).
    if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = t {
        if **inner == Ty::Nothing {
            // In a VALUE position this is the null-only bottom type, which also arises when generic
            // inference combines only `null` arguments. Keep its erased top representation here;
            // declaration descriptors use `jvm_declared_ty` and name inferred or explicit
            // `Nothing?` as Void.
            return Ty::obj("kotlin/Any");
        }
        if **inner == Ty::Unit {
            return Ty::obj("kotlin/Unit");
        }
        if inner.is_unsigned() {
            // Unlike a signed primitive wrapper, an unsigned box has the same classifier name as
            // its semantic scalar. Preserve the nullable type itself as the reference-slot marker;
            // returning bare `UInt` here would necessarily mean the unboxed `int` carrier.
            return *t;
        }
        if let Some(boxed) = inner.boxed_ref() {
            // `boxed_ref` already picks the right wrapper — `java/lang/Integer` for `Int?`, the inline-class
            // `kotlin/UInt` for `UInt?` — so do NOT re-map through `ir_ty_to_jvm` (which would erase the
            // unsigned wrapper to `Integer`).
            return boxed;
        }
    }
    // Nullability is otherwise erased at the JVM-type level (a nullable reference keeps its descriptor),
    // so peel the `?` first.
    match t.non_null() {
        Ty::Unit => Ty::Unit,
        Ty::Nothing => Ty::Nothing,
        // `null` has its own JVM verification type. Preserve it through slot lowering so loop and
        // resume frames describe an always-null local as `Null`, not as the unusable `Top` type.
        Ty::Null => Ty::Null,
        // Bare scalar/`String` variants are already JVM types — pass through. (Checked/common-IR types
        // can arrive either as these variants or as their `Obj("kotlin/…")` spelling; both must map here.)
        Ty::Int => Ty::Int,
        Ty::Long => Ty::Long,
        Ty::Short => Ty::Short,
        Ty::Byte => Ty::Byte,
        Ty::Boolean => Ty::Boolean,
        Ty::Char => Ty::Char,
        Ty::Double => Ty::Double,
        Ty::Float => Ty::Float,
        Ty::String => Ty::String,
        // Unsigned scalars are inline classes over the signed primitive; unboxed they ARE that primitive
        // (`UInt` = `int`, `ULong` = `long`) — same JVM slots and `istore`/`iload`/arithmetic. Unsigned
        // semantics live in the intrinsic calls (`Integer.compareUnsigned`, …) common lowering inserted.
        Ty::UByte => Ty::Byte,
        Ty::UShort => Ty::Short,
        Ty::UInt => Ty::Int,
        Ty::ULong => Ty::Long,
        Ty::Obj(fq_name, type_args) => {
            // Arrays are regular class types the JVM backend lowers to JVM array types here. Every
            // primitive specialized array, signed and unsigned alike, goes through the one operation
            // that identifies them and the one that decides an element's width — see
            // `jvm::array_representation`.
            if let Some(carrier) = crate::jvm::array_representation::prim_array_carrier(fq_name) {
                return carrier;
            }
            match () {
                _ if fq_name.matches("kotlin/Int") => Ty::Int,
                _ if fq_name.matches("kotlin/Long") => Ty::Long,
                _ if fq_name.matches("kotlin/Short") => Ty::Short,
                _ if fq_name.matches("kotlin/Byte") => Ty::Byte,
                _ if fq_name.matches("kotlin/Boolean") => Ty::Boolean,
                _ if fq_name.matches("kotlin/Char") => Ty::Char,
                _ if fq_name.matches("kotlin/Double") => Ty::Double,
                _ if fq_name.matches("kotlin/Float") => Ty::Float,
                _ if fq_name.matches("kotlin/String") => Ty::String,
                // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
                // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
                _ if fq_name.matches("kotlin/Array") => Ty::array(
                    type_args
                        .first()
                        .map(|e| {
                            // A projection is valid here as the ARRAY classifier's type argument, even
                            // though it is never a value type of its own. Erase it at this boundary:
                            // `out X` has the readable element `X`; `in X` can only be read as `Any`.
                            let semantic = match e.non_null() {
                                Ty::OutProjection(inner) | Ty::StarProjection(inner) => *inner,
                                Ty::InProjection(_) => Ty::obj("kotlin/Any"),
                                _ => *e,
                            };
                            let boxed = jvm_reference_array_element(semantic);
                            // Keep a NULLABLE element's `?`: `Array<Int?>` = `Integer[]` whose `get` yields the
                            // BOXED element (it can be `null`), UNLIKE `Array<Int>` whose `get` unboxes.
                            // `boxed_prim_of` returns `None` for a `Nullable(..)`, so the emitter's `Array.get`
                            // keeps it boxed and `.set` skips the extra box — matching the value the front end
                            // supplies (boxed for a nullable element, unboxed for a non-null one).
                            if e.is_nullable() {
                                Ty::nullable(boxed)
                            } else {
                                boxed
                            }
                        })
                        .unwrap_or(Ty::obj("java/lang/Object")),
                ),
                _ => Ty::obj(&crate::jvm::names::classfile_internal_name(
                    &fq_name.render(),
                )),
            }
        }
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`. A `suspend`
        // function type carries a trailing `Continuation` parameter, so its arity is one greater.
        Ty::Fun(s) => Ty::obj(&crate::jvm::names::function_interface_internal_name(
            s.params.len() + usize::from(s.suspend),
        )),
        // JVM erasure of a type parameter: collapse `T` to its declared upper bound (which itself
        // erases to `java/lang/Object` for an `Any` bound). This is the ONE place `T` becomes a
        // concrete JVM type.
        // A nullable occurrence keeps its `?` on the bound, as kotlinc's type mapper does: `T?` over
        // `T : Int` is `Integer`, never `int`.
        Ty::TyParam(_, bound) if t.is_nullable() => ir_ty_to_jvm(&Ty::nullable(*bound)),
        Ty::TyParam(_, bound) => ir_ty_to_jvm(bound),
        _ => Ty::Error,
    }
}

fn super_ctor_jvm_tys(ir: &IrFile, c: &IrClass, superclass: &str) -> (Vec<Ty>, bool) {
    let mut params = jvm_tys(&c.super_ctor_params);
    let uses_accessor = (c.super_ctor.primary && ir.has_value_param_ctor(superclass))
        || constructor_accessors::reached_through_accessor(
            c.super_ctor,
            Some(c.fq_name_id()),
            c.superclass,
        );
    if uses_accessor {
        params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    }
    (params, uses_accessor)
}

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

/// The `String.valueOf` overload descriptor for a single interpolated value's type (`"$x"`).
fn valueof_desc(t: Ty) -> &'static str {
    match t {
        Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
        Ty::Long => "(J)Ljava/lang/String;",
        Ty::Float => "(F)Ljava/lang/String;",
        Ty::Double => "(D)Ljava/lang/String;",
        Ty::Boolean => "(Z)Ljava/lang/String;",
        Ty::Char => "(C)Ljava/lang/String;",
        _ => "(Ljava/lang/Object;)Ljava/lang/String;",
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

fn discard(t: Ty, code: &mut CodeBuilder) {
    match slot_words(t) {
        2 => code.pop2(),
        1 => code.pop(),
        _ => {}
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
    use crate::ir::{IrExpr, IrFile, IrFunction};
    use crate::jvm::classreader::MethodCode;
    use crate::jvm::inline::MethodBodies;
    use crate::types::Ty;

    pub(super) struct NoBodies;
    impl MethodBodies for NoBodies {
        fn body(&self, _o: &str, _n: &str, _d: &str) -> Option<MethodCode> {
            None
        }
    }

    struct NoClassifiers;

    impl BackendClassifierSource for NoClassifiers {
        fn classifier(
            &self,
            _classifier: crate::types::TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            None
        }
    }

    pub(super) fn emit_for_test(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        emit_for_test_with_machines(
            ir,
            facade,
            run,
            &crate::jvm::suspend::EmitTimeMachines::default(),
        )
    }

    /// [`emit_for_test`] with the emit-time coroutine machines a suspend pass would have recorded.
    pub(super) fn emit_for_test_with_machines(
        ir: &IrFile,
        facade: &str,
        run: &EmitRun,
        emit_time_machines: &crate::jvm::suspend::EmitTimeMachines,
    ) -> Option<Vec<(String, Vec<u8>)>> {
        let continuations = crate::jvm::suspend::ContinuationMetadataMap::default();
        let property_realizations =
            crate::jvm::property_realizations::PropertyRealizations::default();
        let property_reference_realizations =
            crate::jvm::property_references::PropertyReferenceRealizations::default();
        let default_call_operands =
            crate::jvm::default_call_operands::DefaultCallOperands::default();
        let bridge_returns =
            crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations::default();
        let unit_result_tail_forwards = crate::jvm::suspend::UnitResultTailForwards::default();
        emit_all_with_checked_classifiers(
            ir,
            facade,
            &NoBodies,
            CheckedEmitFacts {
                metadata: EmitMetadata {
                    facade: None,
                    continuations: &continuations,
                    bridge_returns: &bridge_returns,
                    emit_time_machines,
                    unit_result_tail_forwards: &unit_result_tail_forwards,
                },
                signature_symbols: &NoClassifiers,
                property_realizations: &property_realizations,
                property_reference_realizations: &property_reference_realizations,
                default_call_operands: &default_call_operands,
            },
            &EmitOptions::default(),
            run,
        )
    }

    #[test]
    fn member_metadata_flags_keep_inline_operator_and_infix_capabilities() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "convention".into(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: Some(crate::types::type_name("demo/Owner")),
            param_checks: Vec::new(),
        });
        ir.inline_fns.insert(function);
        ir.operator_fns.insert(function);
        ir.infix_fns.insert(function);

        let flags = function_flags(&ir, function, &ir.functions[function as usize]);
        assert_ne!(flags & (1 << 8), 0);
        assert_ne!(flags & (1 << 9), 0);
        assert_ne!(flags & (1 << 10), 0);
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

        let (resolved_owner, method) =
            class_enclosure(&ir, &anonymous, "IgnoredFacade").expect("bound enclosure");
        assert_eq!(resolved_owner, "demo/Owner");
        assert_eq!(
            method,
            Some(("build".to_string(), "(Ljava/lang/String;)V".to_string()))
        );
    }

    #[test]
    fn nested_metadata_uses_declaration_identity_not_generated_name_shape() {
        let mut ir = IrFile::default();
        let mut outer = crate::plugins::synthetic_class("demo/Outer");
        outer.is_source_declared = true;
        let outer_id = ir.add_class(outer);
        ir.record_class_source_order(outer_id, 0);

        // A digit is valid in a source identifier; the former generated-name heuristic dropped it.
        let mut declared = crate::plugins::synthetic_class("demo/Outer$Node2");
        declared.is_source_declared = true;
        let declared_id = ir.add_class(declared);
        ir.record_class_source_order(declared_id, 1);

        // Conversely, looking nested is not sufficient: backend-generated implementation classes
        // are not declarations in Kotlin metadata.
        ir.add_class(crate::plugins::synthetic_class("demo/Outer$Impl3"));

        let metadata =
            build_class_metadata(&ir, &ir.classes[outer_id as usize], &EmitOptions::default())
                .expect("plain source class metadata");
        assert!(metadata.d2.iter().any(|entry| entry == "Node2"));
        assert!(!metadata.d2.iter().any(|entry| entry == "Impl3"));
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

    #[test]
    fn array_actual_realization_requires_the_selected_full_declaration() {
        let array = Ty::array(Ty::Byte);
        let owner = crate::types::type_name("kotlin/ByteArray");
        assert_eq!(
            jvm_array_actual_realization(owner, "get", array, &[Ty::Int], Ty::Byte),
            Some(JvmArrayActualRealization::Get)
        );
        assert_eq!(
            jvm_array_actual_realization(owner, "set", array, &[Ty::Int, Ty::Byte], Ty::Unit,),
            Some(JvmArrayActualRealization::Set)
        );
        assert_eq!(
            jvm_array_actual_realization(owner, "size", array, &[], Ty::Int),
            Some(JvmArrayActualRealization::Size)
        );
        let boxed_int_array = Ty::obj_args("kotlin/Array", &[Ty::obj("java/lang/Integer")]);
        assert_eq!(
            jvm_array_actual_realization(
                crate::types::type_name("kotlin/Array"),
                "get",
                boxed_int_array,
                &[Ty::Int],
                Ty::Int,
            ),
            Some(JvmArrayActualRealization::Get)
        );

        assert_eq!(
            jvm_array_actual_realization(owner, "get", array, &[Ty::Long], Ty::Byte),
            None
        );
        assert_eq!(
            jvm_array_actual_realization(owner, "set", array, &[Ty::Int, Ty::Int], Ty::Unit),
            None
        );
        assert_eq!(
            jvm_array_actual_realization(owner, "size", array, &[], Ty::Long),
            None
        );
        assert_eq!(
            jvm_array_actual_realization(
                crate::types::type_name("sample/FakeArray"),
                "get",
                array,
                &[Ty::Int],
                Ty::Byte,
            ),
            None
        );
    }

    // A `GetValue` of a value slot that was never allocated is malformed IR. Letting emission
    // silently skip it would preserve a second, fail-soft path around the authoritative final-body
    // analysis; the backend must reject the broken phase contract at the method boundary.
    #[test]
    #[should_panic(expected = "cannot compute JVM frames for box()V: Unsteppable(0)")]
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
            vec![crate::jvm::suspend::cps::SplicedSuspension { call: suspension }],
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
