//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{
    Callee, IrBinOp, IrClass, IrConst, IrCtorArg, IrExpr, IrField, IrFile, IrFunction, IrTypeOp,
};
use crate::jvm::classfile::{
    ClassWriter, CodeBuilder, InnerClassResolver, Label, VerifType, MAJOR_JAVA8,
};
use crate::jvm::classreader::{MethodCode, C};
use crate::jvm::inline::MethodBodies;
use crate::jvm::names::{
    method_descriptor, property_getter_name, property_setter_name, reference_array_element,
    type_descriptor,
};
use crate::kt_string::{KtString, KtStringBuf};
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{stored_value_ty, Ty, TypeName, TypeVariance};

struct InlineStaticTarget<'a> {
    owner: &'a str,
    name: &'a str,
    descriptor: &'a str,
    splice_desc: &'a str,
}

/// kotlinc realizes a NAMED `object` declaration's property backing fields as STATIC fields on the
/// object class: accessors read/write `getstatic`/`putstatic`, initializers run in `<clinit>` after
/// the `INSTANCE` store, and `<init>` is a bare `super()` call. Companions hoist their fields to the
/// OUTER class instead (not modeled yet), and local/anonymous objects keep instance fields.
fn object_static_storage(c: &IrClass) -> bool {
    c.is_object && !c.is_companion && !c.is_local_class && c.enum_entry_of.is_none()
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
    ir.classes
        .iter()
        .any(|candidate| candidate.is_interface && candidate.companion_class == Some(c.fq_name))
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

/// Mutable per-emit-run accumulators, owned by the caller and shared (by `&`, via interior mutability)
/// down the emit callgraph — formerly three thread-locals. The caller reads `inline_bail`/`emit_bail`
/// after `emit_all_with_opts` returns `None` to distinguish an inline-splice failure (a backend bug to
/// fix) from an unsupported construct (skip the file).
#[derive(Default)]
pub struct EmitRun {
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
    /// Lambda classes to synthesize for `-Xlambdas=class`, recorded as their `invokedynamic`
    /// replacement is emitted and drained by the class driver. They cannot be written inline: the
    /// emitter is mid-way through the ENCLOSING class's writer when it reaches a lambda.
    lambda_classes: std::cell::RefCell<Vec<LambdaClassPlan>>,
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
    /// Whether the body lives on an INTERFACE: a static call to one needs an `InterfaceMethodref`
    /// constant, not a `Methodref` (`IncompatibleClassChangeError` otherwise).
    owner_is_interface: bool,
}

impl EmitRun {
    /// The inline-splice failure reason recorded this run, if any (read by the caller after `None`).
    pub fn inline_bail(&self) -> Option<String> {
        self.inline_bail.borrow().clone()
    }
    /// Record a stable public failure category. Concrete owners, callable names, and descriptors are
    /// deliberately excluded here because this value reaches CLI diagnostics and survey buckets;
    /// those identities are emitted through opt-in compiler traces at the failure site instead.
    fn set_inline_bail(&self, reason: &'static str) {
        *self.inline_bail.borrow_mut() = Some(reason.to_string());
    }

    /// The malformed-IR / missing-emission-context reason recorded this run, if any.
    pub fn emit_error(&self) -> Option<String> {
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
pub struct EmitEnv<'a> {
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
    continuation_metadata: &'a crate::jvm::suspend::ContinuationMetadataMap,
    /// Semantic classifier declarations used only while translating Kotlin generic types into JVM
    /// `Signature` attributes. Declaration-site variance is a Kotlin fact; spelling it as JVM
    /// use-site wildcards is owned entirely by this emitter.
    signature_symbols: &'a dyn SymbolSource,
    /// `-jvm-default`. Read at CALL SITES: under `Disable` an interface's `$default` synthetic lives
    /// on the `$DefaultImpls` holder, not on the interface, so a call that omits a defaulted argument
    /// must target the holder or it links to a method that does not exist.
    jvm_default: JvmDefaultMode,
    /// The lambda strategy in force (`-Xlambdas`). Carried here rather than read from `EmitOptions`
    /// at the lambda site because the emitter has the env, not the options.
    lambdas: LambdaMode,
}

/// A built `@kotlin.Metadata` annotation for a file facade: the `k`/`mv`/`xi` ints and the `d1` (the
/// encoded protobuf, one byte per `char`) / `d2` (string table) arrays. Attached to the facade class so
/// another Kotlin/krusty compilation can resolve its top-level declarations — in particular reading the
/// `IS_SUSPEND` flag + logical signature of a `suspend fun`.
#[derive(Clone)]
pub struct KotlinMetadata {
    pub k: i32,
    pub mv: Vec<i32>,
    pub xi: i32,
    pub d1: Vec<String>,
    pub d2: Vec<String>,
}

fn is_continuation_class(class: &crate::ir::IrClass) -> bool {
    class.superclass_matches("kotlin/coroutines/jvm/internal/ContinuationImpl")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedContinuationImpl")
}

fn is_coroutine_state_machine(class: &crate::ir::IrClass) -> bool {
    is_continuation_class(class)
        || class.superclass_matches("kotlin/coroutines/jvm/internal/SuspendLambda")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedSuspendLambda")
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
/// What krusty EMITS today is narrower than that table, which describes kotlinc:
///   * `NoCompatibility` matches — default methods on the interface, no holder, no class forwarders.
///   * `Enable` emits the holder ONLY for a member with default parameter values (its `$default`
///     stub). kotlinc also puts a forwarder there for every other member with a body, emits the
///     `access$…$jd` bridges, and gives implementing classes forwarder overrides. That gap predates
///     `-jvm-default` support and is why `Enable` is not yet claimed as byte-parity.
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
        for check in &mut function.param_checks {
            *check = None;
        }
    }
    for class in &mut ir.classes {
        for parameter in &mut class.ctor_args {
            parameter.check = None;
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
    /// `-Xlambdas` / `-Xsam-conversions`: which lambda strategy the emitter uses. Per-INVOCATION,
    /// applied by the backend with [`EmitOptions::with_lambdas`] like [`EmitOptions::with_jvm_default`].
    pub lambdas: LambdaMode,
    pub inner_class_resolver: Option<InnerClassResolver>,
}

impl EmitOptions {
    /// Select the `-jvm-default` mode, keeping every other field as configured.
    pub fn with_jvm_default(mut self, mode: JvmDefaultMode) -> Self {
        self.jvm_default = mode;
        self
    }

    /// Select the lambda strategy (`-Xlambdas`), keeping every other field as configured.
    pub fn with_lambdas(mut self, mode: LambdaMode) -> Self {
        self.lambdas = mode;
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
            lambdas: LambdaMode::Indy,
            inner_class_resolver: None,
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
/// (FINAL=0, OPEN=1, ABSTRACT=2) | bits6-7 memberKind (DECLARATION=0, SYNTHESIZED=3) | bit8 isOperator.
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
    let operator: u64 = if ir.operator_fns.contains(&fid) {
        1 << 8
    } else {
        0
    };
    (visibility << 1) | (modality << 4) | operator
}

/// The primary constructor's parameter descriptors. Only the LEADING `ctor_param_count` fields are
/// constructor parameters — a BODY property (`val y: Int = 2`) is a field but not a ctor argument.
fn ctor_field_descs(c: &IrClass) -> String {
    c.fields
        .iter()
        .take(c.ctor_param_count as usize)
        .map(|f| crate::jvm::names::type_descriptor(f.ty))
        .collect()
}

/// `String` literals a class `init_body` assigns to a field, by field index. kotlinc interns each as
/// an `ldc` constant just before that property's store.
/// Field indices a class `init_body` actually stores into. A body property initialized to `null` is
/// not among them — the JVM's zero-initialization already does the job, so kotlinc emits no store.
fn init_body_stored_fields(ir: &IrFile, c: &IrClass) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    let Some(body) = c.init_body else { return out };
    let IrExpr::Block { stmts, .. } = ir.expr(body) else {
        return out;
    };
    for &s in stmts {
        if let IrExpr::SetField { index, .. } = ir.expr(s) {
            out.insert(*index);
        }
    }
    out
}

fn init_body_string_consts(ir: &IrFile, c: &IrClass) -> std::collections::HashMap<u32, KtString> {
    let mut out = std::collections::HashMap::new();
    let Some(body) = c.init_body else { return out };
    let IrExpr::Block { stmts, .. } = ir.expr(body) else {
        return out;
    };
    for &s in stmts {
        if let IrExpr::SetField { index, value, .. } = ir.expr(s) {
            if let IrExpr::Const(crate::ir::IrConst::String(t)) = ir.expr(init_operand(ir, *value))
            {
                out.insert(*index, t.clone());
            }
        }
    }
    out
}

/// The value an initializer STORES, seeing through a value-class construction: the value-class pass
/// rewrites `val k: K = K("OK")` to `K.constructor-impl("OK")`, whose stored value is still the constant
/// the `ldc` pushes. Anything else is its own operand.
fn init_operand(ir: &IrFile, value: crate::ir::ExprId) -> crate::ir::ExprId {
    match ir.expr(value) {
        IrExpr::Call {
            callee: crate::ir::Callee::Static { name, .. },
            args,
            ..
        } if name == "constructor-impl" && args.len() == 1 => args[0],
        _ => value,
    }
}

/// Per field index, the `(value class, `constructor-impl` descriptor)` its initializer constructs FROM A
/// CONSTANT. The constant-pool seeder needs it to intern the factory where kotlinc does: after the
/// constant the initializer pushes and before the field the store writes.
///
/// Restricted to a constant operand on purpose. kotlinc interns in EVALUATION order, so an initializer
/// that computes its argument (`K(compute())`) interns that call first and the factory after it —
/// seeding the factory at the field's position would put it ahead of a call the seeder does not model.
/// Leaving those to natural emission order keeps them where they were.
fn init_body_value_class_ctors(
    ir: &IrFile,
    c: &IrClass,
) -> std::collections::HashMap<u32, (String, String)> {
    let mut out = std::collections::HashMap::new();
    let Some(body) = c.init_body else { return out };
    let IrExpr::Block { stmts, .. } = ir.expr(body) else {
        return out;
    };
    for &s in stmts {
        if let IrExpr::SetField { index, value, .. } = ir.expr(s) {
            if let IrExpr::Call {
                callee:
                    crate::ir::Callee::Static {
                        owner,
                        name,
                        descriptor,
                        ..
                    },
                args,
                ..
            } = ir.expr(*value)
            {
                let from_constant = args
                    .first()
                    .is_some_and(|&a| matches!(ir.expr(a), IrExpr::Const(_)));
                if name == "constructor-impl" && from_constant {
                    out.insert(*index, (owner.render(), descriptor.clone()));
                }
            }
        }
    }
    out
}

/// Is this property declared as a bare TYPE PARAMETER (`class C<T>(val a: T)`)? It erases to
/// `Ljava/lang/Object;`, but `T`'s implicit bound is `Any?`, so kotlinc annotates it neither
/// `@NotNull` nor `@Nullable` and emits no `checkNotNullParameter` guard. `field_signatures` already
/// tracks these — it is what drives their `Signature` attribute.
fn is_type_parameter_field(ir: &IrFile, fq_name: &str, field: &str) -> bool {
    ir.field_signatures(fq_name)
        .is_some_and(|fs| fs.iter().any(|(name, _)| name == field))
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
        COPY_FN_FLAGS, EQUALS_FN_FLAGS, FN_IS_SUSPEND, HASHCODE_TOSTRING_FN_FLAGS,
        OBJECT_CTOR_FLAGS, SEALED_CTOR_FLAGS,
    };
    if is_coroutine_state_machine(c) {
        return Some(KotlinMetadata {
            k: 3,
            mv: vec![2, 4, 0],
            xi: 48,
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
    let data_method_names: std::collections::HashSet<String> = if c.is_data {
        let mut s: std::collections::HashSet<String> = (1..=data_component_fields.len())
            .map(|i| format!("component{i}"))
            .collect();
        s.extend(["equals", "hashCode", "toString"].map(String::from));
        if synthesizes_copy {
            s.insert("copy".to_string());
        }
        s
    } else {
        std::collections::HashSet::new()
    };
    // The only methods allowed in this bounded shape are the properties' own accessors (`getX`/`setX`)
    // plus a data class's synthesized set; any other real method is a shape not computed yet.
    // Accessor spellings are matched by name AND shape below, so the getter and setter names stay
    // in separate sets: a declared `operator fun getValue(thisRef, prop)` shares the JVM getter
    // name of a property called `value` but takes parameters no getter has — swallowing it by name
    // alone dropped its Function record from `@Metadata`, and a consumer could then not resolve
    // the delegate operator.
    let mut getter_names = std::collections::HashSet::new();
    let mut setter_names = std::collections::HashSet::new();
    for name in c
        .fields
        .iter()
        .map(|f| f.name.as_str())
        // A HOISTED companion property has no companion field, but its delegating accessors are
        // ordinary IR methods — they realize the Property record, never a Function one.
        .chain(
            c.properties
                .iter()
                .enumerate()
                .filter(|(property, p)| {
                    p.backing_field.is_none() && hoisted_static_for(ir, c, *property).is_some()
                })
                .map(|(_, p)| p.name.as_str()),
        )
    {
        let (getter, setter) = accessor_jvm_names(c, name);
        getter_names.insert(getter);
        setter_names.insert(setter);
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
    // Any member that is NOT an accessor and NOT part of a data/value class's synthesized set is a REAL
    // declared function — emit it (with derived flags) rather than declining the whole class.
    let mut declared_fids: Vec<u32> = c
        .methods
        .iter()
        .copied()
        .filter(|&fid| {
            if ir.lambda_own_params_from.contains_key(&fid) || ir.synthetic_methods.contains(&fid) {
                return false;
            }
            if ext_prop_accessor_fids.contains(&fid) {
                return false;
            }
            let function = &ir.functions[fid as usize];
            let n = &function.name;
            let accessor_shaped = (getter_names.contains(n) && function.params.is_empty())
                || (setter_names.contains(n) && function.params.len() == 1);
            !accessor_shaped && !data_method_names.contains(n) && !value_method_names.contains(n)
        })
        .collect();
    declared_fids.sort_by_key(|fid| ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX));
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
    // Four shapes still decline, and NONE of them for the reason removed above: each is a WRITE-side
    // gap — krusty's own output differs from kotlinc's — so the read half fixed here cannot reach them.
    // Each was invisible while the record was withheld, and each is proven by a differential comparison
    // against real kotlinc for the same source.
    //
    // 1. A VALUE-CLASS-typed CONSTRUCTOR PARAMETER. `class Holder(val id: ItemId)` gets kotlinc's
    //    PRIVATE-primary + synthetic `DefaultConstructorMarker` ABI, which the builder cannot describe:
    //    krusty named the PRIVATE `<init>(Ljava/lang/String;)V` rather than kotlinc's
    //    `(Ljava/lang/String;Lkotlin/jvm/internal/DefaultConstructorMarker;)V`, typed `id` as `String`
    //    instead of `LItemId;`, and dropped the getter's mangled `getId-YyT5sjE`. Real kotlinc reading
    //    that record rejects `Holder(ItemId("OK"))` as a type mismatch, and a caller that satisfied it
    //    would `invokespecial` the private constructor. `ir.has_value_param_ctor` (recorded before
    //    erasure loses the parameter's identity) is the signal; `vc_declared_sigs` cannot be, as it
    //    holds non-synthesized FUNCTIONS only.
    //
    // 2. A VALUE class with a DECLARED MEMBER of its own. kotlinc realizes
    //    `value class S(val v: String) { fun k(): String }` as the STATIC
    //    `k-impl(Ljava/lang/String;)Ljava/lang/String;` over the unboxed carrier; krusty emits an
    //    INSTANCE `k()` on the box. Reading krusty's record then puts the carrier on the stack under an
    //    `invokevirtual S.k()` — "Type 'java/lang/String' is not assignable to 'S'", a VerifyError.
    //    (krusty's caller is right: against a KOTLINC-built `S` the same source runs.)
    //
    //    A COMPUTED property counts, and `declared_fids` cannot see it: its accessor is synthesized
    //    straight from `IrProperty` and has no `IrFunction` entry, while `accessor_names` is derived
    //    from BACKING fields, which a computed property has none of. `A<T> { val publicValue: String
    //    get() = … }` is that shape — kotlinc emits the static `getPublicValue-impl(Object)`, krusty an
    //    instance `getPublicValue()`. The SOLE underlying property is not a declared member in this
    //    sense: kotlinc gives it an instance `getV()` too, so it stays admissible.
    // 3. A member whose value-class position erases to `Object` — i.e. the value class's underlying is
    //    itself erased-top (`value class A<T>(val value: T)`, `kotlin/Result`). The RETURN model
    //    described above rests on the physical type identifying the carrier, and at `Object` it does
    //    not: a carrier and a BOX sitting in a generic slot are spelled identically, which is the same
    //    ambiguity `call_declared_ret` exists to resolve. That declared-return fact is now threaded
    //    through ordinary member, static and operator-invoke calls, but parameter positions still lack
    //    an equivalent selected-declaration carrier fact: an `Object`-underlying value-class argument
    //    can still arrive boxed where the callee expects its carrier. Admission therefore remains a
    //    conservative whole-member decline whenever ANY declared value-class position erases to
    //    `Object`, until both directions are verified on every call route. Read this off the erased
    //    signature rather than a value-class table, so it holds for a classpath value class (`Result`)
    //    exactly as for a same-file one.
    //
    //    A `suspend` member's return is EXEMPT, because the CPS rewrite makes every suspend method
    //    return `Object` regardless of what it declares (the real return rides the `Continuation`'s
    //    type argument). Reading that `Object` as value-class erasure would decline shapes that are
    //    perfectly describable — `interface I { suspend fun f(a: K): String }` is byte-identical to
    //    kotlinc. Value-class PARAMETERS are still checked; only the return is exempt.
    //
    // 4. The exemption itself has an exception, and it is a real miscompile rather than a lost
    //    opportunity: when the value-class pass BOXES the value-class return at the CPS `areturn`
    //    (`ir.suspend_boxed_value_class_returns`), krusty's bytecode and kotlinc's disagree. kotlinc
    //    boxes only for a PRIMITIVE underlying; over a reference, nullable, or generic underlying it
    //    returns the raw carrier, while krusty boxes unconditionally. Since the RECORD krusty writes
    //    is byte-identical to kotlinc's, describing such a member advertises an ABI the class file
    //    does not implement: a consumer compiled against it does `C().gk().v` and gets
    //    "class K cannot be cast to class java.lang.String". Against a KOTLINC-built `C` the same
    //    source runs, so this is krusty's boxing, not its reader. That table is keyed by `FunId` and
    //    holds exactly the members whose CPS return krusty boxes — an ABSTRACT member has no return
    //    expression and never appears, which is why the interface shapes above stay admissible.
    let erases_value_class_to_object = |fid: &u32| {
        let Some((_, declared_params, declared_ret)) = ir.vc_declared_sigs.get(fid) else {
            return false;
        };
        let f = &ir.functions[*fid as usize];
        // The CPS marker itself: a suspend method's erased signature ends in the `Continuation`.
        let is_cps = f
            .params
            .last()
            .and_then(|p| p.non_null().obj_internal())
            .is_some_and(|n| n.matches("kotlin/coroutines/Continuation"));
        let cps_boxes_value_class_return = ir.suspend_boxed_value_class_returns.contains_key(fid);
        let param_erased = declared_params
            .iter()
            .zip(f.params.iter())
            .any(|(declared, erased)| declared != erased && erased.non_null().is_erased_top());
        // Per-kind rule. CLASS members: only a CPS return krusty BOXES diverges — a non-suspend
        // member returns the raw carrier byte-identically to kotlinc (verified:
        // `constructor-impl; areturn`, same mangle hash), so it is described. INTERFACE members
        // keep the ORIGINAL decline for the non-CPS case: a consumer resolving a mangled operator
        // through an interface record misencodes the call owner (corpus kt50974:
        // IncompatibleClassChangeError "found interface, but class was expected") — while a CPS
        // abstract member (no body, never in the boxed table) stays admitted, as it always was.
        let ret_diverges = if c.is_interface {
            !is_cps || cps_boxes_value_class_return
        } else {
            is_cps && cps_boxes_value_class_return
        };
        let ret_erased = ret_diverges && *declared_ret != f.ret && f.ret.non_null().is_erased_top();
        param_erased || ret_erased
    };
    let has_object_erased_value_class_member =
        declared_fids.iter().any(erases_value_class_to_object);
    if has_object_erased_value_class_member {
        crate::trace_compiler!(
            "emit",
            "class {} metadata declined: object-erased value-class member / value-param ctor",
            c.fq_name()
        );
        return None;
    }
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
    if declared_fids.iter().any(|fid| {
        ir.vc_declared_sigs
            .get(fid)
            .is_some_and(|(_, params, ret)| {
                params
                    .iter()
                    .chain(std::iter::once(ret))
                    .any(mentions_undescribed_value_class)
            })
    }) || c
        .properties
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
                        method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)),
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
                                method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)),
                            )
                        })
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        (!visibility.is_private())
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
                        method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)),
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
                                method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)),
                            )
                        })
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        (!visibility.is_private() && property.is_var)
                            .then(|| (default_setter, format!("({})V", desc(field.ty))))
                    })
                });
            (
                property.source_order,
                PropMeta {
                    name: property.name.clone(),
                    ty: property.ty,
                    is_var: property.is_var,
                    visibility,
                    // A HOISTED companion property still records a (derived) backing field — the field
                    // exists, on the outer class — and a literal-initialized `val` keeps kotlinc's
                    // HAS_CONSTANT flag exactly like an instance-field one.
                    has_constant: backing.is_some_and(|(index, field)| {
                        field.is_final()
                            && index >= c.ctor_param_count
                            && const_fields.contains(&index)
                    }) || hoisted_static_for(ir, c, property_index)
                        .is_some_and(|s| !s.is_var && const_value_idx_peek(ir, s.init)),
                    is_const: false,
                    is_abstract: c.is_interface && property.getter.is_none(),
                    has_backing_field: !c.is_annotation
                        && (backing.is_some()
                            || hoisted_static_for(ir, c, property_index).is_some())
                        && !c.is_interface,
                    tparam: ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                        signatures
                            .iter()
                            .find(|(field, _)| field == &property.name)
                            .and_then(|(_, parameter)| {
                                c.type_params
                                    .iter()
                                    .position(|candidate| candidate == parameter)
                            })
                            .map(|index| index as u32)
                    }),
                    receiver: None,
                    getter,
                    setter,
                    field_desc: backing
                        .filter(|(_, field)| property.ty != field.ty)
                        .map(|(_, field)| desc(field.ty)),
                    // The PHYSICAL field name when the JVM realization mangles it — an instance
                    // property beside a same-named hoisted companion static (`result` → `result$1`).
                    field_name: backing
                        .map(|(_, field)| instance_field_jvm_name(ir, c, field))
                        .filter(|physical| *physical != property.name),
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
                name: prop.name.clone(),
                ty: prop.ty,
                is_var: prop.is_var,
                visibility: prop.visibility,
                has_constant: true,
                is_const: true,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: None,
                setter: None,
                field_desc: None,
                field_name: None,
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
                    method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)),
                )
            })
        };
        props.push(PropMeta {
            name: ext.name.clone(),
            ty: ext.ty,
            is_var: ext.is_var,
            visibility: ext.visibility,
            has_constant: false,
            is_const: false,
            is_abstract: false,
            has_backing_field: false,
            tparam: None,
            receiver: Some(ext.receiver),
            getter: accessor_sig(ext.getter),
            setter: ext.setter.and_then(accessor_sig),
            field_desc: None,
            field_name: None,
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
            arg.name
                .as_ref()
                .map(|name| (name.clone(), arg.ty, arg.has_default, arg.type_param))
        })
        .collect();
    let ctor_params_with_defaults = if named_ctor_args.is_empty() {
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
    // A boxed nullable primitive (`Int?` → `Ljava/lang/Integer;`): its JVM descriptor is not derivable
    // from the proto type alone, so kotlinc records a `JvmMethodSignature` on any synthesized method
    // whose param/return is one. The descriptor (name derivable) is emitted only when needed.
    let is_boxed_prim = |t: Ty| t.is_nullable() && t.non_null().is_jvm_scalar();
    let boxed_fn_sig = |params: &[Ty], ret: Ty| -> Option<String> {
        (is_boxed_prim(ret) || params.iter().copied().any(is_boxed_prim))
            .then(|| method_descriptor(params, ret))
    };
    let declared_methods = || {
        declared_fids
            .iter()
            .filter_map(|&fid| {
                let f = ir.functions.get(fid as usize)?;
                // Real parameter NAMES — metadata is reflection-visible, so a placeholder would be an
                // observable lie. Positional fallback only when the IR has no recorded names.
                let names = ir.param_names(fid);
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
                let (name, params, ret) =
                    declared.unwrap_or((f.name.as_str(), f.params.as_slice(), f.ret));
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
                // A member EXTENSION realized its receiver as `params[0]` — restore it to
                // `Function.receiver_type` so the record's value parameters are the LOGICAL ones.
                // Both parameter side tables use the physical IR order. An extension receiver is the
                // first parameter, while Kotlin metadata exposes it separately from value parameters.
                let is_ext =
                    ir.extension_receiver_fns.contains(&fid) && !metadata_params.is_empty();
                let recv_offset = usize::from(is_ext);
                let apply_nullable = |i_full: usize, t: crate::types::Ty| {
                    if declared_nullable
                        .and_then(|v| v.get(i_full))
                        .copied()
                        .unwrap_or(false)
                    {
                        crate::types::Ty::nullable(t)
                    } else {
                        t
                    }
                };
                let receiver = is_ext.then(|| apply_nullable(0, metadata_params[0]));
                let logical_params: Vec<(String, crate::types::Ty)> = metadata_params
                    [recv_offset..]
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        // `fn_params` leads with the extension receiver (`$this$<fn>`) when one exists,
                        // exactly like `param_defaults`; read past that explicitly recorded IR slot.
                        let n = names
                            .and_then(|ns| ns.get(i + recv_offset).cloned())
                            .unwrap_or_else(|| format!("p{i}"));
                        (n, apply_nullable(i + recv_offset, *t))
                    })
                    .collect();
                // Per-parameter DECLARES_DEFAULT_VALUE — recorded so a cross-module caller may
                // OMIT a defaulted member argument (the `$default` synthetic realizes the call).
                let param_defaults: Vec<bool> = ir
                    .param_defaults(fid)
                    .map(|ds| ds.iter().skip(recv_offset).map(|d| d.is_some()).collect())
                    .unwrap_or_default();
                Some(FnMeta {
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
                    // The physical descriptor rides along whenever a reader could not derive it from
                    // the proto types: a VC/suspend-rewritten member (`declared`), a signature
                    // mentioning a TYPE PARAMETER (`vararg parts: T` erases to `[Ljava/lang/Object;`
                    // — nothing in the record names that), or a vararg (kotlinc records it there
                    // too). Derivable signatures omit it, kotlinc's usual shape.
                    jvm_sig: (declared.is_some()
                        || ir.fn_vararg_index.contains_key(&fid)
                        || matches!(metadata_ret, crate::types::Ty::TyParam(..))
                        || metadata_params
                            .iter()
                            .any(|parameter| matches!(parameter, crate::types::Ty::TyParam(..))))
                    .then(|| crate::jvm::names::method_descriptor(&f.params, f.ret)),
                    jvm_sig_name: (name != f.name).then(|| f.name.clone()),
                })
            })
            .collect::<Vec<_>>()
    };
    let methods: Vec<FnMeta> = if c.is_data {
        let class_ty = Ty::obj(&c.fq_name());
        let field_tys: Vec<Ty> = data_component_fields.iter().map(|f| f.ty).collect();
        let mut m: Vec<FnMeta> = data_component_fields
            .iter()
            .enumerate()
            .map(|(i, f)| FnMeta {
                name: format!("component{}", i + 1),
                params: vec![],
                ret: f.ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: boxed_fn_sig(&[], f.ty),
                jvm_sig_name: None,
            })
            .collect();
        if synthesizes_copy {
            m.push(FnMeta {
                name: "copy".into(),
                params: data_component_fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty))
                    .collect(),
                ret: class_ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COPY_FN_FLAGS,
                params_have_defaults: true,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: boxed_fn_sig(&field_tys, class_ty),
                jvm_sig_name: None,
            });
        }
        m.push(FnMeta {
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
        });
        m.push(FnMeta {
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
        });
        m.push(FnMeta {
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
        });
        m.extend(declared_methods());
        m
    } else if c.is_value {
        // A value class's Kotlin-visible overrides. Each dispatches to a differently-named static
        // `-impl` taking the erased underlying, so each records a `JvmMethodSignature` (name + desc).
        let u = desc(c.fields[0].ty);
        vec![
            FnMeta {
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
            },
            FnMeta {
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
            },
            FnMeta {
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
            },
        ]
    } else {
        declared_methods()
    };
    let member_order = if c.is_data || c.is_value {
        Vec::new()
    } else {
        let mut ordered = Vec::with_capacity(props.len() + methods.len());
        ordered.extend(
            prop_source_orders
                .iter()
                .copied()
                .enumerate()
                .map(|(index, order)| (order, ClassMemberOrder::Property(index))),
        );
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
        ordered.sort_by_key(|(order, _)| *order);
        ordered.into_iter().map(|(_, member)| member).collect()
    };
    // A value class's primary constructor is realized as the static `constructor-impl` returning the
    // erased underlying, not `<init>`; its `@Metadata` signature records that.
    let vc_ctor_desc = c
        .is_value
        .then(|| format!("({0}){0}", desc(c.fields[0].ty)));
    // `Class.enumEntry` (f13) — the builder has always accepted these; only the caller withheld them.
    let enum_entry_names: Vec<String> = c.enum_entries.iter().map(|e| e.name.clone()).collect();
    // Metadata keeps nested declarations ordered and sealed subclasses sorted.
    // Every DECLARED direct nested classifier joins `Class.nestedClassName` (f7) — kotlinc records
    // them all, not only sealed subtypes. Declaration origin and the exact identity-tree relation
    // keep synthesized classes out without interpreting their backend spellings.
    let mut nested_names: Vec<String> = ir
        .classes
        .iter()
        .filter(|candidate| {
            candidate.is_source_declared
                && !candidate.is_local_class
                && candidate.fq_name.nested_owner() == Some(c.fq_name)
        })
        .map(|candidate| candidate.fq_name.nested_segment_ref().to_string())
        .collect();
    // kotlinc lists the companion under `nestedClassName` (f7) TOO, alongside its own
    // `companionObjectName` (f4) record — both reference the same interned string.
    if let Some(companion) = &c.companion_class {
        let segment = companion.nested_segment_ref().to_string();
        if !nested_names.contains(&segment) {
            nested_names.push(segment);
        }
    }
    // `Class.sealedSubclassFqName` (f16) belongs only to a SEALED classifier — the IR records
    // subtype relationships for every class, but kotlinc writes the field for sealed ones alone
    // (a plain interface with implementors carries none).
    let sealed_sorted = if c.is_sealed {
        sorted_sealed_subclasses(c)
    } else {
        Vec::new()
    };
    let sealed_descs: Vec<String> = sealed_sorted.iter().map(|s| format!("L{s};")).collect();
    let nested_refs: Vec<&str> = nested_names.iter().map(String::as_str).collect();
    let sealed_refs: Vec<&str> = sealed_descs.iter().map(String::as_str).collect();
    let class_type_parameters = ir
        .class_signature(&c.fq_name())
        .map(|signature| signature.type_params.as_slice())
        .unwrap_or_default();
    // Metadata lists the declared superclass before interfaces.
    let super_internal = c.superclass.render();
    let mut supertypes = ir
        .class_signature(&c.fq_name())
        .filter(|signature| !signature.supers.is_empty())
        .map(|signature| signature.supers.clone())
        .unwrap_or_default();
    if supertypes.is_empty() {
        if super_internal != "kotlin/Any" {
            supertypes.push(Ty::obj(&super_internal));
        }
        supertypes.extend(c.interfaces.iter_ids().map(Ty::obj_name));
    }
    // DECLARED secondary constructors → `Class.constructor` records (flags 22 = public secondary),
    // described from their recorded source names + SEMANTIC types (fun-type parameters keep their
    // shape — `Cfg.() -> Unit` — where the erased realization is a bare `Function1`). Synthetic
    // ctors get no record, matching kotlinc.
    let secondary_ctor_shapes: Vec<SecondaryCtorShape> = c
        .secondary_ctors
        .iter()
        .filter(|sc| !sc.synthetic)
        .map(|sc| SecondaryCtorShape {
            params: sc.named_params.clone(),
            desc: format!(
                "({}{})V",
                sc.params.iter().map(|&t| desc(t)).collect::<String>(),
                if sc.vc_params {
                    "Lkotlin/jvm/internal/DefaultConstructorMarker;"
                } else {
                    ""
                }
            ),
            vararg_index: sc.vararg_index,
        })
        .collect();
    let secondary_ctor_metas: Vec<crate::metadata::class_builder::CtorMeta> = secondary_ctor_shapes
        .iter()
        .map(|shape| crate::metadata::class_builder::CtorMeta {
            params: &shape.params,
            desc: &shape.desc,
            vararg_index: shape.vararg_index,
            flags: crate::metadata::class_builder::SECONDARY_CTOR_FLAGS,
        })
        .collect();
    let (d1_bytes, d2) = build_class(
        &c.fq_name(),
        &ctor_params,
        vc_ctor_desc.as_deref().unwrap_or(&ctor_desc),
        &props,
        &methods,
        &enum_entry_names,
        &ClassTail {
            type_params: &c.type_params,
            type_param_bounds: class_type_parameters,
            captured_type_params: &c.captured_type_params,
            ctor_param_tparams: &ctor_param_tparams,
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
            inline_underlying: c
                .is_value
                .then(|| (c.fields[0].name.as_str(), c.fields[0].ty)),
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
            sealed_subclasses: &sealed_refs,
            supertypes: &supertypes,
            annotations: &c.applied_annotations,
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
/// - declared in another file of this MODULE: unknown here (that file's record is decided by its own
///   emit), so the answer is no;
/// - anything else is on the CLASSPATH, where value-class-ness is itself decoded from the `@Metadata`
///   inline record — being known as a value class at all IS the evidence that a record exists.
fn value_class_is_readable(ir: &IrFile, fq_name: crate::types::TypeName) -> bool {
    if let Some(declared) = ir.classes.iter().find(|other| other.fq_name == fq_name) {
        return value_class_metadata_shape_admitted(ir, declared);
    }
    !ir.module_source_value_classes.contains(&fq_name)
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
        // A DECLARED secondary constructor is described (`Class.constructor`, flags 22) from its
        // recorded source names + semantic types; one without that record (an unmodeled synthesis
        // path) would be published with wrong parameters, so the class declines instead. Synthetic
        // ctors (`@Serializable` deserialization) get no record, matching kotlinc.
        || c.secondary_ctors
            .iter()
            .any(|sc| !sc.synthetic && sc.named_params.len() != sc.params.len())
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
        && class_metadata_declares_only_synthesized_members(ir, c)
}

/// Whether the VALUE class `c` declares nothing beyond the members a value class synthesizes (the
/// `-impl` family, the `Any` overrides, and its own field accessor) — part of the condition under which
/// [`build_class_metadata`] describes it at all.
fn class_metadata_declares_only_synthesized_members(ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    const SYNTHESIZED: [&str; 10] = [
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
    ];
    c.methods.iter().all(|&fid| {
        let name = &ir.functions[fid as usize].name;
        SYNTHESIZED.contains(&name.as_str())
            || c.fields.iter().any(|f| {
                let (getter, setter) = accessor_jvm_names(c, &f.name);
                *name == getter || *name == setter
            })
            || c.properties
                .iter()
                .any(|property| property.getter == Some(fid) || property.setter == Some(fid))
    })
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
    let owner = class.fq_name();
    let descriptor = type_descriptor(ir_ty_to_jvm(&field.ty));
    let conflicts_with_static = ir.statics.iter().any(|static_field| {
        static_field.owner_matches(&owner)
            && static_field.name == field.name
            && type_descriptor(ir_ty_to_jvm(&static_field.ty)) == descriptor
    });
    if !conflicts_with_static {
        return field.name.clone();
    }
    for suffix in 1usize.. {
        let candidate = format!("{}${suffix}", field.name);
        let occupied_by_instance = class.fields.iter().any(|other| {
            other.name == candidate && type_descriptor(ir_ty_to_jvm(&other.ty)) == descriptor
        });
        let occupied_by_static = ir.statics.iter().any(|static_field| {
            static_field.owner_matches(&owner)
                && static_field.name == candidate
                && type_descriptor(ir_ty_to_jvm(&static_field.ty)) == descriptor
        });
        if !occupied_by_instance && !occupied_by_static {
            return candidate;
        }
    }
    unreachable!("an unused JVM backing-field suffix always exists")
}

fn seed_plain_class_pool(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fq_name: &str,
    superclass: &str,
    ctor_signature: Option<&str>,
    cw: &mut ClassWriter,
) {
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    // Reference-type annotation kind: 0 = primitive or bare type parameter (no annotation), 1 =
    // non-null reference (@NotNull + a `checkNotNullParameter` guard), 2 = nullable (@Nullable, no guard).
    let ann_kind = |name: &str, t: Ty| -> u8 {
        let d = desc(t);
        if !(d.starts_with('L') || d.starts_with('[')) || is_type_parameter_field(ir, fq_name, name)
        {
            0
        } else if matches!(t, Ty::Nullable(_)) {
            2
        } else {
            1
        }
    };
    let ctor_desc = format!("({})V", ctor_field_descs(c));
    let body_consts = init_body_string_consts(ir, c);
    let body_value_class_ctors = init_body_value_class_ctors(ir, c);
    let stored = init_body_stored_fields(ir, c);
    // A static-storage object's `<init>` stores nothing (initializers run in `<clinit>`, emitted
    // last) — its fields first appear at their GETTERS, which the seeder already orders correctly
    // for never-stored fields.
    let statics_storage = static_storage(ir, c);
    let fields: Vec<crate::jvm::classfile::SeedField> = c
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| crate::jvm::classfile::SeedField {
            name: instance_field_jvm_name(ir, c, f),
            desc: desc(f.ty),
            ann_kind: ann_kind(&f.name, f.ty),
            is_ctor_param: i < c.ctor_param_count as usize,
            stores_in_ctor: !statics_storage
                && (i < c.ctor_param_count as usize || stored.contains(&(i as u32))),
            string_const: body_consts
                .get(&(i as u32))
                .filter(|_| !statics_storage)
                .cloned(),
            value_class_ctor: body_value_class_ctors.get(&(i as u32)).cloned(),
        })
        .collect();
    // Generic `Signature`s for PARAMETERIZED-type members (`List<String>` → `Ljava/util/List<Ljava/lang/String;>;`).
    // Only for a class with NO bare type-parameter fields — a generic class's bare-`T` members are handled by
    // the existing tparam path, left untouched. Seeded here so the natural emission (add_field_sig/
    // add_method_sig) dedupes to kotlinc's interning positions.
    // A field's generic `Signature`: a bare type parameter (`val a: T` → `TT;`), else a parameterized
    // concrete type (`List<String>`). Disjoint — a field is one or the other.
    let field_sig_of = |f: &crate::ir::IrField| -> Option<String> {
        ir.field_signatures(fq_name)
            .and_then(|fs| {
                fs.iter()
                    .find(|(name, _)| name == &f.name)
                    .map(|(_, tp)| format!("T{tp};"))
            })
            .or_else(|| parameterized_sig(formatter, &f.ty))
    };
    let ctor_sig = ctor_signature;
    let field_sigs: Vec<Option<String>> = c.fields.iter().map(field_sig_of).collect();
    // A data class's accessor signatures join its accessor window below, while its backing-field
    // signatures land late after the synthesized data methods. Ordinary classes intern both naturally
    // at the exact accessor/field visits.
    let (super_param_tys, _) = super_ctor_jvm_tys(ir, c, superclass, |a| {
        ir.logical_types
            .get(&a)
            .cloned()
            .map(|ty| ir_ty_to_jvm(&ty))
            .unwrap_or(Ty::obj("kotlin/Any"))
    });
    let super_ctor_desc = crate::jvm::names::method_descriptor(&super_param_tys, Ty::Unit);
    // The primary ctor's `$default` overload interning window (marker desc, default STRING
    // constants, delegating `<init>` ref) — kotlinc writes the synthetic right after the primary.
    let ctor_default_seed = ir
        .class_ctor_defaults(fq_name)
        .filter(|defaults| defaults.iter().any(Option::is_some))
        .map(|defaults| {
            let masks = "I".repeat(defaults.len().div_ceil(32).max(1));
            crate::jvm::classfile::SeedCtorDefaults {
                marker_desc: format!(
                    "({}{masks}Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                    ctor_field_descs(c)
                ),
                string_consts: defaults
                    .iter()
                    .flatten()
                    .filter_map(|&d| match ir.expr(d) {
                        IrExpr::Const(crate::ir::IrConst::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
            }
        });
    cw.seed_plain_class_pool(
        fq_name,
        superclass,
        (&ctor_desc, &super_ctor_desc),
        &fields,
        &crate::jvm::classfile::MemberSignatures {
            ctor: ctor_sig,
        },
        ctor_default_seed.as_ref(),
        &{
            use crate::jvm::classfile::SeedSuperArg;
            fn collect(ir: &IrFile, expr: crate::ir::ExprId, entries: &mut Vec<SeedSuperArg>) {
                match ir.expr(init_operand(ir, expr)) {
                    IrExpr::Const(crate::ir::IrConst::String(s)) => {
                        entries.push(SeedSuperArg::Str(s.clone()));
                    }
                    IrExpr::New {
                        internal,
                        args,
                        ctor_params,
                        ctor_desc,
                    } => {
                        let owner = internal.render();
                        entries.push(SeedSuperArg::Class(owner.clone()));
                        for &arg in args {
                            collect(ir, arg, entries);
                        }
                        let desc = if let Some(desc) = ctor_desc {
                            desc.clone()
                        } else if let Some(params) = ctor_params {
                            method_descriptor(&jvm_tys(params), Ty::Unit)
                        } else {
                            let class = ir.class_id_by_name(*internal).expect(
                                "checked construction without explicit parameters must name an IR class",
                            );
                            method_descriptor(
                                &class_ctor_jvm_tys(&ir.classes[class as usize]),
                                Ty::Unit,
                            )
                        };
                        entries.push(SeedSuperArg::Ctor { owner, desc });
                    }
                    _ => {}
                }
            }

            let mut entries: Vec<SeedSuperArg> = Vec::new();
            for &arg in &c.super_args {
                collect(ir, arg, &mut entries);
            }
            entries
        },
    );
    // A companion OUTER's `access$…$cp` bridges, `<clinit>`, and hoisted-initializer constants are
    // NOT seeded here: kotlinc interns them at their natural emission position — after the declared
    // member methods (whose bodies intern their own constants in between) — so `emit_class` reserves
    // each name at its emission site instead.
    if synthesizes_data_class_members(c) {
        let simple = fq_name.rsplit('/').next().unwrap_or(fq_name);
        // The synthesized members cover the PRIMARY-CONSTRUCTOR properties only; a body property has a
        // backing field in `c.fields` but no `componentN` and no `copy` parameter (see
        // `build_class_metadata`, which takes the same prefix).
        let component_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
        let data_fields: Vec<(String, String)> = component_fields
            .iter()
            .map(|f| (f.name.clone(), desc(f.ty)))
            .collect();
        // Per-field `hashCode` owner (interface field → `java/lang/Object`), recorded by `field_hash`.
        let hashcode_owners: Vec<Option<String>> = component_fields
            .iter()
            .map(|f| ir.data_hashcode_owner(fq_name, &f.name).map(str::to_string))
            .collect();
        let mut data_accessors = Vec::new();
        for property in &c.properties {
            if property.is_private {
                continue;
            }
            let Some(field) = property
                .backing_field
                .and_then(|index| c.fields.get(index as usize))
            else {
                continue;
            };
            let accessor_ty = declared_property_accessor_jvm(ir, property, field);
            let accessor_desc = desc(accessor_ty);
            let field_sig = field_sig_of(field);
            let getter = property
                .getter_jvm_name
                .clone()
                .unwrap_or_else(|| crate::names::property_getter_name(&property.name));
            data_accessors.push(crate::jvm::classfile::DataAccessorInfo {
                name: getter,
                desc: format!("(){accessor_desc}"),
                setter_kind: 0,
                signature: field_sig.as_ref().map(|signature| format!("(){signature}")),
            });
            if property.is_var {
                let setter = property
                    .setter_jvm_name
                    .clone()
                    .unwrap_or_else(|| crate::names::property_setter_name(&property.name));
                let guarded = accessor_ty.is_reference()
                    && !property.ty.is_nullable()
                    && !is_type_parameter_field(ir, fq_name, &field.name);
                data_accessors.push(crate::jvm::classfile::DataAccessorInfo {
                    name: setter,
                    desc: format!("({accessor_desc})V"),
                    setter_kind: if guarded { 2 } else { 1 },
                    signature: field_sig.map(|signature| format!("({signature})V")),
                });
            }
        }
        // `copy`'s generic Signature shares the ctor's parameter list, returning `self` instead of `void`.
        let copy_sig = ctor_sig
            .and_then(|s| s.strip_suffix('V'))
            .map(|params| format!("{params}L{fq_name};"));
        cw.seed_data_class_pool(
            fq_name,
            &ctor_desc,
            simple,
            &data_fields,
            &crate::jvm::classfile::DataMemberInfo {
                accessors: &data_accessors,
                hashcode_owners: &hashcode_owners,
                copy_sig: copy_sig.as_deref(),
                field_sigs: &field_sigs,
            },
        );
    }
}

/// One synthesized value-class member's debug shape: `(jvm name, jvm descriptor, LocalVariableTable
/// entries as `(name, descriptor, slot)`)`.
type VcDebugMethod = (String, String, Vec<(String, String, u16)>);

/// Attach kotlinc's `LineNumberTable` + `LocalVariableTable` to a class's DECLARED methods (as opposed
/// to the synthesized ctor/accessors handled by [`attach_synth_debug_tables`]). kotlinc maps a method's
/// table to its own `fun` line — recorded per-FunId by the lowering — and lists `this` plus each
/// parameter for the whole method.
fn attach_declared_method_debug(ir: &IrFile, c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let this_desc = format!("L{};", c.fq_name());
    // `aload <slot>` byte length: 1 (aload_0..3), 2 (aload u1), or 4 (wide aload u2).
    let aload_len = |slot: u16| -> u16 {
        if slot <= 3 {
            1
        } else if slot <= 255 {
            2
        } else {
            4
        }
    };
    for &fid in &c.methods {
        let Some(f) = ir.functions.get(fid as usize) else {
            continue;
        };
        let Some(&line) = ir.fn_decl_lines.get(&fid) else {
            continue;
        };
        if f.body.is_none() {
            continue; // abstract: no Code, so no debug tables
        }
        let param_tys = jvm_tys(&f.params);
        let ret = ir_ty_to_jvm(&f.ret);
        let desc = method_descriptor(&param_tys, ret);
        let mut locals: Vec<(String, String, u16)> = Vec::new();
        let mut slot = 0u16;
        if !f.is_static {
            locals.push(("this".to_string(), this_desc.clone(), 0));
            slot = 1;
        }
        // kotlinc attributes the `fun` line to the first instruction of the BODY, not to the
        // `checkNotNullParameter` guards it emits ahead of it — so the entry starts past that
        // prologue, measured the same way the constructor's is.
        let mut body_pc = 0u16;
        for (i, t) in param_tys.iter().enumerate() {
            let name = ir
                .param_names(fid)
                .and_then(|ns| ns.get(i).cloned())
                .or_else(|| f.param_checks.get(i).and_then(|n| n.clone()))
                .unwrap_or_else(|| format!("p{i}"));
            if let Some(Some(guarded)) = f.param_checks.get(i) {
                // guard = aload(slot) + ldc(param-name String) + invokestatic checkNotNullParameter(3)
                body_pc += aload_len(slot) + cw.string_ldc_len(guarded).unwrap_or(2) + 3;
            }
            locals.push((name, crate::jvm::names::type_descriptor(*t), slot));
            slot += slot_words(*t);
        }
        cw.set_method_debug(&f.name, &desc, Some((body_pc, line)), &locals);
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

fn attach_synth_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    param_assertions: bool,
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
    // A non-null reference param carries a `checkNotNullParameter` guard (`aload <slot>; ldc <name>;
    // invokestatic`) before the body; kotlinc's LineNumberTable maps the decl line to the post-prologue
    // offset. The guard's length is SLOT-dependent: `aload_0..3` is 1 byte but `aload <u1>` (slot ≥ 4)
    // is 2, so a class with enough (or wide) ctor params pushes a non-null-ref param past slot 3 and its
    // guard grows — the fixed-6 assumption was wrong there.
    let is_nonnull_ref = |name: &str, t: Ty| -> bool {
        let d = desc(t);
        (d.starts_with('L') || d.starts_with('['))
            && !matches!(t, Ty::Nullable(_))
            && !is_type_parameter_field(ir, &c.fq_name(), name)
    };
    // `aload <slot>` byte length: 1 (aload_0..3), 2 (aload u1), or 4 (wide aload u2).
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
    // Primary constructor: `this` + one local per ctor parameter (a property-backed param). An
    // `enum class`'s ctor is `(String name, int ordinal, …declared params)`: kotlinc prepends the two
    // synthetic `Enum` parameters and names them `$enum$name` / `$enum$ordinal` in the LVT.
    let is_enum = !c.enum_entries.is_empty();
    let ctor_desc = if is_enum {
        format!("(Ljava/lang/String;I{})V", ctor_field_descs(c))
    } else {
        format!("({})V", ctor_field_descs(c))
    };
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
    let mut ctor_pc = 0u16;
    // Only ctor PARAMETERS are constructor locals — a body property is a field, never an argument.
    for f in c.fields.iter().take(c.ctor_param_count as usize) {
        ctor_locals.push((f.name.clone(), desc(f.ty), slot));
        // `-Xno-param-assertions` removed the guards, so the constructor body starts at pc 0. Counting
        // them anyway put the `LineNumberTable` entry past the end of the emitted code, which the JVM
        // rejects outright: `ClassFormatError: Invalid pc in LineNumberTable`.
        if param_assertions && is_nonnull_ref(&f.name, f.ty) {
            // guard = aload(slot) + ldc(param-name String) + invokestatic checkNotNullParameter(3).
            // The ldc width is read off the REAL pool (2, or 3 for `ldc_w` past index 255) — the
            // guard was already emitted, so the String constant exists.
            let ldc = cw.string_ldc_len(&f.name).unwrap_or(2);
            ctor_pc += aload_len(slot) + ldc + 3;
        }
        slot += slot_size(f.ty);
    }
    let this_only = [("this".to_string(), this_desc.clone(), 0u16)];
    cw.set_method_debug("<init>", &ctor_desc, Some((ctor_pc, line)), &ctor_locals);
    if !ctor_lines.is_empty() {
        let mut entries = vec![(ctor_pc, line)];
        entries.extend_from_slice(ctor_lines);
        // kotlinc never emits two consecutive entries for the same line — a run of stores on the
        // class-declaration line (a single-line `class C(val a: Int)`) collapses to one entry.
        entries.dedup_by_key(|(_, l)| *l);
        cw.set_method_lines("<init>", &ctor_desc, &entries);
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
    // Property accessors: getter has only `this`; a `var` setter also has its value parameter (named
    // `<set-?>` by kotlinc), guarded when the property type is a non-null reference.
    for f in &c.fields {
        // A CTOR-parameter property's accessors sit on the class-declaration line; a BODY property's
        // sit on its own `val`/`var` line.
        let pline = ir
            .prop_decl_lines
            .get(&(c.fq_name_id(), f.name.clone()))
            .copied()
            .filter(|&l| l != 0)
            .unwrap_or(line);
        let (g, s) = accessor_jvm_names(c, &f.name);
        cw.set_method_debug(
            &g,
            &format!("(){}", desc(f.ty)),
            Some((0, pline)),
            &this_only,
        );
        if !f.is_final() {
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
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some() {
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
        let pd = crate::jvm::names::type_descriptor(ir_ty_to_jvm(&hoisted.ty));
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
            ir.is_jvm_companion_hoisted_static(*index as u32) && s.owner_matches(&c.fq_name())
        })
        .map(|(_, s)| s)
    {
        let pd = crate::jvm::names::type_descriptor(ir_ty_to_jvm(&s.ty));
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
                        ("p1".to_string(), u.clone(), 0),
                        ("p2".to_string(), u.clone(), w),
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
                    "{ctor_desc_no_v}{self_ref}",
                    ctor_desc_no_v = &ctor_desc[..ctor_desc.len() - 1]
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
        let d = desc(t);
        if !(d.starts_with('L') || d.starts_with('['))
            || is_type_parameter_field(ir, &c.fq_name(), name)
        {
            return None;
        }
        Some(if matches!(t, Ty::Nullable(_)) {
            "Lorg/jetbrains/annotations/Nullable;"
        } else {
            "Lorg/jetbrains/annotations/NotNull;"
        })
    };
    // Interfaces have accessors but no backing fields. The annotation targets the PHYSICAL field
    // (`result$1` when mangled away from a same-named hoisted companion static).
    if !c.is_interface {
        for f in &c.fields {
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
            if let Some(a) = ann(&s.name, ir_ty_to_jvm(&s.ty)) {
                cw.set_field_nullability(&s.name, a);
            }
        }
    }
    // Primary constructor: one parameter annotation slot per property-backed parameter.
    // Constructor PARAMETERS only — a body property is a field, never an argument, so it must not
    // contribute a parameter-annotation slot (an all-body-property class has a `()V` ctor).
    let ctor_params: Vec<Option<&str>> = c
        .fields
        .iter()
        .take(c.ctor_param_count as usize)
        .map(|f| ann(&f.name, f.ty))
        .collect();
    if ctor_params.iter().any(|p| p.is_some()) {
        let ctor_desc = format!("({})V", ctor_field_descs(c));
        cw.set_method_nullability("<init>", &ctor_desc, None, &ctor_params);
    }
    // HOISTED companion properties: the delegating accessors annotate like ordinary accessors
    // (reference getter return; a `var` reference setter's parameter).
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some() {
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
        let self_ref = format!("L{};", c.fq_name());
        let data_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
        // A `data object` synthesizes no `copy` (see the metadata assembly), so it takes no annotations.
        if !data_fields.is_empty() {
            let copy_desc = format!("({}){self_ref}", ctor_field_descs(c));
            // `copy`'s parameters mirror the primary-constructor properties, so each reference param
            // takes the SAME `@NotNull`/`@Nullable` annotation kotlinc puts on the constructor's.
            let copy_params: Vec<Option<&str>> =
                data_fields.iter().map(|f| ann(&f.name, f.ty)).collect();
            cw.set_method_nullability("copy", &copy_desc, Some(not_null), &copy_params);
        }
        cw.set_method_nullability("toString", "()Ljava/lang/String;", Some(not_null), &[]);
        cw.set_method_nullability(
            "equals",
            "(Ljava/lang/Object;)Z",
            None,
            &[Some("Lorg/jetbrains/annotations/Nullable;")],
        );
        for (i, f) in data_fields.iter().enumerate() {
            if let Some(a) = ann(&f.name, f.ty) {
                cw.set_method_nullability(
                    &format!("component{}", i + 1),
                    &format!("(){}", desc(f.ty)),
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
                cw.set_method_nullability("constructor-impl", &format!("({u}){u}"), Some(a), &[]);
            }
        }
    }
}

/// Register the file's nested-class `InnerClasses` candidates on `cw`; the writer's `finish` keeps only
/// the entries it references as a class constant (kotlinc's rule). Covers the `@Serializable` model
/// shape — a class's `$$serializer` (inner name `$serializer`) and its `Companion`, both `public static
/// final` — emitted in kotlinc's order ($serializer before Companion). Anonymous nested classes (the
/// suspend continuations) are not yet registered (they also need an `EnclosingMethod` attribute).
fn inner_class_access(ir: &IrFile, c: &IrClass) -> u16 {
    const PUBLIC: u16 = 0x0001;
    const STATIC: u16 = 0x0008;
    const FINAL: u16 = 0x0010;
    const INTERFACE: u16 = 0x0200;
    const ABSTRACT: u16 = 0x0400;
    const ANNOTATION: u16 = 0x2000;
    const ENUM: u16 = 0x4000;

    // Inner/static nesting is a source-level class property, not a consequence of a field spelling.
    // In particular, another synthetic class may conventionally call an ordinary capture `this$0`.
    let is_inner = c.is_inner_class;
    // The InnerClasses access carries the SOURCE visibility (`protected class Category` reads
    // `protected static final` there), unlike the class's own access flags.
    let visibility = match ir.class_visibilities.get(&c.fq_name_id()) {
        Some(crate::types::Visibility::Protected) => 0x0004,
        Some(crate::types::Visibility::Private) => 0x0002,
        _ => PUBLIC,
    };
    let mut access = visibility | if is_inner { 0 } else { STATIC };
    if c.is_annotation {
        access |= INTERFACE | ABSTRACT | ANNOTATION;
    } else if c.is_interface {
        access |= INTERFACE | ABSTRACT;
    } else if !c.enum_entries.is_empty() {
        access |= FINAL | ENUM;
    } else if c.is_sealed || c.is_abstract {
        access |= ABSTRACT;
    } else if !c.is_open {
        access |= FINAL;
    }
    access
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
    if class.is_interface {
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

fn add_singleton_instance_field(cw: &mut ClassWriter, class: &str) {
    cw.add_field(0x0019, "INSTANCE", &format!("L{class};"));
}

fn emit_singleton_instance_clinit(cw: &mut ClassWriter, class: &str) {
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

fn register_inner_classes(cw: &mut ClassWriter, ir: &IrFile) {
    use crate::jvm::classfile::InnerClassSpec;
    for c in &ir.classes {
        let fq = c.fq_name();
        if let Some(outer) = fq.strip_suffix("$$serializer") {
            cw.add_inner_class(InnerClassSpec {
                inner: fq.clone(),
                outer: Some(outer.to_string()),
                name: Some("$serializer".to_string()),
                access: inner_class_access(ir, c),
            });
        }
    }
    for c in &ir.classes {
        if let Some(comp) = c.companion_class() {
            cw.add_inner_class(InnerClassSpec {
                inner: comp,
                outer: Some(c.fq_name()),
                name: c
                    .companion_class
                    .map(|companion| companion.nested_segment_ref().to_string()),
                access: c
                    .companion_class
                    .and_then(|name| {
                        ir.classes
                            .iter()
                            .find(|candidate| candidate.fq_name_id() == name)
                    })
                    .map_or(0x0019, |candidate| inner_class_access(ir, candidate)),
            });
        }
    }
    // `finish` retains only nested classes referenced by this classfile.
    for c in &ir.classes {
        let fq = c.fq_name();
        if fq.ends_with("$$serializer") {
            continue; // handled above (special inner name `$serializer`)
        }
        if c.is_anonymous_object {
            cw.add_inner_class(InnerClassSpec {
                inner: fq,
                outer: None,
                name: None,
                access: 0x0019,
            });
            continue;
        }
        // Where the OUTER class ends is not the last `$`: a backticked declaration may carry `$` in
        // its own simple name (`class \`Nested$With$Dollars\``), and splitting there named an outer
        // class that does not exist — the loader then failed with `NoClassDefFoundError` on the
        // invented name. The boundary is the longest proper prefix that is ITSELF a class of this
        // file; only when no declared class is a prefix does the textual split stand in (an outer
        // this file does not declare).
        let Some(pos) = ir
            .classes
            .iter()
            .map(IrClass::fq_name)
            .filter(|outer| {
                outer.len() < fq.len()
                    && fq.starts_with(outer.as_str())
                    && fq.as_bytes()[outer.len()] == b'$'
            })
            .map(|outer| outer.len())
            .max()
            .or_else(|| fq.rfind('$'))
        else {
            continue; // top-level class — not nested
        };
        let name = &fq[pos + 1..];
        if c.is_companion {
            continue; // handled above
        }
        let anonymous = is_coroutine_state_machine(c);
        // A LOCAL class is not a member of anything: its name is qualified by the DECLARATION it
        // was written in, so the text before the last `$` names no class. The JVM spells that with
        // `outer_class_info_index = 0` and a non-zero `inner_name_index` — which is also what
        // reflection reads back as `simpleName`. Treating the prefix as an outer class makes the
        // loader look for a class that does not exist. An ANONYMOUS class carries neither outer nor
        // simple name (kotlinc's inner-only entry, access `public static final`).
        let member = !anonymous && !c.is_local_class;
        cw.add_inner_class(InnerClassSpec {
            inner: fq.clone(),
            outer: member.then(|| fq[..pos].to_string()),
            name: (!anonymous).then(|| name.to_string()),
            access: if anonymous {
                0x0008 | 0x0010
            } else {
                inner_class_access(ir, c)
            },
        });
    }
}

fn sorted_sealed_subclasses(c: &IrClass) -> Vec<String> {
    let mut subclasses: Vec<String> = c.sealed_subclasses.iter_rendered().collect();
    subclasses.sort_by(|a, b| {
        let a_simple = a.rsplit(['$', '/']).next().unwrap_or(a);
        let b_simple = b.rsplit(['$', '/']).next().unwrap_or(b);
        a_simple.cmp(b_simple).then_with(|| a.cmp(b))
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
    let self_fq = c.fq_name();
    let subs = sorted_sealed_subclasses(c);
    if subs.is_empty() {
        return;
    }
    for sub in &subs {
        if let Some(name) = sub.strip_prefix(&format!("{self_fq}$")) {
            cw.seed_class(sub);
            cw.add_inner_class(InnerClassSpec {
                inner: sub.clone(),
                outer: Some(self_fq.clone()),
                name: Some(name.to_string()),
                access: ir
                    .classes
                    .iter()
                    .find(|candidate| candidate.fq_name_matches(sub))
                    .map_or(0x0019, |candidate| inner_class_access(ir, candidate)),
            });
        }
    }
    if emit_permitted {
        for sub in &subs {
            cw.seed_class(sub);
        }
        cw.set_permitted_subclasses(subs);
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
    let signature = ir
        .class_signature_name(c.fq_name)
        .and_then(|signature| jvm_class_signature(&JvmSignatureFormatter::new(env), signature));
    let internal = c.fq_name();
    // kotlinc (ASM) visits `(name, signature, superName)`, so the signature VALUE interns between
    // the two class names — it must reach the writer's constructor, not only `set_signature`.
    let mut cw = new_writer_generic(&internal, signature.as_deref(), super_internal, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    cw
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

pub fn emit_all(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
    symbols: &crate::frontend::FrontendSymbols,
) -> Option<Vec<(String, Vec<u8>)>> {
    // [`EmitOptions::default`]: per-class `@Metadata` ON, as on the shipping path — what this default
    // lacks is the `SourceFile`, the inner-class resolver and any `-jvm-target` class version, so it is
    // NOT the artifact `krusty -d …` writes. A caller that must emit the shipping bytes (the
    // byte-identity gates, the conformance corpus, `survey`) goes through
    // [`crate::jvm::backend::shipping_emit_options`] and the `emit_all_with_opts*` entry points; a
    // caller that must attach a per-class `@Metadata` it computed elsewhere uses
    // [`emit_all_with_class_meta`], which this passes a provider returning `None` for every class. The
    // run accumulators are discarded here (callers that need the inline-bail reason use
    // `emit_all_with_opts` with their own `EmitRun`).
    let run = EmitRun::default();
    let empty_continuation_metadata = crate::jvm::suspend::ContinuationMetadataMap::default();
    let module = crate::module_symbols::ModuleSymbols::new(symbols);
    let signature_symbols = CompositeSource::new(vec![&module, &*symbols.libraries]);
    let env = EmitEnv {
        bodies,
        run: &run,
        continuation_metadata: &empty_continuation_metadata,
        signature_symbols: &signature_symbols,
        jvm_default: JvmDefaultMode::default(),
        lambdas: LambdaMode::Indy,
    };
    emit_all_with_class_meta(ir, facade, &env, metadata, &EmitOptions::default(), &|_| {
        None
    })
}

/// Like [`emit_all`], but with explicit per-file [`EmitOptions`] (class version, source name) and a
/// caller-owned [`EmitRun`] the caller inspects after a `None` return (the inline-bail reason). Every
/// shipping-bytes path uses this — the CLI backend, `survey`, the conformance corpus and the
/// in-process test helpers — so `-jvm-target`, the `SourceFile` name and the inner-class resolver reach
/// every emitted class.
pub fn emit_all_with_opts(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    run: &EmitRun,
    symbols: &crate::frontend::FrontendSymbols,
) -> Option<Vec<(String, Vec<u8>)>> {
    let continuation_metadata = Default::default();
    emit_all_with_opts_and_metadata(
        ir,
        facade,
        bodies,
        EmitMetadata {
            facade: metadata,
            continuations: &continuation_metadata,
        },
        opts,
        run,
        symbols,
    )
}

/// Semantic metadata emitted beside one file's JVM classes.
pub struct EmitMetadata<'a> {
    pub facade: Option<&'a KotlinMetadata>,
    pub continuations: &'a crate::jvm::suspend::ContinuationMetadataMap,
}

/// Emit classes with continuation metadata produced by the JVM suspend pass.
pub fn emit_all_with_opts_and_metadata(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: EmitMetadata<'_>,
    opts: &EmitOptions,
    run: &EmitRun,
    symbols: &crate::frontend::FrontendSymbols,
) -> Option<Vec<(String, Vec<u8>)>> {
    let module = crate::module_symbols::ModuleSymbols::new(symbols);
    let signature_symbols = CompositeSource::new(vec![&module, &*symbols.libraries]);
    let env = EmitEnv {
        bodies,
        run,
        continuation_metadata: metadata.continuations,
        signature_symbols: &signature_symbols,
        jvm_default: opts.jvm_default,
        lambdas: opts.lambdas,
    };
    emit_all_with_class_meta(ir, facade, &env, metadata.facade, opts, &|_| None)
}

/// Like [`emit_all`], but `class_meta` may supply a per-class `@kotlin.Metadata` (keyed by the class's
/// internal/fq name) attached to that emitted class. This lets a separately-compiled module expose its
/// classes' Kotlin signatures (member source params, etc.) so a dependent module resolves them — the
/// cross-module analogue of the facade `metadata`. OPT-IN: the default [`emit_all`] passes a provider
/// that returns `None` for every class, so krusty-core's emit is unchanged.
pub fn emit_all_with_class_meta(
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
    // Pass 1 (discovery): emit everything, recording which lambda impls actually get an `invokedynamic`
    // (`run.used_lambdas`). A lambda spliced by the inliner never emits one — its standalone `$lambda$N`
    // is dead, and kotlinc emits neither the method nor (for a class-only file) the facade holding it.
    env.run.used_lambdas.borrow_mut().clear();
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
    // `invokedynamic` was added in class-file version 51 (Java 7). Do not return a version-50 class
    // containing an opcode that its declared target cannot represent. This check uses the emitter's
    // discovery result rather than the source/IR lambda count: an inline-only lambda that was fully
    // spliced emitted no call site and therefore remains valid for the older target.
    if opts.class_major.is_some_and(|major| major < 51) && !used.is_empty() {
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
    let class_member_fids: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    // Dead = a FACADE-owned lambda impl (no receiver, not a class member — a class-owned or
    // suspend-state-machine lambda may be reached through paths discovery doesn't model) that no emitted
    // `invokedynamic` references. NB single iteration: an indy inside a dead lambda still marks its inner
    // lambda used, so a nested-dead chain keeps the inner method — rare, and strictly better than today.
    // An anonymous function's impl is reached by an `invokestatic` the SPLICE emits, never by an
    // `invokedynamic` — discovery would otherwise read it as dead and drop the method the splice calls.
    let spliced_as_a_call = splice_called_impls(ir);
    let dead: std::collections::HashSet<u32> = ir
        .lambda_own_params_from
        .keys()
        .filter(|&&fid| {
            !used.contains(&fid)
                && !spliced_as_a_call.contains(&fid)
                && !ir.inline_only_fns.contains(&fid)
                && !class_member_fids.contains(&fid)
                && ir
                    .functions
                    .get(fid as usize)
                    .is_some_and(|f| f.dispatch_receiver.is_none())
                && !ir.suspend_lambda_sm.iter().any(|(f2, _, _)| *f2 == fid)
        })
        .copied()
        .collect();
    if dead.is_empty() && rescued.is_empty() {
        return Some(first);
    }
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

fn emit_pass(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
    lambdas: &LambdaSelection,
) -> Option<Vec<(String, Vec<u8>)>> {
    if !jvm_can_emit(ir) {
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
    let mut cw = new_writer(facade, "java/lang/Object", opts);
    // The facade constructs the file's local classes, and a class that references one as a class
    // constant must list it in `InnerClasses` — reflection cross-checks the two sides and throws
    // `IncompatibleClassChangeError` when only one carries the entry. kotlinc emits it here too.
    register_inner_classes(&mut cw, ir);
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
                    for (i, f) in ir.functions.iter().enumerate() {
                        if f.name == fr.call_name
                            && f.params.len() == fr.target_param_tys.len()
                            && f.dispatch_receiver.is_none()
                            && ir.private_methods.contains(&(i as u32))
                            && !class_member_fids.contains(&(i as u32))
                        {
                            out.insert(i as u32);
                        }
                    }
                }
            }
        }
        out
    };
    let mut facade_has_method = false;
    for (i, f) in ir.functions.iter().enumerate() {
        if class_member_fids.contains(&(i as u32)) {
            continue;
        }
        if f.dispatch_receiver.is_some() || f.body.is_none() {
            continue;
        }
        // An inline-only lambda impl is never emitted (it's spliced) — don't count it as a facade method,
        // else an otherwise class-only file emits an empty facade kotlinc omits. A DEAD lambda impl
        // (inlined at every use — pass-1 discovery) is dropped the same way.
        let rescued = lambdas.rescued.contains(&(i as u32));
        if (ir.inline_only_fns.contains(&(i as u32)) && !rescued)
            || lambdas.dead.contains(&(i as u32))
        {
            continue;
        }
        emit_method_maybe_rescued(ir, i as u32, facade, facade, &mut cw, false, env, rescued);
        facade_has_method = true;
        if facade_access_bridges.contains(&(i as u32)) {
            let param_tys = jvm_tys(&f.params);
            let ret = ir_ty_to_jvm(&f.ret);
            let desc = method_descriptor(&param_tys, ret);
            let words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
            let mut g = CodeBuilder::new(words);
            let mut slot: u16 = 0;
            for &t in &param_tys {
                load(t, slot, &mut g);
                slot += slot_words(t);
            }
            let m = cw.methodref(facade, &f.name, &desc);
            let aw: i32 = words as i32;
            g.invokestatic(m, aw, slot_words(ret) as i32);
            emit_return(ret, &mut g);
            g.ensure_locals(words);
            g.link();
            cw.add_method(
                0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
                &format!("access${}", f.name),
                &desc,
                &g,
            );
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
    emit_statics(ir, facade, &mut cw, env, opts.param_assertions);
    // kotlinc emits the `<File>Kt` facade class ONLY when the file has top-level callables/properties
    // (or a facade `@Metadata` payload). A file of only classes/objects gets no facade — emitting an
    // empty one is an ABI divergence (spurious extra class). A facade static is owner-less.
    let facade_has_static = ir.statics.iter().any(|s| s.is_facade_owned());
    let facade_needed = facade_has_method || facade_has_static || metadata.is_some();
    if facade_needed {
        if let Some(m) = metadata {
            cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
        }
        out.push((facade.to_string(), cw.finish()));
        out.extend(drain_lambda_classes(env, opts));
    }
    // Each class — with its optional `@Metadata` (the provider returns `None` for the default emit).
    for c in &ir.classes {
        let fq_name = c.fq_name();
        let cm = class_meta(&fq_name);
        let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
        out.push((
            fq_name,
            emit_class(ir, c, facade, env, opts, cm.as_ref(), &mut extra),
        ));
        // An interface's `$DefaultImpls` holder (its `name$default` synthetics), when any exist.
        out.extend(extra);
        out.extend(drain_lambda_classes(env, opts));
    }
    out.extend(drain_lambda_classes(env, opts));
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
                unbox_prim(&mut cw, &mut invoke, want);
            } else if wanted != *physical {
                // `Function1.invoke` declares `Object`; the body declares the real type. Under
                // `indy` the metafactory's adapter inserted this cast — nothing else does here, and
                // without it the class fails verification on any non-`Object` reference parameter.
                let internal = wanted
                    .strip_prefix('L')
                    .and_then(|rest| rest.strip_suffix(';'))
                    .map(|name| name.to_string())
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
        invoke.ret_void();
    } else if descriptor_is_reference(sam_ret) {
        invoke.areturn();
    } else {
        return_primitive(&mut invoke, impl_ret);
    }
    // ACC_PUBLIC | ACC_FINAL: the interface slot, implemented once.
    cw.add_method(0x0011, &plan.sam_method, &plan.sam_desc, &invoke);

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
    plans
        .iter()
        .map(|plan| build_lambda_class(plan, opts))
        .collect()
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

/// Whether the JVM backend can represent this IR. The JVM stdlib provides fixed-arity
/// `kotlin/jvm/functions/Function0..22`; a function type or lambda of higher arity needs a different
/// vararg representation krusty doesn't emit, so such a file is skipped — never miscompiled. This is a
/// JVM constraint (the language allows any arity), so it lives in the JVM emitter, not common lowering.
/// Map each reachable `IrExpr::Variable` declaration index to its JVM type for one emitted body.
/// Value indices are body-local and intentionally restart between functions, constructors, and
/// initializers, so a file-wide map lets an unrelated body overwrite the active slot's type.
fn collect_var_types(ir: &IrFile, roots: impl IntoIterator<Item = u32>) -> HashMap<u32, Ty> {
    let mut m = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut pending = roots.into_iter().collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Variable { index, ty, .. } = ir.expr(expression) {
            m.insert(*index, ir_ty_to_jvm(ty));
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    m
}

/// Attach any user annotations recorded for `field` (by name) to the most recently added field.
fn apply_field_annotations(cw: &mut ClassWriter, c: &crate::ir::IrClass, field: &str) {
    if let Some(fa) = c.field_annotations.iter().find(|fa| fa.field == field) {
        cw.set_last_field_annotations(&fa.visible, &fa.invisible);
    }
}

pub(crate) fn jvm_can_emit(ir: &IrFile) -> bool {
    fn ty_ok(t: &Ty) -> bool {
        match t.non_null() {
            Ty::Fun(s) => s.params.len() <= 22 && s.params.iter().all(ty_ok) && ty_ok(&s.ret),
            Ty::Obj(_, type_args) => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    fn callee_ok(callee: &Callee) -> bool {
        match callee {
            Callee::Static { .. } | Callee::Special { .. } => true,
            // A user (sibling-file) method carries `Ty`s; a classpath one a descriptor string.
            Callee::Virtual { params, .. } => match params {
                Some((ps, ret)) => ps.iter().all(ty_ok) && ty_ok(ret),
                None => true,
            },
            Callee::CrossFile { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            Callee::Local(_)
            | Callee::LocalDefault(_)
            | Callee::ClassStatic { .. }
            | Callee::Intrinsic { .. } => true,
        }
    }
    fn generic_value_class_ok(ir: &IrFile, class_idx: usize) -> bool {
        let c = &ir.classes[class_idx];
        if !c.is_value || c.type_params.is_empty() {
            return true;
        }
        if c.fields.iter().any(|f| {
            matches!(
                f.ty.non_null(),
                Ty::Obj(n, _) if n.matches("java/lang/Comparable") || n.matches("kotlin/Comparable")
            )
        }) {
            return false;
        }
        true
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
    if !(0..ir.classes.len()).all(|idx| generic_value_class_ok(ir, idx)) {
        return false;
    }
    ir.exprs.iter().all(|e| match e {
        IrExpr::Lambda { arity, .. } => *arity <= 22,
        IrExpr::Variable { ty, .. } => ty_ok(ty),
        IrExpr::Call { callee, .. } => callee_ok(callee),
        // A plugin placeholder that reached emit means its owning plugin didn't run (or couldn't
        // specialize it) — decline the file rather than miscompile (the node has no JVM lowering).
        IrExpr::PluginPlaceholder { .. } => false,
        _ => true,
    })
}

/// Emit the facade's top-level properties as `public static` fields plus a `<clinit>` that runs
/// their initializers in declaration order.
/// Convert the inliner's `VType` (a relocated frame verification type) to the class-writer's
/// `VerifType`. `Uninitialized` types shouldn't reach here (`splice_unified` bails on them).
/// A method's `StackMapTable` frames resolved to byte offsets: `(offset, locals, stack)` each.
type ResolvedFrames = Vec<(usize, Vec<VerifType>, Vec<VerifType>)>;

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

fn vtype_to_verif(v: &crate::jvm::inline::VType) -> VerifType {
    use crate::jvm::inline::VType;
    match v {
        VType::Top => VerifType::Top,
        VType::Int => VerifType::Integer,
        VType::Float => VerifType::Float,
        VType::Long => VerifType::Long,
        VType::Double => VerifType::Double,
        VType::Null => VerifType::Null,
        VType::Object(idx) => VerifType::Object(*idx),
        VType::UninitThis | VType::Uninit(_) => VerifType::Top,
    }
}

/// Expand a COLLAPSED frame-locals list (long/double = one entry) to SLOT-indexed (long/double = the
/// type + a trailing `Top` filler), so per-slot overlays line up.
fn expand_collapsed_locals(collapsed: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::with_capacity(collapsed.len());
    for v in collapsed {
        let wide = matches!(v, VerifType::Long | VerifType::Double);
        out.push(v.clone());
        if wide {
            out.push(VerifType::Top);
        }
    }
    out
}

/// Collapse a SLOT-indexed locals list back to the JVM `StackMapTable` form (long/double = one entry,
/// its second slot dropped).
fn collapse_locals(slots: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < slots.len() {
        let wide = matches!(slots[i], VerifType::Long | VerifType::Double);
        out.push(slots[i].clone());
        i += if wide { 2 } else { 1 };
    }
    out
}

/// The constant-pool index for a `const val`'s `ConstantValue` attribute when its initializer is a
/// compile-time literal; `None` otherwise (then the field is initialized in `<clinit>` as before).
fn const_value_idx(ir: &IrFile, init: crate::ir::ExprId, cw: &mut ClassWriter) -> Option<u16> {
    use crate::ir::{IrConst, IrExpr};
    match ir.expr(init) {
        IrExpr::Const(c) => Some(match c {
            IrConst::Boolean(b) => cw.const_int(*b as i32),
            IrConst::Byte(v) => cw.const_int(*v as i32),
            IrConst::Short(v) => cw.const_int(*v as i32),
            IrConst::Int(v) => cw.const_int(*v),
            IrConst::Char(c) => cw.const_int(*c as i32),
            IrConst::Long(v) => cw.const_long(*v),
            IrConst::Float(v) => cw.const_float(*v),
            IrConst::Double(v) => cw.const_double(*v),
            IrConst::String(s) => cw.const_string_kt(s),
            IrConst::Null => return None,
        }),
        _ => None,
    }
}

/// Whether `init` is a `ConstantValue`-eligible literal (mirrors [`const_value_idx`] without interning).
fn const_value_idx_peek(ir: &IrFile, init: crate::ir::ExprId) -> bool {
    matches!(ir.expr(init), crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null))
}

fn emit_statics(
    ir: &IrFile,
    facade: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
    param_assertions: bool,
) {
    // Statics OWNED by a specific class (a companion `const val`) are emitted on that class, not the
    // facade — see `emit_owned_consts`.
    let facade_statics: Vec<&crate::ir::IrStatic> =
        ir.statics.iter().filter(|s| s.is_facade_owned()).collect();
    if facade_statics.is_empty() {
        return;
    }
    for s in &facade_statics {
        // kotlinc: `const val` → `public static final`; a plain `val` → `private static final`; a `var`
        // → `private static` (mutated through the synthesized setter). The private field is read/written
        // directly only from within the facade; other classes go through the get/set accessors.
        let acc = if s.is_const {
            0x0019 // PUBLIC | STATIC | FINAL
        } else if s.is_var {
            0x000A // PRIVATE | STATIC
        } else {
            0x001A // PRIVATE | STATIC | FINAL
        };
        let desc = ir_type_desc(&s.ty);
        // A reference-typed facade static carries kotlinc's nullability annotation like any other
        // backing field.
        let field_ann = (desc.starts_with('L') || desc.starts_with('[')).then(|| {
            if s.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        // A `const val` initialized by a compile-time literal carries a `ConstantValue` attribute (the
        // JVM initializes the field; its `<clinit>` store is omitted below) — byte-identical to kotlinc.
        // LATE adds: kotlinc visits the facade's fields AFTER its methods, so a backing field's name
        // first interns at its accessor body and the const payload lands after the `<clinit>` window.
        let cv = (s.is_const && const_value_idx_peek(ir, s.init))
            .then(|| match ir.expr(s.init) {
                crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null) => {
                    Some(c.clone())
                }
                _ => None,
            })
            .flatten();
        cw.add_field_late(acc, &s.name, &desc, cv, field_ann);
    }
    // Which statics a CLASS body (a different JVM class than the facade) reads/writes — a PRIVATE
    // top-level property has no public accessors, so those references need kotlinc's `access$get<X>$p` /
    // `access$set<X>$p` bridges (emitted below, only when actually referenced).
    let mut cross_get: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut cross_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
    {
        let mut roots: Vec<u32> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                    roots.push(b);
                }
            }
            roots.extend(c.init_body);
            roots.extend(c.super_args.iter().copied());
            for sc in &c.secondary_ctors {
                roots.extend(sc.body);
                roots.extend(sc.delegate_prelude.iter().copied());
                roots.extend(sc.delegate_args.iter().copied());
            }
            for en in &c.enum_entries {
                roots.extend(en.args.iter().copied());
            }
        }
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            match &ir.exprs[cur as usize] {
                IrExpr::GetStatic(i) => {
                    cross_get.insert(*i);
                }
                IrExpr::SetStatic { index, .. } => {
                    cross_set.insert(*index);
                }
                _ => {}
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
    }
    // Accessors: a plain top-level `val`/`var` gets a `public static final getX()` (and `setX()` for a
    // `var`), so other classes read/write it the way kotlinc compiles cross-file property access. A
    // `const val` is `public static final` with no accessor (kotlinc inlines const reads). A PRIVATE
    // property gets NO public accessors — only the `access$…$p` bridges, and only when referenced.
    for (sidx, s) in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_facade_owned())
    {
        // A `const val` inlines (no accessor); a CUSTOM-accessor property emits its `getX`/`setX` as
        // ordinary facade methods (from `ir.functions`), so skip the trivial auto-accessor here.
        if s.is_const || s.custom_accessor {
            continue;
        }
        let jt = ir_ty_to_jvm(&s.ty);
        let desc = type_descriptor(jt);
        if s.visibility.is_private() {
            if cross_get.contains(&(sidx as u32)) {
                let mut g = CodeBuilder::new(0);
                let fref = cw.fieldref(facade, &s.name, &desc);
                g.getstatic(fref, slot_words(jt) as i32);
                emit_return(jt, &mut g);
                g.ensure_locals(0);
                g.link();
                cw.add_method(
                    0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
                    &format!("access${}$p", property_getter_name(&s.name)),
                    &format!("(){desc}"),
                    &g,
                );
            }
            if s.is_var && cross_set.contains(&(sidx as u32)) {
                let words = slot_words(jt);
                let mut st = CodeBuilder::new(words);
                load(jt, 0, &mut st);
                let fref = cw.fieldref(facade, &s.name, &desc);
                st.putstatic(fref, slot_words(jt) as i32);
                st.ret_void();
                st.ensure_locals(words);
                st.link();
                cw.add_method(
                    0x1019,
                    &format!("access${}$p", property_setter_name(&s.name)),
                    &format!("({desc})V"),
                    &st,
                );
            }
            continue;
        }
        // kotlinc visits the accessor's name, descriptor, and nullability annotation BEFORE its
        // body's field cluster; the accessor maps to the property's declaration line.
        let acc_ann = (jt.is_reference()).then(|| {
            if ir_ty_nullable(&s.ty) {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        let gname = property_getter_name(&s.name);
        cw.reserve_method_name(&gname);
        cw.seed_utf8(&format!("(){desc}"));
        if let Some(a) = acc_ann {
            cw.seed_utf8(a);
        }
        let mut g = CodeBuilder::new(0);
        if s.line != 0 {
            g.mark_line(s.line);
        }
        let fref = cw.fieldref(facade, &s.name, &desc);
        g.getstatic(fref, slot_words(jt) as i32);
        emit_return(jt, &mut g);
        finish_code::<0x0019>(cw, &gname, &format!("(){desc}"), &mut g, 0);
        cw.set_method_nullability(&gname, &format!("(){desc}"), acc_ann, &[None]);
        if s.is_var {
            let sname = property_setter_name(&s.name);
            cw.reserve_method_name(&sname);
            cw.seed_utf8(&format!("({desc})V"));
            let words = slot_words(jt);
            let mut st = CodeBuilder::new(words);
            // kotlinc guards a non-null reference setter parameter with checkNotNullParameter("<set-?>").
            // `-Xno-param-assertions` removes it, like every other parameter guard.
            if param_assertions && jt.is_reference() && !ir_ty_nullable(&s.ty) {
                st.aload(0);
                st.push_string("<set-?>", cw);
                let m = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                st.invokestatic(m, 2, 0);
            }
            // The store maps to the property line at the POST-GUARD pc (kotlinc's shape).
            if s.line != 0 {
                st.mark_line(s.line);
            }
            load(jt, 0, &mut st);
            let fref = cw.fieldref(facade, &s.name, &desc);
            st.putstatic(fref, slot_words(jt) as i32);
            st.ret_void();
            finish_code::<0x0019>(cw, &sname, &format!("({desc})V"), &mut st, words);
            cw.set_method_nullability(&sname, &format!("({desc})V"), None, &[acc_ann]);
            // The setter's value parameter is kotlinc's synthetic `<set-?>`, live for the body.
            cw.set_method_debug(
                &sname,
                &format!("({desc})V"),
                None,
                &[("<set-?>".to_string(), desc.clone(), 0)],
            );
        }
    }
    // A store the JVM already performs is pure redundancy: kotlinc emits no `<clinit>` store for a
    // `const val` folded into a `ConstantValue`, nor for an initializer that IS the field's default
    // (`val absent: String? = null`, `var count: Int = 0`) — the same elision instance fields get
    // from `elide_default_property_stores`.
    let should_store = |s: &crate::ir::IrStatic| {
        !(crate::jvm::property_storage::is_jvm_default(ir, s.init)
            || s.is_const && const_value_idx_peek(ir, s.init))
    };
    // kotlinc visits `<clinit>` (name + descriptor) before the initializer constants its body
    // interns. With nothing left to store there is NO `<clinit>` at all, so reserve only when a
    // store will be emitted.
    if !facade_statics.iter().any(|s| should_store(s)) {
        return;
    }
    cw.reserve_method_name("<clinit>");
    cw.seed_utf8("()V");
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        facade,
        facade,
        Ty::Unit,
        facade_statics.iter().map(|property| property.init),
    );
    let mut code = CodeBuilder::new(0);
    // Each store maps to its property's declaration line (kotlinc's `<clinit>` LineNumberTable).
    // `add_method` drops a `<clinit>`'s inline marks (they are curated), so collect + set after.
    let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
    for s in &facade_statics {
        if !should_store(s) {
            continue;
        }
        if s.line != 0 {
            clinit_lines.push((code.bytes.len() as u16, s.line));
        }
        e.emit_value(s.init, &mut code);
        let jt = ir_ty_to_jvm(&s.ty);
        let fref = e.cw.fieldref(facade, &s.name, &type_descriptor(jt));
        code.putstatic(fref, slot_words(jt) as i32);
    }
    code.ret_void();
    finish_code::<0x0008>(e.cw, "<clinit>", "()V", &mut code, e.next_slot);
    if !clinit_lines.is_empty() {
        e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
    }
}

/// The StackMapTable verification type of a JVM value type — the free-function twin of
/// `Emitter::verif_single`, for code synthesized outside an `Emitter`.
fn verif_of(ty: Ty) -> VerifType {
    match ty {
        t if is_jvm_int_category(t) => VerifType::Integer,
        Ty::Long => VerifType::Long,
        Ty::Double => VerifType::Double,
        Ty::Float => VerifType::Float,
        Ty::String => VerifType::ObjectName("java/lang/String".to_string()),
        t if t.is_array() => VerifType::ObjectName(type_descriptor(ty)),
        Ty::Obj(n, _) => {
            VerifType::ObjectName(crate::jvm::names::classfile_internal_name(&n.render()))
        }
        Ty::Null => VerifType::Null,
        _ => VerifType::Top,
    }
}

/// JVM type exposed by a synthesized property accessor. A property whose own type is a value class
/// uses its erased field carrier; an explicit backing field with a narrower type does not change the
/// property's public descriptor.
fn declared_property_accessor_jvm(
    ir: &IrFile,
    property: &crate::ir::IrProperty,
    field: &crate::ir::IrField,
) -> Ty {
    if property
        .ty
        .non_null()
        .obj_internal()
        .is_some_and(|name| ir.is_value_class_name(name))
    {
        ir_ty_to_jvm(&field.ty)
    } else {
        ir_ty_to_jvm(&stored_value_ty(property.ty))
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
        unbox_prim(cw, code, accessor_jvm);
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
            let internal = ref_internal(accessor_jvm);
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
        unbox_prim(cw, code, field_jvm);
    } else if accessor_jvm.is_jvm_scalar() && field_jvm.is_reference() {
        box_prim_free(cw, code, accessor_jvm);
    } else if accessor_jvm.is_reference() && field_jvm.is_reference() {
        let internal = ref_internal(field_jvm);
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
/// occupies. A same-named method with a different return descriptor does not hide the accessor.
fn emit_declared_property_accessor(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    property: &crate::ir::IrProperty,
    fq_name: &str,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    param_assertions: bool,
) {
    // A private property reached from outside (an `inline` body spliced into its caller) needs the
    // synthetic accessor kotlinc emits for exactly this: `access$get<X>$p(<owner>)<ty>`.
    if property.needs_access_bridge {
        if let Some(field) = property
            .backing_field
            .and_then(|i| c.fields.get(i as usize))
        {
            let field_jt = ir_ty_to_jvm(&field.ty);
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
                    let d = method_descriptor(&[], ir_ty_to_jvm(&f.ret));
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
                        let d = method_descriptor(&[ir_ty_to_jvm(&f.params[0])], Ty::Unit);
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
    if property.is_private || property.getter.is_some() {
        return;
    }
    let Some(field_index) = property.backing_field else {
        return;
    };
    let Some(field) = c.fields.get(field_index as usize) else {
        return;
    };
    let field_jt = ir_ty_to_jvm(&field.ty);
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
            function.name == name
                && method_descriptor(&function.params, ir_ty_to_jvm(&function.ret)) == descriptor
        })
    };
    let getter_desc = format!("(){accessor_desc}");
    if !occupied(&getter, &getter_desc) {
        // Visit the method header before constructing its code. This is especially observable for a
        // setter guard (`<set-?>`) and for a generic accessor Signature.
        let sig = ir
            .field_signatures(fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| *fname == field.name))
            .map(|(_, tp)| format!("()T{tp};"))
            .or_else(|| field.type_param.as_ref().map(|tp| format!("()T{tp};")))
            .or_else(|| method_parameterized_sig(formatter, &[], &field.ty));
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
            g.add_frame_if_new(
                lbl,
                vec![VerifType::ObjectName(fq_name.to_string())],
                vec![verif_of(field_jt)],
            );
            g.bind(lbl);
        }
        emit_backing_field_read_adaptation(ir, cw, &mut g, property, field_jt, accessor_jt);
        emit_return(accessor_jt, &mut g);
        g.ensure_locals(1);
        g.link();
        let access = if overridable { 0x0001 } else { 0x0011 };
        cw.add_method_sig(access, &getter, &getter_desc, &g, sig.as_deref());
    }
    if property.is_var {
        let setter = property
            .setter_jvm_name
            .clone()
            .unwrap_or_else(|| crate::names::property_setter_name(&property.name));
        let setter_desc = format!("({accessor_desc})V");
        if !occupied(&setter, &setter_desc) {
            let sig = ir
                .field_signatures(fq_name)
                .and_then(|fs| fs.iter().find(|(fname, _)| *fname == field.name))
                .map(|(_, tp)| format!("(T{tp};)V"))
                .or_else(|| field.type_param.as_ref().map(|tp| format!("(T{tp};)V")))
                .or_else(|| {
                    method_parameterized_sig(formatter, std::slice::from_ref(&field.ty), &Ty::Unit)
                });
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
            // cannot be null, and a bare type parameter's bound is `Any?`, so neither is guarded.
            let guarded = param_assertions
                && accessor_jt.is_reference()
                && !property.ty.is_nullable()
                && !is_type_parameter_field(ir, fq_name, &field.name);
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

fn emit_declared_property_accessors(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fq_name: &str,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    param_assertions: bool,
) {
    for property in &c.properties {
        emit_declared_property_accessor(ir, c, property, fq_name, cw, formatter, param_assertions);
    }
}

/// The checker/lowerer-bound callable that encloses an anonymous class, with its JVM owner. The
/// callable identity is exact; only the final owner rendering is a backend boundary conversion.
fn anonymous_scope<'a>(
    ir: &'a IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
) -> Option<(String, &'a IrFunction)> {
    if !c.is_anonymous_object {
        return None;
    }
    let function = &ir.functions[c.enclosing_function? as usize];
    let owner = function
        .dispatch_receiver
        .map(TypeName::render)
        .unwrap_or_else(|| facade.to_string());
    Some((owner, function))
}

/// One DECLARED secondary constructor's metadata shape: its source parameter names/types, its JVM
/// descriptor, and the position of a `vararg` parameter.
struct SecondaryCtorShape {
    params: Vec<(String, Ty)>,
    desc: String,
    vararg_index: Option<usize>,
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
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade, env, opts);
    }
    if let Some(iface) = &c.annotation_impl_of {
        return emit_annotation_impl_class(ir, c, &iface.render(), facade, env, opts);
    }
    if c.is_annotation {
        return emit_annotation_class(ir, c, opts, class_meta);
    }
    if c.is_interface {
        return emit_interface_class(ir, c, facade, env, opts, class_meta, extra);
    }
    if let Some(user_tys) = &c.enum_entry_of {
        return emit_enum_entry_subclass(ir, c, facade, env, opts, user_tys);
    }
    if c.prop_ref.is_some() {
        return emit_prop_ref_class(c, facade, opts);
    }
    if c.func_ref.is_some() {
        return emit_func_ref_class(ir, c, facade, opts);
    }
    let fq_name = c.fq_name();
    let superclass = c.superclass();
    let signature_formatter = JvmSignatureFormatter::new(env);
    let mut cw = new_classifier_writer(ir, c, &superclass, env, opts);
    // A LOCAL class needs an `EnclosingMethod` attribute: without it reflection reads the class as
    // top-level and `simpleName` reports the whole `owner$Local` name instead of `Local`. The
    // enclosing class is the longest `$`-prefix of the name that is itself an emitted class (a local
    // class inside a member) — otherwise the file facade, which is where a top-level function lives.
    if c.is_local_class && !c.is_anonymous_object {
        let owner = fq_name
            .match_indices('$')
            .map(|(at, _)| &fq_name[..at])
            .rfind(|candidate| ir.classes.iter().any(|other| other.fq_name() == *candidate))
            .map(str::to_string)
            .or_else(|| (!facade.is_empty()).then(|| facade.to_string()));
        if let Some(owner) = owner {
            cw.set_enclosing_class(&owner);
        }
    }
    let continuation_metadata = env.continuation_metadata.get(&fq_name);
    if let Some(metadata) = continuation_metadata {
        cw.set_enclosing_method(
            &metadata.enclosing_class,
            &metadata.enclosing_method,
            &metadata.enclosing_descriptor,
        );
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
    register_sealed_subtypes(
        &mut cw,
        ir,
        c,
        opts.class_major.unwrap_or(MAJOR_JAVA8) >= 61,
    );
    // An ANONYMOUS class carries kotlinc's enclosure record: the `EnclosingMethod` attribute
    // (owner + the exact enclosing method's name/descriptor) and an INNER-ONLY `InnerClasses` entry
    // (`outer_class_info_index = 0`, no simple name — the JVM's anonymous-class shape). Registered
    // BEFORE the file-nest registration below: the first registration for a class wins, and the
    // nest derives an outer+name shape kotlinc doesn't give anonymous classes.
    if let Some((owner, function)) = anonymous_scope(ir, c, facade) {
        let descriptor = ir_method_desc(&function.params, &function.ret);
        cw.set_enclosing_method(&owner, &function.name, &descriptor);
    }
    register_inner_classes(&mut cw, ir);
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
    if byte_parity {
        seed_plain_class_pool(
            &signature_formatter,
            ir,
            c,
            &fq_name,
            &superclass,
            ctor_signature.as_deref(),
            &mut cw,
        );
    }
    // Access: an extended or abstract class must not be `final`; a class with an abstract method
    // (body `None`) is `ACC_ABSTRACT`.
    let extended = ir.classes.iter().any(|o| o.superclass_matches(&fq_name));
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    // A synthesized continuation class is package-private in kotlinc.
    let mut access = if is_continuation {
        0x0020 // SUPER (package-private)
    } else {
        0x0001 | 0x0020 // PUBLIC | SUPER
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
    if ir.is_synthetic_class(&fq_name) {
        access |= 0x1000;
    } // ACC_SYNTHETIC (a `$$serializer` object)
    cw.set_access(access);
    if ir.is_deprecated_class(&fq_name) {
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
    let mut field_order: Vec<&crate::ir::IrField> = if static_storage(ir, c) {
        Vec::new()
    } else {
        c.fields.iter().collect()
    };
    if is_continuation {
        field_order.sort_by_key(|field| match field.name.as_str() {
            "result" => 1,
            "this$0" => 2,
            "label" => 3,
            _ => 0,
        });
    }
    for field in field_order {
        let name = &field.name;
        let ty = &field.ty;
        // Map the field's (platform-neutral) visibility to JVM access flags: a `private` field →
        // `ACC_PRIVATE` (the default — Kotlin backing fields are private, reached via accessors); a
        // non-private field → `ACC_PUBLIC` (read/written cross-class, e.g. a coroutine continuation's
        // `result`/`label`).
        let private = field.is_private();
        let acc = if is_continuation {
            // kotlinc's continuation field layout: everything package-private; `result` is SYNTHETIC,
            // the captured receiver `this$0` is FINAL|SYNTHETIC; `label` and the `L$N` spills are plain.
            match name.as_str() {
                "result" => 0x1000,
                "this$0" => 0x1010,
                _ => 0x0000,
            }
        } else {
            (if private { 0x0002 } else { 0x0001 })
                | if field.is_final() { 0x0010 } else { 0 }
                | if static_storage(ir, c) { 0x0008 } else { 0 }
        };
        // A field typed by a bare type parameter (`val a: A`) carries a `Signature` (`TA;`); a
        // PARAMETERIZED concrete type (`val xs: List<String>`) carries its full generic signature. Both
        // like kotlinc; disjoint (a field is one or the other).
        let field_sig = ir
            .field_signatures(&fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| fname == name))
            .map(|(_, tp)| format!("T{tp};"))
            .or_else(|| parameterized_sig(&signature_formatter, ty));
        let physical_name = instance_field_jvm_name(ir, c, field);
        let field_desc = ir_type_desc(ty);
        // Data-class field headers are part of the synthesized-member seed order, while coroutine
        // continuation fields are compiler-generated storage rather than Kotlin properties. Both are
        // therefore visited eagerly and neither receives property nullability annotations here.
        // Ordinary declared properties use the later field-table visit: their methods establish the
        // preceding pool window and their backing fields carry Kotlin's nullability annotation.
        if c.is_data || is_continuation {
            cw.add_field_sig(acc, &physical_name, &field_desc, field_sig.as_deref());
        } else {
            let field_ann = ((field_desc.starts_with('L') || field_desc.starts_with('['))
                && field.type_param.is_none())
            .then(|| {
                if ty.is_nullable() {
                    "Lorg/jetbrains/annotations/Nullable;"
                } else {
                    "Lorg/jetbrains/annotations/NotNull;"
                }
            });
            cw.add_field_late_sig(
                acc,
                &physical_name,
                &field_desc,
                field_sig.as_deref(),
                None,
                field_ann,
            );
        }
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
        // visibility (kotlinc: every access goes through the accessors/bridges, never the field).
        let hoisted = ir.is_jvm_companion_hoisted_static(static_index);
        let acc = if s.visibility.is_private() || hoisted {
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
        // A reference-typed static carries kotlinc's nullability annotation (private hoisted
        // fields included).
        let ann = (desc.starts_with('L') || desc.starts_with('[')).then(|| {
            if s.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        cw.add_field_late(acc, &s.name, &desc, cv, ann);
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
    let param_tys = class_ctor_jvm_tys(c);
    // A class with NO primary constructor emits no primary `<init>` — every `<init>` comes from a
    // secondary constructor (below). Otherwise emit the primary `<init>` here.
    if c.has_primary_ctor {
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
                c.super_args.iter().copied().chain(c.init_body),
            );
            e.next_slot = 1 + params_words;
            e.this_uninitialized = true;
            e.slots.insert(0, (0, Ty::obj(&fq_name)));
            let mut s = 1u16;
            for (vi, t) in param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // kotlinc guards each non-null reference constructor parameter with checkNotNullParameter at
            // the very start of `<init>` — before the super() call.
            for (i, a) in c.ctor_args.iter().enumerate() {
                if let Some(name) = &a.check {
                    if let Some(&(slot, _)) = e.slots.get(&(i as u32 + 1)) {
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
            let ctor_param_is_field: Vec<bool> = if c.ctor_args.is_empty() {
                vec![true; param_tys.len()]
            } else {
                c.ctor_args
                    .iter()
                    .map(|argument| argument.is_field)
                    .collect()
            };
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
            // `super(args)` — `this` is loaded first, so spill any branchy arg to temps before it.
            let super_args = c.super_args.clone();
            if super_args.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&super_args, &mut ctor);
                ctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut ctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                ctor.aload(0);
                for &a in &super_args {
                    e.emit_value(a, &mut ctor);
                }
            }
            // A base whose primary ctor takes a value-class param — or a SEALED base — has a PRIVATE
            // primary; a subclass `super(…)` must reach it through the PUBLIC|SYNTHETIC
            // `(…args, DefaultConstructorMarker)` accessor (a trailing `null` marker), never the
            // inaccessible private primary.
            let (super_param_tys, super_accessor) =
                super_ctor_jvm_tys(e.ir, c, &superclass, |arg| e.value_ty(arg));
            if super_accessor {
                ctor.aconst_null();
            }
            let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let super_init = e.cw.methodref(
                &superclass,
                "<init>",
                &method_descriptor(&super_param_tys, Ty::Unit),
            );
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
                let mut field_i = 0usize;
                for (i, t) in param_tys.iter().enumerate() {
                    if ctor_param_is_field.get(i).copied().unwrap_or(true) {
                        let name = &c.fields[field_i].name;
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
                            if let Some(&pl) =
                                ir.prop_decl_lines.get(&(c.fq_name_id(), name.clone()))
                            {
                                if pl != 0 {
                                    ctor_lines.push((pc, pl));
                                }
                            }
                            ctor.aload(0);
                            load(*t, slot, &mut ctor);
                            let physical_name = instance_field_jvm_name(ir, c, &c.fields[field_i]);
                            let fref =
                                e.cw.fieldref(&fq_name, &physical_name, &type_descriptor(*t));
                            ctor.putfield(fref, slot_words(*t) as i32);
                        }
                        field_i += 1;
                    }
                    slot += slot_words(*t);
                }
            }
            if let Some(init_body) = c.init_body.filter(|_| !static_storage(ir, c)) {
                // kotlinc gives the ctor one LineNumberTable entry per body-property initializer, on
                // that property's own source line. Emit the initializer statements one at a time so
                // each one's real start pc is known; only for a pure list of `SetField` stores (the
                // desugared `val y: Int = 2` shape) — anything else keeps the whole-block emit.
                let stmts: Option<Vec<crate::ir::ExprId>> = match ir.expr(init_body) {
                    crate::ir::IrExpr::Block { stmts, value } if value.is_none() => stmts
                        .iter()
                        .all(|&st| matches!(ir.expr(st), crate::ir::IrExpr::SetField { .. }))
                        .then(|| stmts.clone()),
                    _ => None,
                };
                match stmts {
                    Some(stmts) => {
                        for st in stmts {
                            let pc = ctor.bytes.len() as u16;
                            if let crate::ir::IrExpr::SetField { index, .. } = ir.expr(st) {
                                let name = &c.fields[*index as usize].name;
                                if let Some(&pl) =
                                    ir.prop_decl_lines.get(&(c.fq_name_id(), name.clone()))
                                {
                                    if pl != 0 {
                                        ctor_lines.push((pc, pl));
                                    }
                                }
                            }
                            e.emit(st, &mut ctor);
                        }
                    }
                    None => e.emit(init_body, &mut ctor),
                }
                init_diverges = e.diverges(init_body);
            }
            max_slot = e.next_slot;
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
        let ctor_access = if c.is_singleton() || c.is_value || value_param_ctor || c.is_sealed {
            0x0002
        } else if is_continuation || c.is_anonymous_object {
            // A continuation class's ctor is package-private (constructed only by its own file);
            // kotlinc gives an ANONYMOUS class's ctor the same access (flags 0x0000).
            0x0000
        } else {
            // A DECLARED protected constructor reaches the JVM method too (kotlinc emits `<init>`
            // protected). A declared PRIVATE ctor stays JVM-public for now: a companion factory
            // calls it cross-class, which kotlinc routes through the `DefaultConstructorMarker`
            // accessor — until krusty models that route, ACC_PRIVATE would be an
            // IllegalAccessError; `@Metadata` still records the declared privacy.
            match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => 0x0004,
                _ => 0x0001,
            }
        };
        cw.add_method_sig(
            ctor_access,
            "<init>",
            &method_descriptor(&param_tys, Ty::Unit),
            &ctor,
            ctor_signature.as_deref(),
        );
        // A default on any primary-ctor parameter → kotlinc's synthetic
        // `<init>(params…, int mask, DefaultConstructorMarker)` overload (fills the masked slots from the
        // defaults, then `invokespecial` the real `<init>`).
        if let Some(defaults) = ir.class_ctor_defaults(&fq_name) {
            // (The stub emits its own LineNumberTable — class line, per-fill parameter lines, the
            // ctor's closing-`)` line at `return` — collapsed for single-line declarations.)
            emit_ctor_default_stub(ir, &fq_name, facade, &param_tys, defaults, &mut cw, env);
        }
        // A COMPANION's synthetic marker ctor is emitted LAST, after the instance accessors
        // (kotlinc's member order — see below); every other shape keeps it beside the primary.
        if has_ctor_marker_accessor(ir, c) && !c.is_companion {
            emit_ctor_marker_accessor(&fq_name, &param_tys, &mut cw);
        }
    } // end `if c.has_primary_ctor`

    // Secondary constructors: each `<init>(p)` delegates (via `this(…)` to an own `<init>`, or via
    // `super(…)` to the base `<init>`) then runs its body. A `super(…)`-reaching ctor's `body` already
    // has the class init steps prepended (the lowering does that). `this` is slot 0, parameters follow.
    for sc in &c.secondary_ctors {
        let sc_param_tys = jvm_tys(&sc.params);
        // Reserve this constructor's header — its declared annotations included — before its body
        // interns anything, matching the order kotlinc's writer produces.
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &method_descriptor(&sc_param_tys, Ty::Unit),
            None,
            &[],
            &sc.annotations.visible,
            &sc.annotations.invisible,
        );
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        {
            let mut e = Emitter::new(
                ir,
                &mut cw,
                env,
                &fq_name,
                facade,
                Ty::Unit,
                sc.delegate_prelude
                    .iter()
                    .chain(&sc.delegate_args)
                    .copied()
                    .chain(sc.body),
            );
            e.next_slot = 1 + sc_words;
            e.this_uninitialized = true;
            e.slots.insert(0, (0, Ty::obj(&fq_name)));
            let mut s = 1u16;
            for (vi, t) in sc_param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // The checker selected the exact delegation descriptor; lowering only materialized operands.
            use crate::ir::CtorDelegateTarget;
            let (target_class, mut target_jvm_tys, default_masks): (String, Vec<Ty>, &[i32]) =
                match &sc.delegate {
                    CtorDelegateTarget::This {
                        target_params,
                        default_masks,
                        ..
                    } => (fq_name.clone(), jvm_tys(target_params), default_masks),
                    CtorDelegateTarget::Super {
                        owner,
                        target_params,
                        default_masks,
                    } => {
                        let owner =
                            crate::jvm::jvm_class_map::to_jvm_internal(&owner.render()).to_string();
                        (owner, jvm_tys(target_params), default_masks)
                    }
                };
            for &statement in &sc.delegate_prelude {
                e.emit(statement, &mut sctor);
            }
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut sctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                sctor.aload(0);
                for &a in &dargs {
                    e.emit_value(a, &mut sctor);
                }
            }
            if !default_masks.is_empty() {
                for &mask in default_masks {
                    sctor.push_int(mask, e.cw);
                    target_jvm_tys.push(Ty::Int);
                }
                sctor.aconst_null();
                target_jvm_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            // A cross-class delegation target (`super(…)` to a base) whose primary ctor takes a value-class
            // param has a PRIVATE primary — reach it through the `(…args, DefaultConstructorMarker)`
            // accessor. A same-class `this(…)` to the own private primary stays direct (accessible).
            let target_sealed = target_class != fq_name
                && e.ir
                    .classes
                    .iter()
                    .any(|o| o.fq_name_matches(&target_class) && o.is_sealed);
            if default_masks.is_empty()
                && ((target_class != fq_name && e.ir.has_value_param_ctor(&target_class))
                    || target_sealed)
            {
                sctor.aconst_null();
                target_jvm_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            let aw: i32 = target_jvm_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let delegate_init = e.cw.methodref(
                &target_class,
                "<init>",
                &method_descriptor(&target_jvm_tys, Ty::Unit),
            );
            sctor.invokespecial(delegate_init, aw, 0);
            e.this_uninitialized = false;
            if let Some(body) = sc.body {
                e.emit(body, &mut sctor);
                sec_diverges = e.diverges(body);
            }
            sec_max = e.next_slot;
        }
        if !sec_diverges {
            sctor.ret_void();
        }
        sctor.ensure_locals(sec_max);
        sctor.link();
        // A SEALED class's secondary ctor is private too, with its own PUBLIC
        // `(…args, DefaultConstructorMarker)` accessor (kotlinc: EVERY sealed ctor pairs with one).
        // A VALUE-CLASS-parametered secondary ctor gets the same private+marker ABI (kotlinc's).
        let sc_access = (if c.is_sealed || sc.vc_params {
            0x0002
        } else {
            0x0001
        }) | if sc.synthetic { 0x1000 } else { 0 };
        let sc_desc = method_descriptor(&sc_param_tys, Ty::Unit);
        cw.add_method(sc_access, "<init>", &sc_desc, &sctor);
        // Declared constructor annotations, with the same `Deprecated` / `ACC_SYNTHETIC` companions
        // a function's carry (see the method emitter).
        if !sc.annotations.visible.is_empty() || !sc.annotations.invisible.is_empty() {
            cw.set_method_annotations(
                "<init>",
                &sc_desc,
                &sc.annotations.visible,
                &sc.annotations.invisible,
            );
            if sc.annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &sc_desc);
            }
            if sc.annotations.deprecated_hidden() {
                cw.set_method_synthetic("<init>", &sc_desc);
            }
        }
        if sc.defaults.iter().any(Option::is_some) {
            emit_ctor_default_stub(
                ir,
                &fq_name,
                facade,
                &sc_param_tys,
                &sc.defaults,
                &mut cw,
                env,
            );
        }
        if c.is_sealed || sc.vc_params {
            emit_ctor_marker_accessor(&fq_name, &sc_param_tys, &mut cw);
        }
    }
    // JVM method order follows Kotlin declaration order. A plain property's accessors do not have
    // `FunId`s, so interleave the declaration itself with source-written functions/accessors rather
    // than grouping backend-synthesized accessors ahead of every function.
    enum DeclaredMember<'a> {
        Property(&'a crate::ir::IrProperty),
        Function(u32),
    }
    let mut ordered = Vec::with_capacity(c.properties.len() + c.methods.len());
    ordered.extend(c.properties.iter().map(DeclaredMember::Property));
    ordered.extend(c.methods.iter().copied().map(DeclaredMember::Function));
    ordered.sort_by_key(|member| match member {
        DeclaredMember::Property(property) => property.source_order,
        DeclaredMember::Function(fid) => ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX),
    });
    for member in ordered {
        let fid = match member {
            DeclaredMember::Property(property) => {
                emit_declared_property_accessor(
                    ir,
                    c,
                    property,
                    &fq_name,
                    &mut cw,
                    &signature_formatter,
                    opts.param_assertions,
                );
                continue;
            }
            DeclaredMember::Function(fid) => fid,
        };
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // A `static` member (e.g. a value class's `box-impl`/`constructor-impl`) emits with no
            // `this` slot; an ordinary member is an instance method.
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
        } else {
            cw.add_abstract_method_sig(
                0x0001 | 0x0400,
                &f.name,
                &ir_method_desc(&f.params, &f.ret),
                method_signature(&signature_formatter, ir, fid, f).as_deref(),
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
                    &fq_name,
                    &mut cw,
                    defaults,
                    env,
                    Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"),
                );
            } else {
                emit_default_stub(ir, fid, &fq_name, facade, &mut cw, defaults, env, false);
            }
        }
    }
    // EVERY parameter defaulted → kotlinc also emits the no-arg convenience `<init>()`
    // (`AuditFilters()` in Java/reflection), delegating to the `$default` overload with a full
    // mask — AFTER the declared methods (kotlinc's member order), at the primary's declared
    // visibility (a PROTECTED primary gets a protected convenience ctor).
    if c.has_primary_ctor {
        let param_tys = class_ctor_jvm_tys(c);
        let value_param_ctor = ir.has_value_param_ctor(&fq_name);
        let ctor_access = if c.is_singleton() || c.is_value || value_param_ctor || c.is_sealed {
            0x0002
        } else if is_continuation || c.is_anonymous_object {
            0x0000
        } else {
            match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => 0x0004,
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
            }
        }
    }
    // A companion's synthetic `(…, DefaultConstructorMarker)` ctor goes AFTER the accessors —
    // kotlinc's companion member order (private <init>, accessors, marker <init>).
    if c.has_primary_ctor && c.is_companion && has_ctor_marker_accessor(ir, c) {
        emit_ctor_marker_accessor(&fq_name, &class_ctor_jvm_tys(c), &mut cw);
    }
    emit_bridges(c, &mut cw);
    // HOISTED companion properties: the private static field lives on THIS class, so the companion's
    // delegating accessors reach it through PUBLIC synthetic `access$get<X>$cp`/`access$set<X>$cp`
    // bridges — emitted AFTER the instance methods, right before `<clinit>` (kotlinc's order).
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| {
            ir.is_jvm_companion_hoisted_static(*index as u32) && s.owner_matches(&fq_name)
        })
        .map(|(_, s)| s)
    {
        let jt = ir_ty_to_jvm(&s.ty);
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
                s.1.owner_matches(&fq_name) && !(s.1.is_const && const_value_idx_peek(ir, s.1.init))
            })
            .map(|(index, s)| (index as u32, s))
            .collect();
        if c.companion_class.is_some() || !clinit_statics.is_empty() {
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
                if ir.is_jvm_companion_hoisted_static(*static_index) {
                    if let Some(&line) = c
                        .companion_class
                        .as_ref()
                        .and_then(|companion| ir.prop_decl_lines.get(&(*companion, s.name.clone())))
                    {
                        if line != 0 {
                            clinit_lines.push((pc, line));
                        }
                    }
                }
                e.emit_value(s.init, &mut clinit);
                let jt = ir_ty_to_jvm(&s.ty);
                let fref = e.cw.fieldref(&fq_name, &s.name, &type_descriptor(jt));
                clinit.putstatic(fref, slot_words(jt) as i32);
            }
            clinit.ret_void();
            clinit.ensure_locals(e.next_slot);
            clinit.link();
            e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
            if byte_parity && !clinit_lines.is_empty() {
                e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
            }
        }
    }
    // A singleton `object` (emitted AFTER the instance methods — kotlinc's method order, which
    // also matches its constant-pool interning sequence): a `public static final INSTANCE` built in `<clinit>`.
    if c.is_object || companion_of_interface(ir, c) {
        // An INTERFACE's companion self-hosts its singleton: a package-private `static final
        // $$INSTANCE` on the companion (the interface's `Companion` field aliases it in the
        // interface `<clinit>`), with the companion's properties as statics here — kotlinc's
        // interface-companion layout. A plain object uses `public static final INSTANCE`.
        let interface_companion = !c.is_object;
        let (instance_name, instance_access) = if interface_companion {
            ("$$INSTANCE", 0x1018) // STATIC | FINAL | SYNTHETIC (package-private)
        } else {
            ("INSTANCE", 0x0019) // PUBLIC | STATIC | FINAL
        };
        let self_desc = format!("L{};", fq_name);
        // kotlinc reaches `<clinit>` before the INSTANCE field, so the pool follows its BODY: the
        // method name, the `<init>` Methodref of `new demo/O`, then the field entries at the
        // `putstatic`, and only then the field's `@NotNull`.
        cw.reserve_method_name("<clinit>");
        let ci = cw.class_ref(&fq_name);
        let init = cw.methodref(&fq_name, "<init>", "()V");
        let fref = cw.fieldref(&fq_name, instance_name, &self_desc);
        cw.add_field(instance_access, instance_name, &self_desc);
        if !interface_companion {
            cw.set_field_nullability("INSTANCE", "Lorg/jetbrains/annotations/NotNull;");
        }
        // The backing fields FOLLOW the INSTANCE entry in the field table (kotlinc's order); their
        // Utf8s were interned by the accessor bodies, so the pool is undisturbed.
        if static_storage(ir, c) {
            for field in &c.fields {
                let private = field.is_private();
                let acc = (if private { 0x0002 } else { 0x0001 })
                    | if field.is_final() { 0x0010 } else { 0 }
                    | 0x0008;
                let field_sig = ir
                    .field_signatures(&fq_name)
                    .and_then(|fs| fs.iter().find(|(fname, _)| *fname == field.name))
                    .map(|(_, tp)| format!("T{tp};"))
                    .or_else(|| parameterized_sig(&signature_formatter, &field.ty));
                let physical_name = instance_field_jvm_name(ir, c, field);
                cw.add_field_sig(
                    acc,
                    &physical_name,
                    &ir_type_desc(&field.ty),
                    field_sig.as_deref(),
                );
            }
        }
        let mut clinit = CodeBuilder::new(0);
        clinit.new_obj(ci);
        clinit.dup();
        clinit.invokespecial(init, 0, 0);
        clinit.putstatic(fref, 1);
        // A static-storage object runs its property initializers + `init {}` blocks HERE, after the
        // INSTANCE store (kotlinc's shape — `<init>` is a bare super() call). `this` references in
        // initializer expressions read the just-stored INSTANCE; a pure list of own-field stores
        // (the common shape) needs no local at all, matching kotlinc's `ldc; putstatic` sequence.
        let mut clinit_max = 0u16;
        let mut clinit_line_entries: Vec<(u16, u32)> = Vec::new();
        if let Some(init_body) = c.init_body.filter(|_| static_storage(ir, c)) {
            let mut e = Emitter::new(
                ir,
                &mut cw,
                env,
                &fq_name,
                facade,
                Ty::Unit,
                std::iter::once(init_body),
            );
            if init_body_reads_this(ir, init_body) {
                let iref = e.cw.fieldref(&fq_name, instance_name, &self_desc);
                clinit.getstatic(iref, 1);
                store(Ty::obj(&fq_name), 0, &mut clinit);
                e.slots.insert(0, (0, Ty::obj(&fq_name)));
                e.next_slot = 1;
            }
            // Per-initializer line numbers (kotlinc maps each store to its property's source line;
            // applied after add_method below) — only for the pure store-list shape.
            let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
            let stmts: Option<Vec<crate::ir::ExprId>> = match ir.expr(init_body) {
                crate::ir::IrExpr::Block { stmts, value } if value.is_none() => stmts
                    .iter()
                    .all(|&st| matches!(ir.expr(st), crate::ir::IrExpr::SetField { .. }))
                    .then(|| stmts.clone()),
                _ => None,
            };
            match stmts {
                Some(stmts) => {
                    for st in stmts {
                        let pc = clinit.bytes.len() as u16;
                        if let crate::ir::IrExpr::SetField { index, .. } = ir.expr(st) {
                            let name = &c.fields[*index as usize].name;
                            if let Some(&pl) =
                                ir.prop_decl_lines.get(&(c.fq_name_id(), name.clone()))
                            {
                                if pl != 0 {
                                    clinit_lines.push((pc, pl));
                                }
                            }
                        }
                        e.emit(st, &mut clinit);
                    }
                }
                None => e.emit(init_body, &mut clinit),
            }
            clinit_max = e.next_slot;
            clinit_lines.dedup_by_key(|(_, l)| *l);
            clinit_line_entries = clinit_lines;
        }
        clinit.ret_void();
        clinit.ensure_locals(clinit_max);
        clinit.link();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        if !clinit_line_entries.is_empty() {
            cw.set_method_lines("<clinit>", "()V", &clinit_line_entries);
        }
    }
    cw.set_runtime_annotations(&c.applied_annotations);
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
        attach_synth_debug_tables(ir, c, &mut cw, opts.param_assertions, &ctor_lines);
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(ir, c, &mut cw);
    }
    if let Some(metadata) = continuation_metadata {
        let self_desc = format!("L{fq_name};");
        let has_this0 = c.fields.iter().any(|f| f.name == "this$0");
        let mut ctor_locals: Vec<(String, String, u16)> =
            vec![("this".to_string(), self_desc.clone(), 0)];
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
        cw.set_method_debug(
            "invokeSuspend",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            None,
            &[
                ("this".to_string(), self_desc, 0),
                ("$result".to_string(), "Ljava/lang/Object;".to_string(), 1),
            ],
        );
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    // Seed the nested OUTER class ref + simple name at kotlinc's post-metadata pool position
    // (its `InnerClasses` visit interns the outer's Class entry, then the simple name). An
    // ANONYMOUS class has neither — its entry is inner-only and its refs seed via the
    // `EnclosingMethod` window in `finish`.
    if !is_coroutine_state_machine(c) && !c.is_anonymous_object {
        if let Some(pos) = fq_name.rfind('$') {
            cw.seed_class(&fq_name[..pos]);
            cw.seed_utf8(&fq_name[pos + 1..]);
        }
    }
    cw.finish()
}

/// Emit a synthesized enum-entry subclass (`Enum$ENTRY extends Enum`) for an entry with a body: a
/// package-private `final` class with one constructor `(String name, int ordinal, <user fields>)V`
/// that delegates to the enum's `(String,int,<user>)V` constructor, plus the entry's overriding
/// methods. It has no fields of its own — overrides read the enum's fields via the inherited `this`.
fn emit_enum_entry_subclass(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    user_tys: &[Ty],
) -> Vec<u8> {
    let superclass = c.superclass();
    let fq_name = c.fq_name();
    let signature_formatter = JvmSignatureFormatter::new(env);
    let mut cw = new_writer(&fq_name, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)

    // Entry-body PROPERTIES are private backing fields (read via synthesized getters, like kotlinc).
    for field in c.fields.iter() {
        let acc = 0x0002 | if field.is_final() { 0x0010 } else { 0 };
        cw.add_field(acc, &field.name, &ir_type_desc(&field.ty));
    }

    // Constructor: `(String, int, <user>)V` → `super(name, ordinal, <user>)`, then the property
    // initializers (`this.<prop> = <init>`, from `init_body`).
    let user_jvm = jvm_tys(user_tys);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(user_jvm.iter().copied())
        .collect();
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    let mut slot = 1u16;
    for t in &ctor_params {
        load(*t, slot, &mut ctor);
        slot += slot_words(*t);
    }
    let super_init = cw.methodref(
        &superclass,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
    );
    let argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    ctor.invokespecial(super_init, argw, 0);
    let mut ctor_max = 1 + ctor_words;
    if let Some(init_body) = c.init_body {
        let mut e = Emitter::new(ir, &mut cw, env, &fq_name, facade, Ty::Unit, [init_body]);
        e.next_slot = 1 + ctor_words;
        e.slots.insert(0, (0, Ty::obj(&fq_name))); // `this`
        e.emit(init_body, &mut ctor);
        ctor_max = e.next_slot;
    }
    ctor.ret_void();
    ctor.ensure_locals(ctor_max);
    ctor.link();
    cw.add_method(
        0x0000,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
        &ctor,
    );

    // The overriding methods + synthesized property getters.
    emit_declared_property_accessors(
        ir,
        c,
        &fq_name,
        &mut cw,
        &signature_formatter,
        opts.param_assertions,
    );
    for &fid in &c.methods {
        emit_method(ir, fid, &fq_name, facade, &mut cw, true, env);
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

fn property_getter_descriptor(pr: &crate::ir::PropRef, owner: &str, ext: bool) -> String {
    pr.getter_descriptor.clone().unwrap_or_else(|| {
        let ret = type_descriptor(ir_ty_to_jvm(&pr.prop_ty));
        if ext {
            format!("({}){ret}", type_descriptor(Ty::obj(owner)))
        } else {
            format!("(){ret}")
        }
    })
}

fn property_setter_target(pr: &crate::ir::PropRef, owner: &str, ext: bool) -> (String, String) {
    let name = pr
        .setter_name
        .clone()
        .unwrap_or_else(|| property_setter_name(&pr.prop_name));
    let descriptor = pr.setter_descriptor.clone().unwrap_or_else(|| {
        let value = type_descriptor(ir_ty_to_jvm(&pr.prop_ty));
        if ext {
            format!("({}{value})V", type_descriptor(Ty::obj(owner)))
        } else {
            format!("({value})V")
        }
    });
    (name, descriptor)
}

fn box_property_reference_value(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::PropRef,
    physical: Ty,
) {
    if let Some(value_class) = property.boxed_value_class {
        let owner = value_class.render();
        let descriptor = format!("({})L{owner};", type_descriptor(ir_ty_to_jvm(&physical)));
        let method = cw.methodref(&owner, "box-impl", &descriptor);
        code.invokestatic(method, slot_words(ir_ty_to_jvm(&physical)) as i32, 1);
    } else if physical.is_jvm_scalar() {
        box_prim_free(
            cw,
            code,
            semantic_scalar_adapter(property.prop_ty, physical),
        );
    }
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
}

impl PropertyReferenceTarget {
    fn new(property: &crate::ir::PropRef, facade: &str) -> Self {
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
        let getter_descriptor = property_getter_descriptor(property, &owner, facade.is_some());
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
            boxed_value_class: property.boxed_value_class,
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
            boxed_value_class: property.boxed_value_class,
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
        if self.array_length {
            let class = cw.class_ref(self.owner);
            code.checkcast(class);
            code.arraylength();
        } else if let Some(facade) = self.facade {
            emit_object_as(cw, code, self.params[0]);
            let method = cw.methodref(facade, self.name, self.descriptor);
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
        if let Some(facade) = self.facade {
            emit_object_as(cw, code, self.params[0]);
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[1]);
            let arg_words = self
                .params
                .iter()
                .map(|param| slot_words(ir_ty_to_jvm(param)) as i32)
                .sum();
            let method = cw.methodref(facade, self.name, self.descriptor);
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
        let Some(value_class) = self.boxed_value_class else {
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
}

fn emit_prop_ref_class(c: &crate::ir::IrClass, facade: &str, opts: &EmitOptions) -> Vec<u8> {
    let pr = c.prop_ref.as_ref().unwrap();
    if pr.static_dispatch {
        return emit_toplevel_prop_ref_class(c, pr, facade, opts);
    }
    if pr.bound {
        return emit_bound_prop_ref_class(c, pr, facade, opts);
    }
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)
    add_singleton_instance_field(&mut cw, &fq);

    let target = PropertyReferenceTarget::new(pr, facade);
    emit_property_reference_constructor(&mut cw, &superclass, pr, &target, false);

    let mut get = CodeBuilder::new(2);
    get.aload(1);
    target
        .getter(pr)
        .emit_get(&mut cw, &mut get, target.getter_ret);
    box_property_reference_value(&mut cw, &mut get, pr, target.getter_ret);
    get.areturn();
    finish_code::<0x0001>(
        &mut cw,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &mut get,
        2,
    );

    if pr.mutable {
        let (setter, setter_desc) =
            property_setter_target(pr, &target.owner, target.facade.is_some());
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
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER

    let target = PropertyReferenceTarget::new(pr, facade);
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
    box_property_reference_value(&mut cw, &mut get, pr, target.getter_ret);
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);

    // `set(Object)V` (a bound `var` reference): `((Owner) this.receiver).setName(v)` after
    // casting/unboxing the argument to the property type.
    if pr.mutable {
        let (setter, setter_desc) =
            property_setter_target(pr, &target.owner, target.facade.is_some());
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
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    let owner = pr.owner_or_facade(facade);
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    add_singleton_instance_field(&mut cw, &fq);

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let prop_desc = type_descriptor(prop_jvm);
    let getter_desc = format!("(){prop_desc}");
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
    let gref = cw.methodref(&owner, &pr.getter_name, &getter_desc);
    get.invokestatic(gref, 0, slot_words(prop_jvm) as i32);
    if prop_jvm.is_jvm_scalar() {
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
        let setter = property_setter_name(&pr.prop_name);
        let setter_desc = format!("({prop_desc})V");
        let mut set = CodeBuilder::new(2);
        set.aload(1);
        if prop_jvm.is_jvm_scalar() {
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
        let sref = cw.methodref(&owner, &setter, &setter_desc);
        set.invokestatic(sref, slot_words(prop_jvm) as i32, 0);
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
/// Whether the facade declares a PRIVATE top-level function `name` with `arity` parameters — the
/// target of a function reference that must route through the `access$<name>` bridge.
fn private_facade_fn(ir: &IrFile, name: &str, arity: usize) -> bool {
    ir.functions.iter().enumerate().any(|(i, f)| {
        f.name == name
            && f.params.len() == arity
            && f.dispatch_receiver.is_none()
            && ir.private_methods.contains(&(i as u32))
    })
}

fn emit_func_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    use crate::ir::FrDispatch;
    let fr = c.func_ref.as_ref().unwrap();
    // A missing `owner_class`/`call_owner` is the facade sentinel (a top-level function lives on the
    // file facade, whose name isn't known until emit) — resolve it here.
    let owner_class = fr.owner_class_or_facade(facade);
    let owner_class = crate::jvm::jvm_class_map::to_jvm_internal(&owner_class).to_string();
    let call_owner = fr.call_owner_or_facade(facade);
    let call_owner = crate::jvm::jvm_class_map::to_jvm_internal(&call_owner).to_string();
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    // Package-private, kotlinc's shape — EXCEPT when the class lands cross-package (an
    // INLINE-SPLICED reference regenerates the callee module's adapter under the callee's package
    // while the caller lives elsewhere) or is referenced from a PUBLIC INLINE body
    // (`IrFile::public_synthetics`) — package-private there is an IllegalAccessError (corpus
    // `adaptedSuspendFunctionReference.kt`).
    let cross_package =
        fq.rsplit_once('/').map(|(p, _)| p) != facade.rsplit_once('/').map(|(p, _)| p);
    let inline_reachable = ir.public_synthetics.contains(&c.fq_name_id());
    cw.set_access(if cross_package || inline_reachable {
        0x0001 | 0x0010 | 0x0020 // PUBLIC | FINAL | SUPER
    } else {
        0x0010 | 0x0020 // FINAL | SUPER
    });
    cw.add_interface(&format!("kotlin/jvm/functions/Function{}", fr.arity));
    if fr.is_suspend || matches!(fr.dispatch, FrDispatch::SuspendConvert) {
        // The suspend-conversion adapter also carries kotlinc's suspend-function marker interface.
        cw.add_interface("kotlin/coroutines/jvm/internal/SuspendFunction");
    }

    // The call argument param types begin AFTER the receiver for an unbound member ref.
    let first_arg = match fr.dispatch {
        FrDispatch::VirtualUnbound => 1usize,
        _ => 0,
    };
    // For `StaticBound` the captured receiver is target arg 0, so invoke arg `k` maps to
    // `target_param_tys[k + 1]`.
    let target_offset = match fr.dispatch {
        FrDispatch::StaticBound => 1usize,
        _ => 0,
    };
    let target_ret_jvm = ir_ty_to_jvm(&fr.target_ret_ty);
    let target_returns_void = matches!(fr.target_ret_ty, Ty::Unit | Ty::Nothing);
    let coerce_unit = fr.ret_ty == Ty::Unit && !target_returns_void;
    // Reflection records the physical target descriptor without an unbound receiver.
    let mut signature_desc = String::from("(");
    for pt in fr.target_param_tys.iter().skip(first_arg) {
        signature_desc.push_str(&ir_type_desc(pt));
    }
    signature_desc.push(')');
    let signature_ret = if target_returns_void {
        "V".to_string()
    } else {
        type_descriptor(target_ret_jvm)
    };
    signature_desc.push_str(&signature_ret);
    let reflection_name = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
    let signature_name = match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound | FrDispatch::SuspendConvert => {
            reflection_name
        }
        FrDispatch::VirtualUnbound | FrDispatch::VirtualBound => {
            crate::jvm::names::mapped_builtin_virtual_name(&call_owner, reflection_name)
        }
    };
    let signature = format!("{signature_name}{signature_desc}");

    let call_desc = if matches!(fr.dispatch, FrDispatch::SuspendConvert) {
        // The delegated call is the wrapped value's ERASED `Function{n}.invoke` — `n` erased Object
        // parameters (the invoke's trailing continuation is dropped), Object return.
        let mut d = String::from("(");
        for _ in 0..fr.arity as usize - 1 {
            d.push_str("Ljava/lang/Object;");
        }
        d.push_str(")Ljava/lang/Object;");
        d
    } else {
        let mut d = String::from("(");
        for pt in fr.target_param_tys.iter().skip(first_arg) {
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

    if fr.bound {
        // `<init>(Object)V`: super(arity, receiver, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(2);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
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
        // The ctor's access mirrors the class's: a PUBLIC synthetic is constructed from other
        // packages by spliced code.
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        }
    } else {
        add_singleton_instance_field(&mut cw, &fq);
        // `<init>()V`: super(arity, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(1);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
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
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "()V", &mut ctor, 1);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);
        }
        emit_singleton_instance_clinit(&mut cw, &fq);
    }

    // The erased `invoke(Object×arity)Object`.
    let arity = fr.arity as u16;
    let mut invoke_desc = String::from("(");
    for _ in 0..arity {
        invoke_desc.push_str("Ljava/lang/Object;");
    }
    invoke_desc.push_str(")Ljava/lang/Object;");
    let mut inv = CodeBuilder::new(1 + arity);
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
            inv.aload(1);
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::Static => {}
        FrDispatch::StaticBound => {
            // The captured receiver is the FIRST static argument: load `this.receiver`, cast to the
            // target receiver type (`target_param_tys[0]`).
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            if let Some(vc) = &fr.staticbound_recv_unbox {
                // A VALUE-CLASS receiver (`Z(42)::ext`) is stored BOXED: `checkcast` to the box class then
                // `unbox-impl` to the underlying the mangled target expects (`Z`→`int`).
                let vc = vc.render();
                let cref = cw.class_ref(&vc);
                inv.checkcast(cref);
                let under = ir_ty_to_jvm(
                    fr.target_param_tys
                        .first()
                        .copied()
                        .as_ref()
                        .unwrap_or(&Ty::Error),
                );
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(under)));
                inv.invokevirtual(m, 0, slot_words(under) as i32);
            } else if let Some(primitive) = fr
                .target_param_tys
                .first()
                .map(ir_ty_to_jvm)
                .filter(|ty| ty.is_jvm_scalar())
            {
                unbox_prim(&mut cw, &mut inv, primitive);
            } else if let Some(internal) = fr
                .target_param_tys
                .first()
                .map(ir_ty_to_jvm)
                .and_then(checkcast_internal)
            {
                let cref = cw.class_ref(&internal);
                inv.checkcast(cref);
            }
        }
    };
    // Push the call arguments (cast/unbox each erased `Object`).
    let mut call_arg_words = match fr.dispatch {
        // The captured receiver already pushed above occupies one (reference) target slot.
        FrDispatch::StaticBound => fr
            .target_param_tys
            .first()
            .map_or(0, |t| slot_words(ir_ty_to_jvm(t)) as i32),
        _ => 0,
    };
    for (k, pt) in fr.param_tys.iter().enumerate().skip(first_arg) {
        // Suspend conversion: the trailing continuation parameter is NOT forwarded — the wrapped
        // plain function never suspends and takes only the value arguments.
        if matches!(fr.dispatch, FrDispatch::SuspendConvert) && k == fr.param_tys.len() - 1 {
            continue;
        }
        inv.aload(1 + k as u16);
        let jt = ir_ty_to_jvm(pt);
        let target_jt = fr
            .target_param_tys
            .get(k + target_offset)
            .map(ir_ty_to_jvm)
            .unwrap_or(jt);
        if jt.is_jvm_scalar() && target_jt.is_jvm_scalar() {
            let adapter = semantic_scalar_adapter(*pt, jt);
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(adapter).unwrap_or("java/lang/Object"),
            );
            inv.checkcast(wref);
            unbox_prim(&mut cw, &mut inv, adapter);
        } else if jt.is_jvm_scalar() && target_jt.is_reference() {
            let target = ref_internal(target_jt);
            if target != "java/lang/Object" {
                let cref = cw.class_ref(&target);
                inv.checkcast(cref);
            }
        } else if let Some(internal) = checkcast_internal(jt) {
            let cref = cw.class_ref(&internal);
            inv.checkcast(cref);
        }
        if let Some(vc) = fr
            .unbox_params
            .get(k)
            .and_then(|v| v.as_ref())
            .filter(|_| !jt.is_jvm_scalar())
        {
            let locals = func_ref_invoke_locals(&mut cw, &fq, arity);
            let stack_prefix = func_ref_call_stack_prefix(&mut cw, &fr.dispatch, &call_owner);
            emit_value_class_unbox_adapter(
                &mut cw,
                &mut inv,
                *vc,
                target_jt,
                fr.unbox_param_nullable.get(k).copied().unwrap_or(false),
                Some(locals),
                stack_prefix,
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
        && private_facade_fn(ir, &fr.call_name, fr.target_param_tys.len())
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
            let vn = crate::jvm::names::mapped_builtin_virtual_name(&call_owner, &fr.call_name);
            let m = cw.interface_methodref(&call_owner, vn, &call_desc);
            inv.invokeinterface(m, call_arg_words, ret_words);
        }
        _ => {
            let vn = crate::jvm::names::mapped_builtin_virtual_name(&call_owner, &fr.call_name);
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
    finish_code::<0x0001>(&mut cw, "invoke", &invoke_desc, &mut inv, 1 + arity);
    cw.finish()
}

fn func_ref_invoke_locals(cw: &mut ClassWriter, self_class: &str, arity: u16) -> Vec<VerifType> {
    let mut locals = vec![VerifType::Object(cw.class_ref(self_class))];
    let obj = VerifType::Object(cw.class_ref("java/lang/Object"));
    locals.extend(std::iter::repeat_n(obj, arity as usize));
    locals
}

fn func_ref_call_stack_prefix(
    cw: &mut ClassWriter,
    dispatch: &crate::ir::FrDispatch,
    call_owner: &str,
) -> Vec<VerifType> {
    match dispatch {
        crate::ir::FrDispatch::Static => Vec::new(),
        crate::ir::FrDispatch::VirtualBound
        | crate::ir::FrDispatch::VirtualUnbound
        | crate::ir::FrDispatch::StaticBound
        | crate::ir::FrDispatch::SuspendConvert => {
            vec![VerifType::Object(cw.class_ref(call_owner))]
        }
    }
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
    locals: Option<Vec<VerifType>>,
    stack_prefix: Vec<VerifType>,
) {
    let value_class = value_class.render();
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
    if let Some(locals) = locals {
        let mut null_stack = stack_prefix.clone();
        null_stack.push(VerifType::Object(cw.class_ref(&value_class)));
        let mut end_stack = stack_prefix;
        end_stack.push(verif_for_jvm_free(cw, target));
        code.add_frame_if_new(null, locals.clone(), null_stack);
        code.add_frame_if_new(end, locals, end_stack);
    }
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
    match ir_ty_to_jvm(elem) {
        Ty::Int => ("kotlin/jvm/internal/Ref$IntRef", "I"),
        Ty::Long => ("kotlin/jvm/internal/Ref$LongRef", "J"),
        Ty::Float => ("kotlin/jvm/internal/Ref$FloatRef", "F"),
        Ty::Double => ("kotlin/jvm/internal/Ref$DoubleRef", "D"),
        Ty::Boolean => ("kotlin/jvm/internal/Ref$BooleanRef", "Z"),
        Ty::Char => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Byte => ("kotlin/jvm/internal/Ref$ByteRef", "B"),
        Ty::Short => ("kotlin/jvm/internal/Ref$ShortRef", "S"),
        _ => ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;"),
    }
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
    code.ensure_locals(locals);
    code.link();
    cw.add_method(ACCESS, name, desc, code);
}

fn finish_bridge(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
) {
    finish_code::<{ 0x0001 | 0x0040 | 0x1000 }>(cw, name, desc, code, locals);
}

fn emit_bridge_barrier_outcome(
    outcome: crate::jvm::backend::BridgeBarrierOutcome,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    match outcome {
        crate::jvm::backend::BridgeBarrierOutcome::False => {
            code.push_int(0, cw);
            code.ireturn();
        }
        crate::jvm::backend::BridgeBarrierOutcome::NotFound => {
            code.push_int(-1, cw);
            code.ireturn();
        }
        crate::jvm::backend::BridgeBarrierOutcome::Null => {
            code.aconst_null();
            code.areturn();
        }
    }
}

/// Emit `ACC_BRIDGE|ACC_SYNTHETIC` methods: each has the supertype's erased descriptor, adapts its
/// arguments (type barrier / checkcast / unbox / numeric convert), delegates to the concrete override,
/// and adapts the return value back (box / numeric convert).
fn emit_bridges(c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    for b in &c.bridges {
        let ep = jvm_tys(&b.erased_params);
        let cp = jvm_tys(&b.concrete_params);
        let er = ir_ty_to_jvm(&b.erased_ret);
        let cr = ir_ty_to_jvm(&b.concrete_ret);
        let erased_desc = method_descriptor(&ep, er);
        // A bridge whose (name, descriptor) already names a REAL method on this class would be a
        // duplicate (`ClassFormatError`) — e.g. an interface getter `getX()T` overridden with the SAME
        // type differs from the impl only by a spurious nullability/representation detail. Skip it; the
        // real method already satisfies the interface. (Real methods are emitted before `emit_bridges`.)
        if cw.has_method(&b.name, &erased_desc) {
            continue;
        }
        let pw: u16 = ep.iter().map(|t| slot_words(*t)).sum();
        let mut code = CodeBuilder::new(1 + pw);
        if let Some(barrier) = crate::jvm::backend::bridge_barrier(b) {
            let dispatch = code.new_label();
            let parameter_slot = 1 + ep[..barrier.parameter]
                .iter()
                .map(|ty| slot_words(*ty))
                .sum::<u16>();
            if b.concrete_params[barrier.parameter].is_nullable() {
                code.aload(parameter_slot);
                code.ifnull(dispatch);
            }
            code.aload(parameter_slot);
            let concrete = ref_internal(cp[barrier.parameter]);
            let concrete_class = cw.class_ref(&concrete);
            code.instance_of(concrete_class);
            code.ifne(dispatch);
            emit_bridge_barrier_outcome(barrier.outcome, cw, &mut code);
            let mut locals = vec![VerifType::ObjectName(c.fq_name())];
            locals.extend(ep.iter().map(|ty| verif_for_jvm_free(cw, *ty)));
            code.add_frame_if_new(dispatch, locals, vec![]);
            code.bind(dispatch);
        }
        code.aload(0);
        let mut slot = 1u16;
        for (k, (et, ct)) in ep.iter().zip(&cp).enumerate() {
            load(*et, slot, &mut code);
            slot += slot_words(*et);
            // A boxed value-class param (a generic supertype method `f(Object,…)` delegating to a mangled
            // concrete override taking the underlying): checkcast the incoming `Object` to the boxed `X`,
            // then `unbox-impl` it to the underlying `ct` the target expects.
            if let Some(Some(vc)) = b.unbox_params.get(k) {
                let vc = vc.render();
                let ci = cw.class_ref(&vc);
                code.checkcast(ci);
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(*ct)));
                code.invokevirtual(m, 0, slot_words(*ct) as i32);
            } else if et != ct {
                if et.is_reference() && ct.is_reference() {
                    let ci = cw.class_ref(&ref_internal(*ct));
                    code.checkcast(ci);
                } else if et.is_reference() && ct.is_jvm_scalar() {
                    unbox_prim(cw, &mut code, *ct);
                } else if et.is_jvm_scalar() && ct.is_reference() {
                    // The erased slot is a PRIMITIVE but the concrete override takes the generic
                    // reference (`B.foo(int)` bridged onto `foo(t: T)="…"` erased to `Object`):
                    // box the scalar before delegating, exactly as kotlinc's bridge does.
                    box_prim_free(cw, &mut code, *et);
                } else if et.is_jvm_scalar() && ct.is_jvm_scalar() {
                    emit_num_conv(*et, *ct, &mut code);
                }
            }
        }
        let argw: i32 = cp.iter().map(|t| slot_words(*t) as i32).sum();
        // A value-class boxing bridge calls the mangled override (`target_name`) which returns the
        // erased underlying, then boxes the result back to `X` with `X.box-impl`.
        let target = b.target_name.as_deref().unwrap_or(&b.name);
        let owner = c.fq_name();
        let m = cw.methodref(&owner, target, &method_descriptor(&cp, cr));
        code.invokevirtual(m, argw, slot_words(cr) as i32);
        if cr.is_reference() && ref_internal(cr) == "java/lang/Void" && !er.is_reference() {
            // A `Nothing` override may have a `java/lang/Void` descriptor while the value-class
            // supertype bridge returns the unboxed primitive. The target must diverge; if it ever
            // falls through, discard the null-only Void result and throw to keep the bridge verifiable.
            code.pop();
            throw_assertion_error(cw, &mut code);
            finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
            continue;
        }
        if b.concrete_ret == Ty::Nothing {
            // Kotlin `Nothing` methods must not fall through. If the concrete descriptor still leaves a
            // physical carrier value, discard it before throwing so the assertion path starts with a clean
            // stack for every bridge return representation.
            if cr == Ty::Nothing {
                code.pop();
            } else {
                discard(cr, &mut code);
            }
            throw_assertion_error(cw, &mut code);
            finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
            continue;
        }
        if let Some(owner) = &b.box_ret {
            let owner = owner.render();
            let bi = cw.methodref(
                &owner,
                "box-impl",
                &format!(
                    "({}){}",
                    type_descriptor(cr),
                    type_descriptor(Ty::obj(&owner))
                ),
            );
            code.invokestatic(bi, slot_words(cr) as i32, 1);
        } else if cr != er {
            if er.is_reference() && cr.is_jvm_scalar() {
                box_prim_free(cw, &mut code, cr);
            } else if er.is_jvm_scalar() && cr.is_jvm_scalar() {
                emit_num_conv(cr, er, &mut code);
            } else if cr == Ty::Unit && er.is_reference() {
                // A `Unit`-returning override bridged to a reference-returning supertype method
                // (`B.foo(): Unit` over `A.foo(): Any`): the JVM call is void, so materialize the
                // `kotlin/Unit` singleton the erased bridge must return.
                let f = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            } else if er.is_reference() && cr.is_reference() && ref_internal(cr) == "java/lang/Void"
            {
                // `Nothing?` has only the value `null`, but its concrete JVM descriptor is
                // `java/lang/Void`. A bridge returning a narrower reference (for example a nullable
                // value class box) must refine the verifier type before `areturn`.
                let ci = cw.class_ref(&ref_internal(er));
                code.checkcast(ci);
            } else if er.is_reference() && !er.is_array() && ref_internal(cr) == "java/lang/Object"
            {
                // Covariant generic DIAMOND: the inherited concrete getter returns the erased
                // `Object` (`val x: T` in a generic base), but an interface in the hierarchy requires
                // a NARROWER reference type (`override val x: String`). This bridge's declared return
                // (`er`) is that narrower type, so the `Object` on the stack must be `checkcast` to it
                // before `areturn` — otherwise the verifier rejects it ("Bad return type"). The usual
                // direction (concrete is a SUBtype of erased) needs no cast; this is the inverse.
                // Restricted to a plain object type (`Ty::Obj`): an array `er` would need a descriptor-
                // form class ref, and that narrowing direction doesn't arise here.
                let ci = cw.class_ref(&ref_internal(er));
                code.checkcast(ci);
            } // reference→reference (concrete is a subtype of erased): no cast needed
        }
        emit_return(er, &mut code);
        finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
    }
}

/// Box a primitive on the stack to its wrapper (free-function form for the bridge emitter). A signed
/// primitive boxes via its `java/lang/*` `valueOf`; an UNSIGNED type via its inline-class wrapper's
/// `box-impl` (`kotlin/UInt.box-impl(I)Lkotlin/UInt;`) — both are rows in the one table.
fn box_prim_free(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
    let (cls, meth, desc) = match t {
        Ty::Int => ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;"),
        Ty::Long => ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;"),
        Ty::Double => ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;"),
        Ty::Float => ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;"),
        Ty::Boolean => ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;"),
        Ty::Char => ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;"),
        Ty::Byte => ("java/lang/Byte", "valueOf", "(B)Ljava/lang/Byte;"),
        Ty::Short => ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;"),
        Ty::UByte => ("kotlin/UByte", "box-impl", "(B)Lkotlin/UByte;"),
        Ty::UShort => ("kotlin/UShort", "box-impl", "(S)Lkotlin/UShort;"),
        Ty::UInt => ("kotlin/UInt", "box-impl", "(I)Lkotlin/UInt;"),
        Ty::ULong => ("kotlin/ULong", "box-impl", "(J)Lkotlin/ULong;"),
        _ => return,
    };
    let m = cw.methodref(cls, meth, desc);
    code.invokestatic(m, slot_words(t) as i32, 1);
}

/// Select the scalar whose box/unbox adapter owns a reference boundary before reducing that scalar
/// to its JVM carrier. Signed scalars are their carriers, but an unsigned scalar is an inline class:
/// `UInt` uses `kotlin/UInt.box-impl`/`unbox-impl` even though values inside a method use the same
/// `int` slots as `Int`. Keeping this choice in one helper prevents property, lambda, function-reference,
/// or future generic-boundary paths from each rediscovering unsigned wrappers after `ir_ty_to_jvm`
/// has intentionally erased the distinction.
fn semantic_scalar_adapter(semantic: Ty, carrier: Ty) -> Ty {
    let semantic = semantic.non_null();
    // Adapter selection must not CREATE a second boundary. Nullable scalars and other expressions
    // can already be represented by a wrapper reference when they reach a generic consumer. In that
    // case the carrier is the authority and this helper is deliberately a no-op; choosing the
    // non-null semantic scalar would try to feed that existing reference into another `box-impl` or
    // `valueOf`. Only a physical scalar crossing into/out of a reference slot needs semantic wrapper
    // identity.
    if carrier.is_jvm_scalar() && semantic.is_jvm_scalar() {
        semantic
    } else {
        carrier
    }
}

/// Unbox a wrapper on the stack to the primitive `t` (free-function form for the bridge emitter).
fn unbox_prim(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
    crate::trace_compiler!("value_classes", "emit scalar unbox adapter={t:?}");
    let (cls, meth, desc) = match t {
        Ty::Int => ("java/lang/Integer", "intValue", "()I"),
        Ty::Long => ("java/lang/Long", "longValue", "()J"),
        Ty::Double => ("java/lang/Double", "doubleValue", "()D"),
        Ty::Float => ("java/lang/Float", "floatValue", "()F"),
        Ty::Boolean => ("java/lang/Boolean", "booleanValue", "()Z"),
        Ty::Char => ("java/lang/Character", "charValue", "()C"),
        Ty::Byte => ("java/lang/Byte", "byteValue", "()B"),
        Ty::Short => ("java/lang/Short", "shortValue", "()S"),
        // An unsigned wrapper unboxes via its inline-class `unbox-impl` (a row, not a special case).
        Ty::UByte => ("kotlin/UByte", "unbox-impl", "()B"),
        Ty::UShort => ("kotlin/UShort", "unbox-impl", "()S"),
        Ty::UInt => ("kotlin/UInt", "unbox-impl", "()I"),
        Ty::ULong => ("kotlin/ULong", "unbox-impl", "()J"),
        _ => return,
    };
    let ci = cw.class_ref(cls);
    code.checkcast(ci);
    let m = cw.methodref(cls, meth, desc);
    code.invokevirtual(m, 0, slot_words(t) as i32);
}

/// Emit a Kotlin `annotation class` as a JVM ANNOTATION INTERFACE: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT|
/// ACC_ANNOTATION`, extending `java/lang/annotation/Annotation`, with one `public abstract` accessor per
/// member (`int x()`, `String s()`) named after the property and returning its type — kotlinc's shape.
/// Members come from `fields`. Instances are built by the synthetic impl ([`emit_annotation_impl_class`]).
fn emit_annotation_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0200 | 0x0400 | 0x2000); // PUBLIC | INTERFACE | ABSTRACT | ANNOTATION
    cw.add_interface("java/lang/annotation/Annotation");
    for field in &c.fields {
        let ret = ir_ty_to_jvm(&field.ty);
        cw.add_abstract_method(0x0401, &field.name, &format!("(){}", type_descriptor(ret)));
        // PUBLIC|ABSTRACT
    }
    // Retention meta-annotations, matching kotlinc: an EXPLICIT `@Retention(X)` stamps
    // `kotlin.annotation.Retention(X)` first, and every annotation class carries
    // `java.lang.annotation.Retention(RUNTIME|CLASS|SOURCE)` (RUNTIME when defaulted) — the java one is
    // what both the JVM and classpath consumers read the retention back from.
    let mut meta: Vec<crate::ir::AppliedAnnotation> = Vec::new();
    if let Some(retention) = c.annotation_retention {
        use crate::ir::AnnoRetention;
        let enum_stamp =
            |internal: &str, enum_ty: &str, constant: &str| crate::ir::AppliedAnnotation {
                internal: crate::types::type_name(internal),
                values: vec![(
                    "value".to_string(),
                    crate::ir::AnnoValue::Enum(
                        crate::types::type_name(enum_ty),
                        constant.to_string(),
                    ),
                )],
            };
        let kotlin_name = match retention {
            AnnoRetention::Default => None,
            AnnoRetention::Runtime => Some("RUNTIME"),
            AnnoRetention::Binary => Some("BINARY"),
            AnnoRetention::Source => Some("SOURCE"),
        };
        if let Some(constant) = kotlin_name {
            meta.push(enum_stamp(
                "kotlin/annotation/Retention",
                "kotlin/annotation/AnnotationRetention",
                constant,
            ));
        }
        let policy = match retention {
            AnnoRetention::Default | AnnoRetention::Runtime => "RUNTIME",
            AnnoRetention::Binary => "CLASS",
            AnnoRetention::Source => "SOURCE",
        };
        meta.push(enum_stamp(
            "java/lang/annotation/Retention",
            "java/lang/annotation/RetentionPolicy",
            policy,
        ));
    }
    let kotlin_retention = crate::types::type_name("kotlin/annotation/Retention");
    meta.extend(
        c.applied_annotations
            .iter()
            .filter(|annotation| annotation.internal != kotlin_retention)
            .cloned(),
    );
    cw.set_runtime_annotations(&meta);
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// The boxed-wrapper internal name + a static `hashCode` helper descriptor for a primitive `Ty`, used by
/// the annotation impl's `hashCode`. Returns `(wrapper_internal, hashCode_arg_descriptor)`.
fn prim_wrapper(t: Ty) -> Option<(&'static str, &'static str)> {
    Some(match t {
        Ty::Boolean => ("java/lang/Boolean", "Z"),
        Ty::Byte => ("java/lang/Byte", "B"),
        Ty::Short => ("java/lang/Short", "S"),
        Ty::Char => ("java/lang/Character", "C"),
        Ty::Int => ("java/lang/Integer", "I"),
        Ty::Long => ("java/lang/Long", "J"),
        Ty::Float => ("java/lang/Float", "F"),
        Ty::Double => ("java/lang/Double", "D"),
        _ => return None,
    })
}

/// Java `String.hashCode()` of `s` (the annotation `hashCode` weights each member by `127 *
/// name.hashCode()`, a compile-time constant).
fn java_string_hash(s: &str) -> i32 {
    s.chars()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

/// Emit the synthetic IMPLEMENTATION class for a Kotlin annotation instantiation (`A(args)`): a final
/// class implementing the annotation interface `iface` and the full `java.lang.annotation.Annotation`
/// contract — private final fields, a constructor, per-member accessors (`x()`/`s()`), `annotationType()`,
/// and content-correct `equals`/`hashCode`/`toString` (arrays via `java.util.Arrays`, `float`/`double` via
/// their wrappers' `equals`/`hashCode` for NaN/`-0.0` semantics). `c.fields` are the members in order.
fn emit_annotation_impl_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    iface: &str,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let members: Vec<(String, Ty)> = c
        .fields
        .iter()
        .map(|f| (f.name.clone(), ir_ty_to_jvm(&f.ty)))
        .collect();
    let mut cw = new_writer(&fq, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0010 | 0x1000); // PUBLIC | FINAL | SYNTHETIC
    cw.add_interface(iface);
    for (name, jt) in &members {
        cw.add_field(0x0002 | 0x0010, name, &type_descriptor(*jt)); // PRIVATE | FINAL
    }

    // <init>(members…): super(); store each arg to its field.
    {
        let params_words: u16 = members.iter().map(|(_, jt)| slot_words(*jt)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        ctor.aload(0);
        let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
        ctor.invokespecial(obj_init, 0, 0);
        let mut slot = 1u16;
        for (name, jt) in &members {
            ctor.aload(0);
            load(*jt, slot, &mut ctor);
            let fref = cw.fieldref(&fq, name, &type_descriptor(*jt));
            ctor.putfield(fref, slot_words(*jt) as i32);
            slot += slot_words(*jt);
        }
        let desc = format!(
            "({})V",
            members
                .iter()
                .map(|(_, jt)| type_descriptor(*jt))
                .collect::<String>()
        );
        ctor.ret_void();
        finish_code::<0x0001>(&mut cw, "<init>", &desc, &mut ctor, 1 + params_words);
        // A default on any annotation member (`annotation class C(val i: Int = 1)`) → the same synthetic
        // `<init>(members…, int mask, DefaultConstructorMarker)` overload an ordinary class gets. The impl
        // class is what `C()` actually constructs, so without it a call omitting a default targets a
        // constructor nothing emits (`NoSuchMethodError`). kotlinc emits it on the impl class too.
        if let Some(defaults) = ir.class_ctor_defaults(&fq) {
            let param_tys: Vec<Ty> = members.iter().map(|(_, jt)| *jt).collect();
            emit_ctor_default_stub(ir, &fq, facade, &param_tys, defaults, &mut cw, env);
        }
    }

    // Per-member accessor `x()T`: return this.x.
    for (name, jt) in &members {
        let mut g = CodeBuilder::new(1);
        g.aload(0);
        let fref = cw.fieldref(&fq, name, &type_descriptor(*jt));
        g.getfield(fref, slot_words(*jt) as i32);
        emit_return(*jt, &mut g);
        finish_code::<0x0011>(
            &mut cw,
            name,
            &format!("(){}", type_descriptor(*jt)),
            &mut g,
            1,
        );
    }

    // annotationType(): return <iface>.class.
    {
        let mut m = CodeBuilder::new(1);
        m.ldc_class(iface, &mut cw);
        m.areturn();
        finish_code::<0x0011>(&mut cw, "annotationType", "()Ljava/lang/Class;", &mut m, 1);
    }

    emit_annotation_equals(&mut cw, &fq, iface, &members);
    emit_annotation_hashcode(&mut cw, &fq, &members);
    emit_annotation_tostring(&mut cw, &fq, iface, &members);
    cw.finish()
}

/// `equals(Object)Z` for an annotation impl: `o` must be an instance of the annotation interface and every
/// member must be equal (arrays compared by content via `Arrays.equals`; `float`/`double` via their
/// wrappers' `equals` so `NaN`==`NaN` and `-0.0`!=`0.0` per the annotation contract; other references via
/// `Object.equals`). One `false` exit label.
fn emit_annotation_equals(cw: &mut ClassWriter, fq: &str, iface: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(2); // this=0, o=1
    cb.ensure_locals(3); // +o-as-iface at local 2
    let lfalse = cb.new_label();
    let icls = cw.class_ref(iface);
    cb.aload(1);
    cb.instance_of(icls);
    cb.ifeq(lfalse);
    cb.aload(1);
    cb.checkcast(icls);
    cb.astore(2);
    for (name, jt) in members {
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        let aref = cw.interface_methodref(iface, name, &format!("(){}", type_descriptor(*jt)));
        let push_this = |cb: &mut CodeBuilder| {
            cb.aload(0);
            cb.getfield(fref, slot_words(*jt) as i32);
        };
        let push_other = |cb: &mut CodeBuilder| {
            cb.aload(2);
            cb.invokeinterface(aref, 0, slot_words(*jt) as i32);
        };
        match *jt {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean => {
                push_this(&mut cb);
                push_other(&mut cb);
                cb.if_icmpne(lfalse);
            }
            Ty::Long => {
                push_this(&mut cb);
                push_other(&mut cb);
                cb.lcmp();
                cb.ifne(lfalse);
            }
            Ty::Float | Ty::Double => {
                let (wrap, pd) = prim_wrapper(*jt).unwrap();
                let valueof = cw.methodref(wrap, "valueOf", &format!("({pd})L{wrap};"));
                push_this(&mut cb);
                cb.invokestatic(valueof, slot_words(*jt) as i32, 1);
                push_other(&mut cb);
                cb.invokestatic(valueof, slot_words(*jt) as i32, 1);
                let eq = cw.methodref(wrap, "equals", "(Ljava/lang/Object;)Z");
                cb.invokevirtual(eq, 1, 1);
                cb.ifeq(lfalse);
            }
            _ if jt.is_array() => {
                let arr_desc = arrays_param_desc(*jt);
                let eq = cw.methodref(
                    "java/util/Arrays",
                    "equals",
                    &format!("({arr_desc}{arr_desc})Z"),
                );
                push_this(&mut cb);
                push_other(&mut cb);
                cb.invokestatic(eq, 2, 1);
                cb.ifeq(lfalse);
            }
            _ => {
                // Reference member (String / enum / nested annotation): Object.equals.
                push_this(&mut cb);
                push_other(&mut cb);
                let eq = cw.methodref("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
                cb.invokevirtual(eq, 1, 1);
                cb.ifeq(lfalse);
            }
        }
    }
    cb.push_int(1, cw);
    cb.ireturn();
    cb.bind(lfalse);
    let impl_ref = cw.class_ref(fq);
    let obj_ref = cw.class_ref("java/lang/Object");
    cb.add_frame_if_new(
        lfalse,
        vec![VerifType::Object(impl_ref), VerifType::Object(obj_ref)],
        vec![],
    );
    cb.push_int(0, cw);
    cb.ireturn();
    cb.set_needs_stackmap();
    cb.link();
    cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &cb);
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

/// `hashCode()I` for an annotation impl: the contract sum of `(127 * memberName.hashCode()) ^
/// memberValue.hashCode()` over members (arrays via `Arrays.hashCode`, primitives via their wrappers'
/// static `hashCode`). Straight-line (no frames).
fn emit_annotation_hashcode(cw: &mut ClassWriter, fq: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(1);
    cb.push_int(0, cw); // acc
    for (name, jt) in members {
        cb.push_int(127i32.wrapping_mul(java_string_hash(name)), cw);
        // value.hashCode():
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        cb.aload(0);
        cb.getfield(fref, slot_words(*jt) as i32);
        match *jt {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char => { /* int value IS its hashCode */ }
            Ty::Boolean | Ty::Long | Ty::Float | Ty::Double => {
                let (wrap, pd) = prim_wrapper(*jt).unwrap();
                let hc = cw.methodref(wrap, "hashCode", &format!("({pd})I"));
                cb.invokestatic(hc, slot_words(*jt) as i32, 1);
            }
            _ if jt.is_array() => {
                let ad = arrays_param_desc(*jt);
                let hc = cw.methodref("java/util/Arrays", "hashCode", &format!("({ad})I"));
                cb.invokestatic(hc, 1, 1);
            }
            _ => {
                let hc = cw.methodref("java/lang/Object", "hashCode", "()I");
                cb.invokevirtual(hc, 0, 1);
            }
        }
        cb.ixor();
        cb.iadd();
    }
    cb.ireturn();
    finish_code::<0x0011>(cw, "hashCode", "()I", &mut cb, 1);
}

/// `toString()` for an annotation impl: `@<fqName>(m1=v1, m2=v2, …)` built with a `StringBuilder` (arrays
/// rendered via `Arrays.toString`). Straight-line (no frames).
fn emit_annotation_tostring(cw: &mut ClassWriter, fq: &str, iface: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(1);
    let sb = "java/lang/StringBuilder";
    let sb_cls = cw.class_ref(sb);
    cb.new_obj(sb_cls);
    cb.dup();
    let sb_init = cw.methodref(sb, "<init>", "()V");
    cb.invokespecial(sb_init, 0, 0);
    let append_str = cw.methodref(
        sb,
        "append",
        "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
    );
    let append_lit = |cb: &mut CodeBuilder, cw: &mut ClassWriter, s: &str| {
        cb.push_string(s, cw);
        cb.invokevirtual(append_str, 1, 1);
    };
    append_lit(&mut cb, cw, &format!("@{}(", iface.replace('/', ".")));
    for (i, (name, jt)) in members.iter().enumerate() {
        append_lit(
            &mut cb,
            cw,
            &format!("{}{}=", if i == 0 { "" } else { ", " }, name),
        );
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        match *jt {
            _ if jt.is_array() => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ad = arrays_param_desc(*jt);
                let ats = cw.methodref(
                    "java/util/Arrays",
                    "toString",
                    &format!("({ad})Ljava/lang/String;"),
                );
                cb.invokestatic(ats, 1, 1);
                cb.invokevirtual(append_str, 1, 1);
            }
            Ty::Int | Ty::Short | Ty::Byte => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(I)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Char => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(C)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Boolean => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(Z)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Long => {
                cb.aload(0);
                cb.getfield(fref, 2);
                let ap = cw.methodref(sb, "append", "(J)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 2, 1);
            }
            Ty::Float => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(F)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Double => {
                cb.aload(0);
                cb.getfield(fref, 2);
                let ap = cw.methodref(sb, "append", "(D)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 2, 1);
            }
            Ty::String => {
                cb.aload(0);
                cb.getfield(fref, 1);
                cb.invokevirtual(append_str, 1, 1);
            }
            _ => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(
                    sb,
                    "append",
                    "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
                );
                cb.invokevirtual(ap, 1, 1);
            }
        }
    }
    append_lit(&mut cb, cw, ")");
    let to_str = cw.methodref(sb, "toString", "()Ljava/lang/String;");
    cb.invokevirtual(to_str, 0, 1);
    cb.areturn();
    finish_code::<0x0011>(cw, "toString", "()Ljava/lang/String;", &mut cb, 1);
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
    let signature_formatter = JvmSignatureFormatter::new(env);
    let mut cw = new_classifier_writer(ir, c, "java/lang/Object", env, opts);
    cw.set_access(0x0001 | 0x0200 | 0x0400); // PUBLIC | INTERFACE | ABSTRACT
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }
    register_sealed_subtypes(
        &mut cw,
        ir,
        c,
        opts.class_major.unwrap_or(MAJOR_JAVA8) >= 61,
    );
    register_inner_classes(&mut cw, ir);
    let mut default_impls: Option<ClassWriter> = None;
    // Whether this compilation publishes the `<Iface>$DefaultImpls` compatibility holder at all.
    let emits_default_impls = opts.jvm_default != JvmDefaultMode::NoCompatibility;
    // `disable` puts NO body on the interface: every member is abstract and the bodies live on the
    // holder as statics taking the receiver as parameter 0. The other two modes emit the body here as
    // a JVM default method.
    let bodies_on_interface = opts.jvm_default != JvmDefaultMode::Disable;
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        // A STATIC member of an interface is not a default method: a lambda synthetic
        // (`f$lambda$0`) is a private static helper that stays where it is. Moving it to the holder
        // prepended a receiver its body never reads, and published it abstract on the interface —
        // the JVM rejected the result (`VerifyError: Bad type on operand stack`).
        if f.body.is_some() && (bodies_on_interface || f.is_static) {
            // A default method — concrete instance method on the interface.
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
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
            // An abstract method still carries kotlinc's nullability annotations: `@NotNull` /
            // `@Nullable` on each reference parameter and on a reference return. Having no body is
            // why it gets no debug tables — it is not a reason to drop its annotations.
            let ann = |t: Ty| -> Option<&'static str> {
                let d = crate::jvm::names::type_descriptor(t);
                if !(d.starts_with('L') || d.starts_with('[')) {
                    return None;
                }
                Some(if matches!(t, Ty::Nullable(_)) {
                    "Lorg/jetbrains/annotations/Nullable;"
                } else {
                    "Lorg/jetbrains/annotations/NotNull;"
                })
            };
            // Declared parameter nullability lives in the side-table (kept off `f.params` for the
            // mangle) — apply it so a nullable reference parameter reads `@Nullable`.
            let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
            let params: Vec<Option<&str>> = f
                .params
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if declared_nullable
                        .and_then(|v| v.get(i))
                        .copied()
                        .unwrap_or(false)
                    {
                        ann(Ty::nullable(*t))
                    } else {
                        ann(*t)
                    }
                })
                .collect();
            if ann(f.ret).is_some() || params.iter().any(Option::is_some) {
                cw.set_method_nullability(&f.name, &desc, ann(f.ret), &params);
            }
            // PUBLIC | ABSTRACT
        }
        // An interface method with default parameters gets a STATIC `<name>$default(iface, params…, mask,
        // marker)` (the JVM realization of interface default args) — it applies the defaults then dispatches
        // to the abstract method via `invokeinterface`. kotlinc emits it ON THE INTERFACE (call sites use
        // it) AND, under a mode that keeps the compatibility holder, a copy on the
        // `<Iface>$DefaultImpls` class (`public final`).
        if let Some(defaults) = ir.param_defaults(fid) {
            // `disable` puts NOTHING executable on the interface, the `$default` stub included: call
            // sites go to the holder's copy instead.
            if bodies_on_interface {
                emit_default_stub(ir, fid, &fq_name, facade, &mut cw, defaults, env, true);
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
                emit_default_stub(ir, fid, &fq_name, facade, di, defaults, env, true);
            }
        }
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
        di.set_kotlin_metadata(3, &[2, 4, 0], 48, &[], &[]);
        extra.push((holder, di.finish()));
    }
    // A companion `val` on the interface is a `public static final` field ON THE INTERFACE (interface
    // fields are implicitly static final): a `const val` as a `ConstantValue`, a non-const `val`
    // initialized in the interface's `<clinit>`. Read as `getstatic C.X`.
    for s in ir.statics.iter().filter(|s| s.owner_matches(&fq_name)) {
        let desc = ir_type_desc(&s.ty);
        if let Some(cv) = const_value_idx(ir, s.init, &mut cw) {
            cw.add_field_const(0x0019, &s.name, &desc, cv); // PUBLIC | STATIC | FINAL
        } else {
            cw.add_field(0x0019, &s.name, &desc);
        }
    }
    // A `companion object` with methods: a `public static final Companion` field of the synthesized
    // `C$Companion` type, constructed (or, for the interface layout, ALIASED from the companion's
    // own `$$INSTANCE`) in the interface's `<clinit>`. kotlinc writes the `<clinit>` BEFORE the
    // field, so its name and body refs intern first — the field add then dedups.
    let clinit_statics: Vec<&crate::ir::IrStatic> = ir
        .statics
        .iter()
        .filter(|s| s.owner_matches(&fq_name) && !(s.is_const && const_value_idx_peek(ir, s.init)))
        .collect();
    if c.companion_class.is_some() || !clinit_statics.is_empty() {
        cw.reserve_method_name("<clinit>");
        cw.seed_utf8("()V");
        let mut e = Emitter::new(
            ir,
            &mut cw,
            env,
            &fq_name,
            facade,
            Ty::Unit,
            clinit_statics.iter().map(|property| property.init),
        );
        let mut clinit = CodeBuilder::new(0);
        emit_companion_init(e.cw, &mut clinit, &fq_name, c);
        for s in &clinit_statics {
            e.emit_value(s.init, &mut clinit);
            let jt = ir_ty_to_jvm(&s.ty);
            let fref = e.cw.fieldref(&fq_name, &s.name, &type_descriptor(jt));
            clinit.putstatic(fref, slot_words(jt) as i32);
        }
        clinit.ret_void();
        clinit.ensure_locals(e.next_slot);
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }
    add_companion_field(&mut cw, c);
    // An interface is a VIEW of the same `IrClass` every other kind is — compute its `@Metadata` (and
    // therefore its debug tables/annotations) through the shared path, exactly like `emit_class`.
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if computed.is_some() {
        attach_synth_debug_tables(ir, c, &mut cw, opts.param_assertions, &[]);
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
    let signature_formatter = JvmSignatureFormatter::new(env);
    let self_desc = format!("L{fq};");
    let arr_desc = format!("[{self_desc}");
    // An enum extends the PARAMETERIZED `java.lang.Enum<E>`, so it carries a class `Signature` —
    // whose value interns between the class and superclass names, as ASM visits them.
    let mut cw = new_classifier_writer(ir, c, "java/lang/Enum", env, opts);
    // An enum with an abstract member is `ACC_ABSTRACT`; one with any bodied entry (so a subclass
    // extends it) must not be `final`. A plain enum stays `final`.
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    let has_subclass = c.enum_entries.iter().any(|e| e.subclass.is_some());
    let mut access = 0x0001 | 0x0020 | ACC_ENUM; // PUBLIC | SUPER | ENUM
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
    // Interfaces the enum implements (`enum class E : I`) — without these the JVM rejects an
    // interface-typed call with `IncompatibleClassChangeError`.
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }

    let field_tys = field_jvm_tys(&c.fields);
    // (bridges emitted after the methods below — `emit_bridges` references emitted method refs)
    let n_params = c.ctor_param_count as usize;
    let user_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    // Property backing fields are private (kotlinc), reached through the synthesized `getX()`/`setX()`
    // accessors — for both the primary-constructor fields and body member-property fields
    // (`enum class E { A; val x = … }`), initialized in the constructor via `init_body`.
    let enum_field_acc = |f: &IrField| {
        (if f.is_private() { 0x0002 } else { 0x0001 }) | if f.is_final() { 0x0010 } else { 0 }
    };
    for (f, t) in c.fields[..n_params].iter().zip(&user_tys) {
        cw.add_field(enum_field_acc(f), &f.name, &type_descriptor(*t));
    }
    for (f, t) in c.fields[n_params..].iter().zip(&field_tys[n_params..]) {
        cw.add_field(enum_field_acc(f), &f.name, &type_descriptor(*t));
    }
    // kotlinc visits the whole CONSTRUCTOR before the entry constants — name, descriptor, its generic
    // `Signature` (the two synthetic `Enum` params are erased, leaving `()V`), then its
    // LocalVariableTable strings. `add_field` would otherwise claim those slots for the first entry.
    cw.reserve_method_name("<init>");
    cw.reserve_descriptor(&format!("(Ljava/lang/String;I{})V", ctor_field_descs(c)));
    cw.reserve_descriptor(&format!("({})V", ctor_field_descs(c)));
    // The ctor BODY's `super(name, ordinal)` call resolves before its LocalVariableTable strings.
    cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    cw.reserve_method_name("this");
    cw.reserve_descriptor(&self_desc);
    cw.reserve_method_name("$enum$name");
    cw.reserve_descriptor("Ljava/lang/String;");
    cw.reserve_method_name("$enum$ordinal");
    cw.reserve_descriptor("I");
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
    cw.reserve_method_name("valueOf");
    cw.reserve_descriptor(&format!("(Ljava/lang/String;){self_desc}"));
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
        apply_field_annotations(&mut cw, c, &entry.name);
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
    // `<clinit>`'s NAME interns before anything its body references (the `EnumEntriesKt.enumEntries`
    // machinery), as kotlinc reaches a method's signature before its code.
    cw.reserve_method_name("<clinit>");
    // A `@Serializable enum`'s serializer machinery: a `public static final Companion` field + any
    // owner-scoped statics the serialization plugin synthesized (`$cachedSerializer$delegate`), both
    // initialized in `<clinit>` below.
    add_companion_field(&mut cw, c);
    let owner_statics: Vec<&crate::ir::IrStatic> =
        ir.statics.iter().filter(|s| s.owner_matches(&fq)).collect();
    for s in &owner_statics {
        // A `var` is reassignable, so it must not carry ACC_FINAL (see the class path).
        let final_flag = if s.is_var { 0x0000 } else { 0x0010 };
        let acc = if s.visibility.is_private() {
            0x000A | final_flag // PRIVATE | STATIC [| FINAL]
        } else {
            0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
        };
        cw.add_field(acc, &s.name, &ir_type_desc(&s.ty));
    }

    // Private constructor `(Ljava/lang/String;I<user params>)V` → `super(name, ordinal)` then store the
    // property params / run the body-property initializers. The user params are ALL primary-ctor params
    // (from `ctor_args`) — a `val`/`var` param backs a field, a plain param is an argument only (in scope
    // for a body-property initializer), so `all_param_tys` can be wider than the `n_params` fields.
    let all_param_tys = class_ctor_jvm_tys(c);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(all_param_tys.iter().copied())
        .collect();
    let ctor_desc = method_descriptor(&ctor_params, Ty::Unit);
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    ctor.aload(1);
    load(Ty::Int, 2, &mut ctor);
    let super_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    ctor.invokespecial(super_init, 2, 0);
    let mut max_locals = 1 + ctor_words;
    // When body-property initializers exist, the lowered `init_body` carries BOTH the property-param→
    // field stores AND the body inits (it set `explicit_param_stores`). Emit it through the standard IR
    // emitter, mapping value ids onto the enum's slot layout — `this` at 0, then EVERY user param at
    // slots 3+ (after the synthetic `name`/`ordinal`), in declaration order. Otherwise hand-store just
    // the property-param fields (a plain param has no field), reading each at its own slot.
    if let Some(init_body) = c.init_body.filter(|_| c.fields.len() > n_params) {
        let mut e = Emitter::new(ir, &mut cw, env, &fq, facade, Ty::Unit, [init_body]);
        e.next_slot = 1 + ctor_words;
        e.slots.insert(0, (0, Ty::obj(&fq)));
        let mut s = 3u16;
        for (i, t) in all_param_tys.iter().enumerate() {
            e.slots.insert(i as u32 + 1, (s, *t));
            s += slot_words(*t);
        }
        e.emit(init_body, &mut ctor);
        max_locals = max_locals.max(e.next_slot);
    } else {
        let mut slot = 3u16;
        let mut field_i = 0usize;
        for (a, t) in c.ctor_args.iter().zip(&all_param_tys) {
            if a.is_field {
                let name = &c.fields[field_i].name;
                ctor.aload(0);
                load(*t, slot, &mut ctor);
                let fref = cw.fieldref(&fq, name, &type_descriptor(*t));
                ctor.putfield(fref, slot_words(*t) as i32);
                field_i += 1;
            }
            slot += slot_words(*t);
        }
    }
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
    let ctor_sig = {
        let mut s = String::from("(");
        for t in &all_param_tys {
            s.push_str(&type_descriptor(*t));
        }
        s.push_str(")V");
        s
    };
    cw.add_method_sig(base_ctor_acc, "<init>", &ctor_desc, &ctor, Some(&ctor_sig));

    // <clinit>: construct each entry, then `$VALUES = $values()` and
    // `$ENTRIES = EnumEntriesKt.enumEntries($VALUES)`. BUILT here but ADDED last (kotlinc orders it
    // after values/valueOf/getEntries/$values); the linked `CodeBuilder` is self-contained.
    let ctor_argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
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
                .flat_map(|entry| entry.args.iter().copied()),
        );
        let mut clinit = CodeBuilder::new(0);
        // kotlinc gives each entry's construction its own `<clinit>` LineNumberTable entry, on that
        // Consecutive enum entries on one source line share one LNT entry.
        for (i, entry) in c.enum_entries.iter().enumerate() {
            if entry.decl_line != 0 && clinit_lines.last().map(|&(_, l)| l) != Some(entry.decl_line)
            {
                clinit_lines.push((clinit.bytes.len() as u16, entry.decl_line));
            }
            let args = &entry.args;
            // A branchy entry arg (`X(1 == 1)`) must run on a clean stack — spill all args to temps
            // first, then construct (mirrors the `New` node's spill).
            let spill = args.iter().any(|&a| e.records_frame(a));
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
            if spill {
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut clinit);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                for &a in args {
                    e.emit_value(a, &mut clinit);
                }
            }
            let ctor_ref = e.cw.methodref(&new_class, "<init>", &ctor_desc);
            clinit.invokespecial(ctor_ref, ctor_argw, 0);
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
        // A `@Serializable enum`'s serializer statics (`$cachedSerializer$delegate`) then its `Companion`
        // — same shape as a plain class's `<clinit>` companion/static init.
        for s in &owner_statics {
            e.emit_value(s.init, &mut clinit);
            let jt = ir_ty_to_jvm(&s.ty);
            let fref = e.cw.fieldref(&fq, &s.name, &type_descriptor(jt));
            clinit.putstatic(fref, slot_words(jt) as i32);
        }
        emit_companion_init(e.cw, &mut clinit, &fq, c);
        clinit.ret_void();
        // `max_locals` is exactly what the body allocated — entry-arg spills bump `next_slot`, and a
        // `<clinit>` that spills nothing has no locals at all (kotlinc writes 0, not a floor of 2).
        clinit.ensure_locals(e.next_slot);
        clinit.link();
        clinit
    };

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
    finish_code::<0x0009>(
        &mut cw,
        "valueOf",
        &format!("(Ljava/lang/String;){self_desc}"),
        &mut vof,
        1,
    );

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

    emit_declared_property_accessors(
        ir,
        c,
        &fq,
        &mut cw,
        &signature_formatter,
        opts.param_assertions,
    );
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // Honor `is_static` (an extension-synthesized `static` member like serialization's
            // `serializer()` accessor) — emitting it as an instance method breaks an `E.serializer()`
            // static call (`IncompatibleClassChangeError`).
            emit_method(ir, fid, &fq, facade, &mut cw, !f.is_static, env);
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

    // <clinit> is added LAST (built earlier), matching kotlinc's member order.
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);

    // Erased bridges for a generic-interface method overridden at the enum level
    // (`enum E : A<String> { …; override fun foo(t: String) }` → bridge `foo(Object)`→`foo(String)`).
    emit_bridges(c, &mut cw);
    // An enum is a VIEW of the same `IrClass` — compute its `@Metadata` (and hence debug tables /
    // annotations) through the shared path, exactly like `emit_class` and `emit_interface_class`.
    // An `enum class` implementing an interface needs the same holder forwarders an ordinary class
    // does — it reaches emission through this function, not `emit_class`.
    emit_default_impls_forwarders(ir, c, &mut cw, env);
    if let Some(m) = opts
        .emit_class_metadata
        .then(|| build_class_metadata(ir, c, opts))
        .flatten()
    {
        attach_synth_debug_tables(ir, c, &mut cw, opts.param_assertions, &[]);
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
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
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
    if ir.inline_only_fns.contains(&fid) {
        return;
    }
    emit_method_inner(ir, fid, owner, facade, cw, instance, env);
}

/// Under `-jvm-default=disable`, emit an override on `c` for each inherited interface member whose
/// body lives on that interface's `$DefaultImpls` holder.
///
/// kotlinc emits `public <ret> f(args) { return I$DefaultImpls.f(this, args); }`. A member the class
/// declares itself is left alone — it already overrides the abstract interface method.
fn emit_default_impls_forwarders(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    if c.is_interface {
        return;
    }
    let symbols = env.signature_symbols;
    let classifier = |owner| symbols.classifier(owner);
    let derives_from = |candidate: crate::types::TypeName, ancestor: crate::types::TypeName| {
        let mut pending = vec![candidate];
        let mut seen = std::collections::HashSet::new();
        while let Some(owner) = pending.pop() {
            if !seen.insert(owner) {
                continue;
            }
            let Some(shape) = classifier(owner) else {
                continue;
            };
            for parent in shape.supertypes.iter_ids() {
                if parent == ancestor {
                    return true;
                }
                if classifier(parent).is_some_and(|parent| parent.is_interface()) {
                    pending.push(parent);
                }
            }
        }
        false
    };

    // Read every declaration through the same symbol-source classifier model. Module and classpath
    // providers both expose direct declarations and direct supertypes; this pass neither searches IR
    // classes nor retries a missing class against another origin.
    let mut pending = c.interfaces.iter_ids().collect::<Vec<_>>();
    let mut seen_interfaces = std::collections::HashSet::new();
    let mut closure = Vec::new();
    while let Some(owner) = pending.pop() {
        if !seen_interfaces.insert(owner) {
            continue;
        }
        let Some(shape) = classifier(owner).filter(|shape| shape.is_interface()) else {
            continue;
        };
        pending.extend(
            shape
                .supertypes
                .iter_ids()
                .filter(|parent| classifier(*parent).is_some_and(|parent| parent.is_interface())),
        );
        closure.push((owner, shape));
    }
    closure.sort_by(|(left, _), (right, _)| {
        if derives_from(*left, *right) {
            std::cmp::Ordering::Less
        } else if derives_from(*right, *left) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });

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
            method_key(&function.name, &jvm_tys(&function.params))
        })
        .chain(
            c.bridges
                .iter()
                .map(|bridge| method_key(&bridge.name, &bridge.erased_params)),
        )
        .collect::<std::collections::HashSet<_>>();
    let mut selected = std::collections::HashSet::new();
    let mut write_forwarder = |interface: crate::types::TypeName,
                               name: &str,
                               param_tys: &[Ty],
                               semantic_params: &[Ty],
                               param_names: &[String],
                               ret: Ty,
                               semantic_ret: Ty,
                               target_owner: &str,
                               target_name: &str,
                               target_descriptor: &str| {
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
        let target = cw.methodref(target_owner, target_name, target_descriptor);
        let argument_words = 1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>();
        code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
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
        for (index, parameter) in param_tys.iter().enumerate() {
            let parameter_name = param_names
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("p{index}"));
            locals.push((parameter_name, local_variable_desc(*parameter), slot));
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
        cw.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: target_owner.to_string(),
            outer: Some(interface.render()),
            name: Some("DefaultImpls".to_string()),
            access: 0x0019,
        });
    };
    for (interface, shape) in closure {
        for member in &shape.members {
            let physical_params = if member.physical_params.len() == member.params.len() {
                member.physical_params.clone()
            } else {
                member.params.clone()
            };
            let param_tys = jvm_tys(&physical_params);
            let name = member
                .physical_name
                .as_deref()
                .unwrap_or(member.name.as_str());
            let key = method_key(name, &param_tys);
            // The nearest declaration wins even when it is abstract: an abstract redeclaration
            // suppresses a farther ancestor's body rather than exposing it as a fake override.
            if !selected.insert(key.clone()) {
                continue;
            }
            if member.is_abstract()
                || member.visibility == crate::types::Visibility::Private
                || implemented.contains(&key)
            {
                continue;
            }
            let (target_owner, target_name, holder_desc) = match member.realization {
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true,
                } => {
                    let Some(owner) = member.owner else { continue };
                    (owner.render(), name.to_string(), member.descriptor.clone())
                }
                crate::libraries::MemberRealization::Dispatch
                    if env.jvm_default == JvmDefaultMode::Disable
                        && member.source_member.is_some() =>
                {
                    let mut with_receiver = vec![Ty::obj_name(interface)];
                    with_receiver.extend_from_slice(&param_tys);
                    (
                        crate::types::type_name_nested_child(interface, "DefaultImpls").render(),
                        name.to_string(),
                        method_descriptor(&with_receiver, ir_ty_to_jvm(&member.physical_ret)),
                    )
                }
                _ => continue,
            };
            let ret = ir_ty_to_jvm(&member.physical_ret);
            write_forwarder(
                interface,
                name,
                &param_tys,
                &member.params,
                &member.call_sig.param_names,
                ret,
                member.ret,
                &target_owner,
                &target_name,
                &holder_desc,
            );
            implemented.insert(key);
        }

        // Properties use the same normalized callable handles as functions. They are not members of
        // the source function namespace, so consume the classifier's declared property facets and
        // feed their getter/setter realizations through the identical forwarder writer.
        for callables in shape.declared_callables.values() {
            for property in callables.properties() {
                for (callable, visibility) in
                    std::iter::once((&property.getter, property.visibility)).chain(
                        property
                            .setter
                            .as_ref()
                            .map(|setter| (setter, property.setter_visibility)),
                    )
                {
                    if visibility == crate::types::Visibility::Private {
                        continue;
                    }
                    let params = jvm_tys(&callable.physical_params);
                    let key = method_key(&callable.name, &params);
                    if !selected.insert(key.clone())
                        || callable.is_abstract
                        || implemented.contains(&key)
                    {
                        continue;
                    }
                    let (target_owner, target_descriptor) = match callable.member_realization {
                        crate::libraries::MemberRealization::Direct {
                            pass_receiver: true,
                        } => (callable.owner.render(), callable.descriptor.clone()),
                        crate::libraries::MemberRealization::Dispatch
                            if env.jvm_default == JvmDefaultMode::Disable
                                && property.source_member.is_some() =>
                        {
                            let mut with_receiver = vec![Ty::obj_name(interface)];
                            with_receiver.extend_from_slice(&params);
                            (
                                crate::types::type_name_nested_child(interface, "DefaultImpls")
                                    .render(),
                                method_descriptor(
                                    &with_receiver,
                                    ir_ty_to_jvm(&callable.physical_ret),
                                ),
                            )
                        }
                        _ => continue,
                    };
                    write_forwarder(
                        interface,
                        &callable.name,
                        &params,
                        &callable.params,
                        &property.context_param_names,
                        ir_ty_to_jvm(&callable.physical_ret),
                        callable.ret,
                        &target_owner,
                        &callable.name,
                        &target_descriptor,
                    );
                    implemented.insert(key);
                }
            }
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
    let receiver_signature = formatter.method_ty(&receiver_ty)?;
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
    let param_tys = jvm_tys(&f.params);
    let ret = ir_ty_to_jvm(&f.ret);
    let mut e = Emitter::new(ir, cw, env, owner, facade, ret, [body]);
    // Suspend lowering does not preserve source-local expression IDs.
    e.record_locals = ir.fn_decl_lines.contains_key(&fid) && !ir.suspend_funs.contains(&fid);
    if instance {
        e.slots.insert(0, (0, Ty::obj(owner)));
        e.next_slot = 1;
    }
    for (i, t) in param_tys.iter().enumerate() {
        let vi = i as u32 + if instance { 1 } else { 0 };
        let slot = e.next_slot;
        e.slots.insert(vi, (slot, *t));
        e.next_slot += slot_words(*t);
    }
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
    let signature_formatter = JvmSignatureFormatter::new(env);
    let method_sig = method_signature(&signature_formatter, ir, fid, f);
    let reserved_sig = match holder_receiver {
        Some(receiver) => holder_method_signature(
            &signature_formatter,
            ir,
            receiver,
            method_sig.as_deref(),
            &method_descriptor(&param_tys, ret),
        ),
        None => method_sig,
    };
    let ann_of = |t: Ty| -> Option<&'static str> {
        let d = crate::jvm::names::type_descriptor(t);
        if !(d.starts_with('L') || d.starts_with('[')) {
            return None;
        }
        Some(if matches!(t, Ty::Nullable(_)) {
            "Lorg/jetbrains/annotations/Nullable;"
        } else {
            "Lorg/jetbrains/annotations/NotNull;"
        })
    };
    // A bare type-parameter position erases to `Object` but is NOT a known-non-null reference —
    // kotlinc annotates neither it nor a parameter in that position.
    let gsig = ir.signatures.get(&fid);
    let member_sem = ir.member_semantic_sigs.get(&fid);
    // A LAMBDA IMPL (`<fn>$lambda$N`) is a synthetic realization — kotlinc gives it debug tables
    // but NO nullability annotations.
    let lambda_impl = ir.lambda_own_params_from.contains_key(&fid);
    let reified_body = f
        .body
        .is_some_and(|body| body_has_reified_markers(ir, body));
    let ret_ann = (!lambda_impl
        && !reified_body
        && gsig.is_none_or(|g| !matches!(g.ret, Some(Ty::TyParam(..))))
        && member_sem.is_none_or(|(_, r)| !matches!(r, Ty::TyParam(..))))
    .then(|| ann_of(f.ret))
    .flatten();
    // A parameter's declared `?` lives in a side-table (not in `f.params`, which stays non-null for the
    // mangle); consult it so a nullable reference parameter is annotated `@Nullable`, not `@NotNull`.
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    let param_anns: Vec<Option<&str>> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let is_tparam = gsig.is_some_and(|g| matches!(g.params.get(i), Some(Ty::TyParam(..))))
                || member_sem.is_some_and(|(ps, _)| matches!(ps.get(i), Some(Ty::TyParam(..))));
            if lambda_impl || reified_body || is_tparam {
                None
            } else if declared_nullable
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
            {
                ann_of(Ty::nullable(*t))
            } else {
                ann_of(*t)
            }
        })
        .collect();
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
    // neither `@NotNull` nor `@Nullable`.
    let nullability_annotated = !declared_annotations.deprecated_hidden();
    let ann_types: Vec<&str> = ret_ann
        .into_iter()
        .chain(emitted_param_anns.iter().flatten().copied())
        .filter(|_| nullability_annotated)
        .collect();
    e.cw.reserve_method_pool_with_annotations(
        &f.name,
        &reserved_desc,
        reserved_sig.as_deref(),
        &ann_types,
        &declared_annotations.visible,
        &declared_annotations.invisible,
    );
    let mut code = CodeBuilder::new(e.next_slot);
    // kotlinc guards each non-null reference parameter of a visible function with
    // `Intrinsics.checkNotNullParameter(param, "name")` at method entry — emit the same.
    let param_checks = f.param_checks.clone();
    for (i, check) in param_checks.iter().enumerate() {
        if let Some(name) = check {
            let vi = i as u32 + if instance { 1 } else { 0 };
            if let Some(&(slot, _)) = e.slots.get(&vi) {
                code.aload(slot);
                code.push_string(name, e.cw);
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
            let slot = e.next_slot;
            e.next_slot += 1;
            code.push_int(0, e.cw);
            store(Ty::Int, slot, &mut code);
            (slot, code.bytes.len() as u16)
        });
    e.emit(body, &mut code);
    // The implicit `return` for a `Unit` function is dead code when the body already diverges
    // (`fun foo() { throw … }`): an unreachable `return` after `athrow` has no stack-map frame and
    // the verifier rejects it. Skip it exactly when the body can't fall through.
    if ret == Ty::Unit && !e.diverges(body) {
        // kotlinc maps the implicit `return` to the body's closing-`}` line. `fn_close_lines` has
        // entries only for PARSED declarations, so a synthesized body (no entry) keeps its
        // decl-line fallback table.
        if let Some(&close) = ir.fn_close_lines.get(&fid) {
            code.mark_line(close);
        }
        code.ret_void();
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
        for (i, t) in param_tys.iter().enumerate() {
            let pname = ir
                .param_names(fid)
                .and_then(|ns| ns.get(i).cloned())
                .or_else(|| f.param_checks.get(i).and_then(|n| n.clone()))
                .unwrap_or_else(|| format!("p{i}"));
            let pdesc = local_variable_desc(*t);
            e.cw.seed_utf8(&pname);
            e.cw.seed_utf8(&pdesc);
            code.add_local_entry(0, None, slot, &pname, &pdesc);
            slot += slot_words(*t);
        }
    }
    code.ensure_locals(e.next_slot);
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
            let called_from_lambda_class = env.lambdas == LambdaMode::Class
                && ir.functions[fid as usize].name.contains("$lambda$");
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
    // kotlinc annotates a reference return and each reference parameter of a declared method.
    if nullability_annotated
        && (ret_ann.is_some() || emitted_param_anns.iter().any(Option::is_some))
    {
        e.cw.set_method_nullability(&f.name, &desc, ret_ann, &emitted_param_anns);
    }
    if ir.deprecated_methods.contains(&fid) {
        e.cw.mark_method_deprecated(&f.name, &desc);
    }
    // User annotations declared on the function. A HIDDEN-deprecated declaration additionally gets
    // `ACC_SYNTHETIC`: kotlinc keeps it only for binary compatibility, and a consumer reads both
    // facts (the annotation for resolution, the flag for the JVM) off this realization.
    if let Some(annotations) = ir.function_annotations.get(&fid) {
        e.cw.set_method_annotations(&f.name, &desc, &annotations.visible, &annotations.invisible);
        if annotations.deprecated() {
            e.cw.mark_method_deprecated(&f.name, &desc);
        }
        if annotations.deprecated_hidden() {
            e.cw.set_method_synthetic(&f.name, &desc);
        }
    }
}

/// Format backend-agnostic semantic types into JVM generic-signature elements. The ordinary JVM
/// descriptor and the optional generic `Signature` attribute are separate classfile declarations: the
/// former supplies runtime calling types, while this formatter preserves type parameters, type
/// arguments, and declaration-site variance for classpath readers.
struct JvmSignatureFormatter<'a> {
    symbols: &'a dyn SymbolSource,
    run: &'a EmitRun,
}

impl<'a> JvmSignatureFormatter<'a> {
    fn new(env: &'a EmitEnv<'_>) -> Self {
        Self {
            symbols: env.signature_symbols,
            run: env.run,
        }
    }

    fn declaration_variance(&self, owner: TypeName, index: usize) -> Option<TypeVariance> {
        let Some(classifier) = self.symbols.classifier(owner) else {
            self.run.set_emit_error(format!(
                "internal: JVM signature references classifier '{}' absent from checked symbols",
                owner.render()
            ));
            return None;
        };
        let Some(variance) = classifier.type_param_variances.get(index).copied() else {
            self.run.set_emit_error(format!(
                "internal: JVM signature supplies type argument {} to classifier '{}' with {} type parameters",
                index + 1,
                owner.render(),
                classifier.type_param_variances.len()
            ));
            return None;
        };
        Some(variance)
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
        let Some(classifier) = self.symbols.classifier(owner) else {
            self.run.set_emit_error(format!(
                "internal: JVM wildcard optimization references classifier '{}' absent from checked symbols",
                owner.render()
            ));
            return None;
        };
        let is_closed = match classifier.kind {
            crate::libraries::TypeKind::Object | crate::libraries::TypeKind::Annotation => true,
            crate::libraries::TypeKind::Class => {
                !classifier.inheritance.is_abstract && !classifier.inheritance.is_extensible
            }
            crate::libraries::TypeKind::Enum => !classifier.inheritance.is_extensible,
            crate::libraries::TypeKind::Interface => false,
        };
        if !is_closed {
            return Some(true);
        }
        for (index, argument) in arguments.iter().copied().enumerate() {
            let declaration = self.declaration_variance(owner, index)?;
            let (use_site, argument) = match argument {
                Ty::InProjection(inner) => (TypeVariance::In, *inner),
                Ty::OutProjection(inner) => (TypeVariance::Out, *inner),
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

    fn type_argument(&self, declaration: TypeVariance, argument: Ty) -> Option<String> {
        match argument {
            Ty::InProjection(inner) => Some(format!("-{}", self.ty(inner)?)),
            Ty::OutProjection(inner) => Some(format!("+{}", self.ty(inner)?)),
            argument => {
                let mut signature = String::new();
                if !self.wildcard_is_redundant(declaration, argument)? {
                    match declaration {
                        TypeVariance::In => signature.push('-'),
                        TypeVariance::Out => signature.push('+'),
                        TypeVariance::Invariant => {}
                    }
                }
                signature.push_str(&self.ty(&argument)?);
                Some(signature)
            }
        }
    }

    fn function_ty(&self, signature: &crate::types::FnSig) -> Option<String> {
        let arity = signature.params.len() + usize::from(signature.suspend);
        if arity > 22 {
            self.run.set_emit_error(format!(
                "internal: JVM generic signature cannot represent function arity {arity}"
            ));
            return None;
        }
        let mut rendered = format!("Lkotlin/jvm/functions/Function{arity}<");
        for parameter in &signature.params {
            rendered.push_str(&self.type_argument(TypeVariance::In, *parameter)?);
        }
        if signature.suspend {
            rendered.push_str("-Lkotlin/coroutines/Continuation<-");
            rendered.push_str(&self.ty(&signature.ret)?);
            rendered.push_str(">;");
            rendered.push_str("+Ljava/lang/Object;");
        } else {
            rendered.push_str(&self.type_argument(TypeVariance::Out, signature.ret)?);
        }
        rendered.push_str(">;");
        Some(rendered)
    }

    /// One parameter or return position in a method `Signature`. Positions without generic structure
    /// use their exact JVM descriptor spelling; structured positions are rendered from the semantic
    /// type. This is a structural choice, not a recovery path after semantic formatting failed.
    fn method_ty(&self, ty: &Ty) -> Option<String> {
        let semantic = match ty {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner,
            ty => ty,
        };
        match semantic {
            Ty::TyParam(..) | Ty::Fun(_) => self.ty(ty),
            Ty::Obj(_, arguments) if !arguments.is_empty() => self.ty(ty),
            Ty::InProjection(_) | Ty::OutProjection(_) | Ty::Null | Ty::Error => {
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
    fn ty(&self, ty: &Ty) -> Option<String> {
        if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = ty {
            return self.ty(inner);
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
            Ty::InProjection(inner) => Some(format!("-{}", self.ty(inner)?)),
            Ty::OutProjection(inner) => Some(format!("+{}", self.ty(inner)?)),
            Ty::Fun(signature) => self.function_ty(signature),
            Ty::Obj(owner, arguments) => {
                let internal = owner.render();
                let jvm = crate::jvm::names::classfile_internal_name(&internal);
                let mut signature = format!("L{jvm}");
                if !arguments.is_empty() {
                    signature.push('<');
                    for (index, argument) in arguments.iter().enumerate() {
                        let variance = self.declaration_variance(owner, index)?;
                        signature.push_str(&self.type_argument(variance, *argument)?);
                    }
                    signature.push('>');
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
        s.push_str(&formatter.method_ty(parameter)?);
    }
    s.push(')');
    let ret = g.ret.as_ref().unwrap_or(&f.ret);
    s.push_str(&formatter.method_ty(ret)?);
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
        // formatted from the platform-agnostic `Ty`s so a reader recovers a member's concrete generic return.
        for sup in &g.supers {
            s.push_str(&formatter.ty(sup)?);
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
fn parameterized_sig(formatter: &JvmSignatureFormatter<'_>, ty: &Ty) -> Option<String> {
    let inner = match ty {
        Ty::Nullable(t) | Ty::PlatformNullable(t) => t,
        t => t,
    };
    match inner {
        Ty::Obj(_, args) if !args.is_empty() => {
            let sig = formatter.ty(inner)?;
            (sig != ir_type_desc(inner)).then_some(sig)
        }
        Ty::Fun(_) => formatter.ty(inner),
        _ => None,
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
        return jvm_method_signature(formatter, generic, f);
    }
    if let Some((params, ret)) = ir.member_semantic_sigs.get(&fid) {
        // A member using ENCLOSING-CLASS type parameters signs with bare references (`(TT;)TT;`)
        // and declares nothing — the parameters belong to the class header's own signature.
        if let Some(sig) = member_semantic_signature(params, *ret) {
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
    let ret_arg = formatter.ty(ret)?;
    let mut s = String::from("(");
    for p in params {
        s.push_str(&formatter.method_ty(p)?);
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
        s.push_str(&formatter.method_ty(p)?);
    }
    s.push(')');
    s.push_str(&formatter.method_ty(ret)?);
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
        if is_field.get(i).copied().unwrap_or(true) {
            let f = c.fields.get(field_i);
            let fname = f.map(|f| f.name.as_str()).unwrap_or("");
            if let Some((_, tp)) = ftp.and_then(|ftp| ftp.iter().find(|(fp, _)| fp == fname)) {
                sig.push_str(&format!("T{tp};"));
                any = true;
            } else if let Some(ps) = f.and_then(|f| parameterized_sig(formatter, &f.ty)) {
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
    let real_params = jvm_tys(&f.params);
    let boxed: HashMap<usize, Ty> = ir
        .default_stub_boxed_params
        .get(&fid)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();
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
    let ret = ir_ty_to_jvm(&f.ret);
    let owner_ty = Ty::obj(owner);

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
    e.slots.insert(0, (0, owner_ty));
    let mut slot = 1u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in stub_param_tys.iter().enumerate() {
        e.slots.insert((i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(logical_param_count))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int)); // register so frames type these slots
            slot += 1;
            s
        })
        .collect();
    e.slots.insert(
        9_000_001 + mask_slots.len() as u32,
        (slot, Ty::obj("java/lang/Object")),
    );
    slot += 1;
    e.next_slot = slot;

    let mut code = CodeBuilder::new(slot);
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
        &boxed,
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
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = vec![owner_ty];
    stub_params.extend(stub_param_tys.iter().copied());
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_param_count),
    ));
    stub_params.push(Ty::obj("java/lang/Object"));
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(
        default_stub_access(ir, fid),
        &format!("{method_name}$default"),
        &desc,
        &code,
    );
    // kotlinc gives the synthetic a one-entry LineNumberTable at the function's declaration line.
    if let Some(&line) = ir.fn_decl_lines.get(&fid) {
        e.cw.set_method_lines(&format!("{method_name}$default"), &desc, &[(0, line)]);
    }
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
    boxed: &HashMap<usize, Ty>,
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
            e.frame(skip, vec![], code);
            code.ifeq(skip);
            // The default is computed in the (erased) UNDERLYING form; a slot typed by a nullable-
            // underlying value class boxes it (`box-impl`) so the slot holds the value class.
            e.emit_value(*def_expr, code);
            if let Some(vc) = boxed.get(&(i + recv_offset)) {
                emit_box_impl(e.ir, e.cw, vc, code);
            }
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

/// A value class's (erased) underlying JVM type — its single field's type.
fn vc_underlying_jvm(ir: &IrFile, vc: &Ty) -> Ty {
    vc.obj_internal()
        .and_then(|fq| {
            let fq = fq.render();
            ir.classes.iter().find(|c| c.fq_name_matches(&fq))
        })
        .and_then(|c| c.fields.first())
        .map(|f| ir_ty_to_jvm(&f.ty))
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
    let real_params = jvm_tys(&f.params);
    let ret = ir_ty_to_jvm(&f.ret);
    let recv_offset = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(recv_offset)
        .expect("an extension receiver is a leading physical parameter");
    // kotlinc interns the synthetic's NAME + DESCRIPTOR at its method header, before any constant
    // its body (the delegating call, the default fills) introduces.
    {
        let mask_words = default_mask_count(logical_param_count);
        let stub_desc = method_descriptor(
            &real_params
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
    let mut slot = 0u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots.insert(i as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(logical_param_count))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int)); // register so frames type these slots
            slot += 1;
            s
        })
        .collect();
    e.slots
        .insert(9_000_001 + mask_slots.len() as u32, (slot, marker));
    slot += 1;
    e.next_slot = slot;

    let mut code = CodeBuilder::new(slot);
    // A top-level EXTENSION's registered defaults/names carry a leading `$receiver` slot; the mask
    // bits stay LOGICAL (kotlinc's convention), so slice the receiver prefix off and offset slots.
    emit_default_param_overwrites(
        &mut e,
        &mut code,
        &defaults[recv_offset..],
        recv_offset,
        &param_slots,
        &mask_slots,
        &HashMap::new(),
    );
    // A REIFIED base (its body carries reification markers) cannot be DELEGATED to: the real
    // method throws at runtime and exists only to be inlined. kotlinc's `$default` therefore
    // inlines the whole body after the default fills — emit the same: the body was lowered with
    // value ids 0..n bound to the parameters, exactly this frame's layout.
    if let Some(body) = f.body.filter(|&body| body_has_reified_markers(ir, body)) {
        e.emit(body, &mut code);
        if ret == Ty::Unit && !e.diverges(body) {
            code.ret_void();
        }
    } else {
        for &(pslot, pty) in &param_slots {
            load(pty, pslot, &mut code);
        }
        let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
        let desc = method_descriptor(&real_params, ret);
        let m = e.cw.methodref(facade, &method_name, &desc);
        code.invokestatic(m, aw, slot_words(ret) as i32);
        emit_return(ret, &mut code);
    }
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = real_params.clone();
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
    // kotlinc gives the synthetic a one-entry LineNumberTable at the function's declaration line.
    if let Some(&line) = ir.fn_decl_lines.get(&fid) {
        e.cw.set_method_lines(&format!("{method_name}$default"), &desc, &[(0, line)]);
    }
}

/// Emit the synthetic `<init>(params…, int mask, DefaultConstructorMarker)` overload for a class whose
/// primary constructor has defaulted parameters. Unlike a `$default` method this is a CONSTRUCTOR: `this`
/// is slot 0, the real parameters follow, then the mask + marker; after overwriting each masked slot with
/// its default it `invokespecial`s the real `<init>`. Access is `PUBLIC | SYNTHETIC` (0x1001), matching
/// kotlinc. The defaults were lowered in the instance frame (`this` = value 0, params = 1..=n).
fn emit_ctor_default_stub(
    ir: &IrFile,
    owner: &str,
    facade: &str,
    real_params: &[Ty],
    defaults: &[Option<u32>],
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let n = real_params.len();
    // The FILE facade, not the class. A default initializer is ordinary file-level code that happens
    // to run inside the constructor; same-file top-level calls still belong to the facade.
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        owner,
        facade,
        Ty::Unit,
        defaults.iter().flatten().copied(),
    );
    e.this_uninitialized = true;
    let marker = Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker");
    // `this` at slot 0 = value-index 0; real params at value-index 1..=n.
    e.slots.insert(0, (0, Ty::obj(owner)));
    let mut slot = 1u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots.insert((i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(real_params.len()))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int));
            slot += 1;
            s
        })
        .collect();
    e.slots
        .insert(9_000_001 + mask_slots.len() as u32, (slot, marker));
    slot += 1;
    e.next_slot = slot;

    // The stackmap frame at each mask-branch target: `this` (slot 0) is UNINITIALIZED (the real `<init>`
    // has not run yet), the params keep their types, then the mask ints + marker. Built manually because
    // the frame machinery types slot 0 from `e.slots` as an initialized `Object`, which the verifier rejects.
    let branch_locals: Vec<VerifType> = {
        let mut raw = vec![VerifType::Top; e.next_slot as usize];
        raw[0] = VerifType::UninitializedThis;
        for &(pslot, pty) in &param_slots {
            raw[pslot as usize] = e.verif_single(pty);
        }
        for &mask_slot in &mask_slots {
            raw[mask_slot as usize] = VerifType::Integer;
        }
        raw[slot as usize - 1] = e.verif_single(marker);
        // Collapse the two-slot categories (long/double occupy one verif entry) and trim trailing Top.
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    };
    // kotlinc's `$default` ctor LineNumberTable: the CLASS declaration line at entry, each masked
    // fill's value at its PARAMETER's declaration line, the delegation back at the class line, and
    // the `return` at the primary ctor's closing-`)` line — consecutive same-line entries collapse.
    let class_decl = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name() == owner);
    let class_line = class_decl.map_or(0, |candidate| candidate.decl_line);
    let mut lines: Vec<(u16, u32)> = vec![(0, class_line)];
    let mut code = CodeBuilder::new(slot);
    for (i, def) in defaults.iter().enumerate().take(n) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i];
            code.iload(mask_slots[i / 32]);
            code.push_int(default_mask_bit(i), e.cw);
            code.iand();
            let skip = code.new_label();
            code.add_frame_if_new(skip, branch_locals.clone(), vec![]);
            code.ifeq(skip);
            if let Some(param_line) = class_decl.and_then(|candidate| {
                candidate.fields.get(i).and_then(|field| {
                    ir.prop_decl_lines
                        .get(&(candidate.fq_name_id(), field.name.clone()))
                })
            }) {
                lines.push((code.bytes.len() as u16, *param_line));
            }
            e.emit_value(*def_expr, &mut code);
            store(pty, pslot, &mut code);
            code.bind(skip);
            // The mask/branch machinery for the next slot maps back to the class declaration. The
            // final fill lands at the delegation pc, where the same line is added below and collapsed.
            lines.push((code.bytes.len() as u16, class_line));
        }
    }
    // `invokespecial <owner>.<init>(realparams)V` — delegate to the real primary constructor.
    lines.push((code.bytes.len() as u16, class_line));
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let init_desc = method_descriptor(real_params, Ty::Unit);
    let aw: i32 = 1 + real_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = e.cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    if let Some(&close) =
        class_decl.and_then(|candidate| ir.ctor_close_lines.get(&candidate.fq_name_id()))
    {
        lines.push((code.bytes.len() as u16, close));
    }
    code.ret_void();
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = real_params.to_vec();
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(real_params.len()),
    ));
    stub_params.push(marker);
    let desc = method_descriptor(&stub_params, Ty::Unit);
    e.cw.add_method(0x1001 /* PUBLIC | SYNTHETIC */, "<init>", &desc, &code);
    if class_line != 0 {
        lines.dedup_by_key(|(_, line)| *line);
        e.cw.set_method_lines("<init>", &desc, &lines);
    }
}

/// Emit the PUBLIC|SYNTHETIC accessor `<init>(…args, DefaultConstructorMarker)` for a class whose primary
/// constructor is private (its parameters mention a value class). It delegates straight to the private
/// `<init>` — `this` at slot 0, the real params, then the marker (unused); `invokespecial` the primary,
/// return. Straight-line (no branches ⇒ no StackMapTable). Distinct from the default-arg overload, which
/// carries the extra `int mask` and fills defaults.
fn emit_ctor_marker_accessor(owner: &str, real_params: &[Ty], cw: &mut ClassWriter) {
    let mut slot = 1u16; // slot 0 = `this`
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for t in real_params {
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let total = slot + 1; // + the marker local
                          // The accessor's OWN descriptor interns before its body's Methodref — kotlinc visits a method's
                          // signature before its code, so the private ctor this delegates to must not claim the earlier slot.
    let mut stub_params = real_params.to_vec();
    stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    let desc = method_descriptor(&stub_params, Ty::Unit);
    cw.reserve_descriptor(&desc);
    let mut code = CodeBuilder::new(total);
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let init_desc = method_descriptor(real_params, Ty::Unit);
    let aw: i32 = 1 + real_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    code.ret_void();
    code.ensure_locals(total);
    code.link();

    cw.add_method(0x1001 /* PUBLIC | SYNTHETIC */, "<init>", &desc, &code);
}

/// The target-neutral identity and selected declaration shape shared by JVM property reads and writes.
/// Keeping these fields together prevents the two realizers from accepting subtly different owner,
/// interface, or physical-type inputs as the semantic operation grows more metadata.
struct PropertyOperation<'a> {
    expression: crate::ir::ExprId,
    receiver: crate::ir::ExprId,
    owner: &'a str,
    name: &'a str,
    ty: &'a Ty,
    interface: bool,
    field: Option<&'a crate::libraries::InstanceFieldRef>,
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
    owner: String,
    facade: String,
    slots: HashMap<u32, (u16, Ty)>,
    /// Every `Variable` index → its JVM type (file-wide); a `value_ty(GetValue)` fallback for a slot not
    /// yet registered in `slots` (queried before its declaration emits — e.g. an inline result temp).
    var_types: HashMap<u32, Ty>,
    next_slot: u16,
    ret: Ty,
    /// Stack of enclosing loops' `(continue_label, break_label)` — `break`/`continue` target the top.
    /// Stack of enclosing loops: `(continue_label, break_label, source_label)`. A labeled
    /// `break@l`/`continue@l` targets the entry whose `source_label == Some(l)`; an unlabeled one
    /// targets the innermost (top).
    loop_stack: Vec<(Label, Label, Option<String>)>,
    /// Operand-stack verification types sitting BELOW the expression currently being emitted (an
    /// arithmetic LHS held on the stack across a branchy RHS, e.g. a data-class `hashCode` accumulator
    /// `result*31 + <branchy nullable-field hash>`). Prepended to every recorded stack-map frame's stack
    /// so the pending operand is typed through the branch (matching kotlinc), avoiding the spill-to-temp
    /// krusty would otherwise need. Pushed/popped around the branchy RHS in `emit_binop`.
    pending_stack: Vec<VerifType>,
    /// Open source locals: `(block_depth, slot, start_pc, name, descriptor)`.
    open_locals: Vec<(usize, u16, u16, String, String)>,
    /// Current block nesting depth; the function body is depth 1.
    block_depth: usize,
    /// Whether this method records source-local debug entries.
    record_locals: bool,
    /// Slot 0 remains the verifier's special uninitialized receiver until the constructor delegates.
    this_uninitialized: bool,
    /// `-Xlambdas`: `Class` replaces each `invokedynamic` with a synthesized class instantiation.
    lambdas: LambdaMode,
}

fn parse_descriptor_params(desc: &str) -> Option<Vec<Ty>> {
    parse_physical_method_desc(desc).map(|(params, _)| params)
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
        Self {
            ir,
            cw,
            bodies: env.bodies,
            run: env.run,
            jvm_default: env.jvm_default,
            owner: owner.to_string(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir, roots),
            next_slot: 0,
            ret,
            loop_stack: Vec::new(),
            pending_stack: Vec::new(),
            open_locals: Vec::new(),
            block_depth: 0,
            record_locals: false,
            this_uninitialized: false,
            lambdas: env.lambdas,
        }
    }

    /// Emit a lambda's `inline_body` (its value-producing form) INLINE at a stdlib-inline-fn splice:
    /// bind its parameter value-indices `0..` to the given JVM slots (captures → caller slots, lambda
    /// params → the on-stack args), then emit the body as a value — leaving the result on the stack. A
    /// user `return` inside the body emits a real `*return` from the enclosing method, i.e. a correct
    /// non-local return (no synthetic-return rewriting needed).
    fn emit_fn_body_inline(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
        code: &mut CodeBuilder,
    ) {
        let saved_slots = std::mem::take(&mut self.slots);
        for (i, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(i as u32, (slot, ty));
        }
        self.emit_value(inline_body, code);
        self.slots = saved_slots;
    }

    /// THE unified host+lambda splice (the merge of the branchy and lambda paths): splice a possibly
    /// BRANCHY host `inline fun` body, replacing each zero-arg lambda-parameter `Function0.invoke` site
    /// with that lambda's body. Handles `require(cond) { msg }` / `check(cond) { msg }` and the like —
    /// where the lambda runs only on a branch. v1: zero-arg (Function0) lambdas with branchless bodies,
    /// at an empty operand-stack baseline. Returns `false` (caller falls back / skips) on any other shape.
    fn try_inline_unified(
        &mut self,
        descriptor: &str,
        args: &[u32],
        body: &crate::jvm::classreader::MethodCode,
        base: u16,
        code: &mut CodeBuilder,
        reified: &HashMap<String, String>,
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
        let top_local = base + body.max_locals;
        self.next_slot = self.next_slot.max(top_local);
        // Build each lambda argument's pre-relocated body (leaving its boxed result on the stack), and
        // its own (branchy-predicate) frames — resolved to byte offsets within the body, relocated below.
        let mut lam_splices: Vec<crate::jvm::inline::LambdaSplice> = Vec::new();
        let mut lam_frames: Vec<ResolvedFrames> = Vec::new();
        // The deepest operand stack any spliced lambda body reaches — the host's `max_stack` must cover it,
        // since the body is inlined into the host (a deep lambda body, e.g. `123 != intArrayOf() as Any`,
        // would otherwise overflow the host's stack). Propagated to `splice_inline` below.
        let mut lam_max_stack = 0u16;
        for (i, &a) in args.iter().enumerate() {
            let mut scratch = CodeBuilder::new(self.next_slot);
            let (lam_insns, lam_fr) = if let IrExpr::Lambda {
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
                let cap_tys = jvm_tys(&impl_f.params[..n_cap]);
                let lam_tys = jvm_tys(&impl_f.params[n_cap..]);
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
                let lam_semantic_ret = semantic_signature
                    .as_ref()
                    .map(|(_, ret)| *ret)
                    .unwrap_or(impl_f.ret);
                crate::trace_compiler!(
                    "splice",
                    "inline lambda expression={a} impl={impl_fn} impl_params={:?} semantic={semantic_signature:?}",
                    impl_f.params
                );
                let impl_ret = ir_ty_to_jvm(&impl_f.ret);
                // Each capture binds to the caller's actual slot (a mutable capture writes through).
                let mut cap_slots: Vec<(u16, Ty)> = Vec::with_capacity(captures.len());
                let mut converted_captures: Vec<(u32, u16, Ty)> = Vec::new();
                for (k, &cap) in captures.iter().enumerate() {
                    let slot = if let IrExpr::GetValue(v) = self.ir.expr(cap) {
                        let Some(&(slot, _)) = self.slots.get(v) else {
                            return false;
                        };
                        slot
                    } else {
                        // Materialize the pure conversion inserted at a value-class capture boundary.
                        let converted_get = match self.ir.expr(cap) {
                            IrExpr::Call {
                                callee: Callee::Virtual { name, .. },
                                dispatch_receiver: Some(cast),
                                args,
                            } if name == "unbox-impl" && args.is_empty() => {
                                matches!(
                                    self.ir.expr(*cast),
                                    IrExpr::TypeOp { arg, .. }
                                        if matches!(self.ir.expr(*arg), IrExpr::GetValue(_))
                                )
                            }
                            _ => false,
                        };
                        if !converted_get {
                            return false;
                        }
                        let slot = self.next_slot;
                        self.next_slot += slot_words(cap_tys[k]);
                        converted_captures.push((cap, slot, cap_tys[k]));
                        slot
                    };
                    cap_slots.push((slot, cap_tys[k]));
                }
                // Build the lambda body into a scratch builder. The host left the lambda's `arity`
                // arguments on the stack (as `Object`, the erased `FunctionN.invoke` parameters);
                // unbox a primitive parameter, or `checkcast` a specific reference parameter to its
                // type, then store it (top = last). Then run the body, then box the result to `Object`
                // (matching the replaced `invoke`'s `Object` result).
                scratch.set_stack(arity as u16);
                for (capture, slot, ty) in converted_captures {
                    self.emit_value(capture, &mut scratch);
                    store(ty, slot, &mut scratch);
                }
                let mut param_slots: Vec<(u16, Ty)> = cap_slots;
                param_slots.extend(std::iter::repeat_n((0u16, Ty::Error), arity));
                for j in (0..arity).rev() {
                    let jt = lam_tys[j];
                    if jt.is_jvm_scalar() {
                        // `FunctionN.invoke` hands every argument over as `Object`. Select its adapter
                        // from the lambda's semantic parameter before using the physical carrier for
                        // the local slot; otherwise `UInt` is mistaken for boxed `Int` here.
                        unbox_prim(
                            self.cw,
                            &mut scratch,
                            semantic_scalar_adapter(lam_semantic_tys[j], jt),
                        );
                    } else if let Some(internal) = checkcast_internal(jt) {
                        let ci = self.cw.class_ref(&internal);
                        scratch.checkcast(ci);
                    }
                    let slot = self.next_slot;
                    self.next_slot += slot_words(jt);
                    store(jt, slot, &mut scratch);
                    param_slots[n_cap + j] = (slot, jt);
                }
                self.emit_fn_body_inline(inline_body, &param_slots, &mut scratch);
                if impl_ret.is_jvm_scalar() {
                    // The erased `invoke` result is `Object`, so reverse the same semantic adapter
                    // choice after the inline body leaves its physical carrier on the stack.
                    box_prim_free(
                        self.cw,
                        &mut scratch,
                        semantic_scalar_adapter(lam_semantic_ret, impl_ret),
                    );
                }
                scratch.link(); // patch the lambda body's own branch operands before reading its bytes
                let lam_fr = scratch.resolved_frames(); // branchy predicate body → its own frames
                let Some(lam_insns) = crate::jvm::inline::disassemble(&scratch.bytes) else {
                    return false;
                };
                (lam_insns, lam_fr)
            } else {
                continue;
            };
            if code.max_locals < scratch.max_locals {
                code.max_locals = scratch.max_locals;
            }
            self.next_slot = self.next_slot.max(scratch.max_locals);
            lam_max_stack = lam_max_stack.max(scratch.max_stack);
            lam_frames.push(lam_fr);
            lam_splices.push(crate::jvm::inline::LambdaSplice {
                param_index: i,
                body: lam_insns,
            });
        }
        if lam_splices.is_empty() {
            return false; // no lambda argument — not this path
        }
        // Probe at offset 0 to learn whether frames are needed (HOST branchy OR any lambda BODY branchy).
        let Some(probe) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            0,
            self.cw,
            reified,
        ) else {
            return false;
        };
        // The splice records frames if it has a join, any lambda body has frames, OR the HOST body itself
        // records frames (a loop HOF's loop frames). All of these are bound relative to an empty operand
        // baseline (no caller operand prefix is threaded into them), so a non-empty baseline must bail —
        // `records_frame` makes a parent operand sequence spill earlier operands so we reach here at 0.
        let needs_frames = probe.join_required
            || !probe.frames.is_empty()
            || lam_frames.iter().any(|f| !f.is_empty());
        if needs_frames && code.stack_height() != 0 {
            crate::trace_compiler!(
                "splice",
                "unified BAIL: needs_frames but stack_height={}",
                code.stack_height()
            );
            return false; // frames carry no stack prefix → need an empty baseline
        }
        let ret_words = descriptor_ret_words(descriptor);
        // Emit each NON-lambda argument (the operands the host prologue stores into its parameter slots).
        let mut arg_words = 0i32;
        for (i, &a) in args.iter().enumerate() {
            if lam_splices.iter().any(|splice| splice.param_index == i) {
                continue;
            }
            self.emit_value(a, code);
            let at = self.value_ty(a);
            if params[i].is_reference() && at.is_jvm_scalar() {
                box_prim_free(self.cw, code, at);
            }
            arg_words += slot_words(params[i]) as i32;
        }
        if !needs_frames {
            // Pure branchless host + lambda: append the bytes, no frames; works at any stack height.
            // The host's stack must cover the host body PLUS the deepest spliced lambda body (a safe upper
            // bound on the real peak) — else a deep lambda body overflows the host's operand stack.
            code.splice_inline(
                &probe.bytes,
                body.max_stack + lam_max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // RE-splice at the real method offset (so any switch in the host/lambda body pads correctly), then
        // bind the relocated HOST frames, the LAMBDA bodies' own frames, the spliced bytes, and the join.
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
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        for (k, frames) in lam_frames.iter().enumerate() {
            let host_ctx = bs.lambda_host_locals.get(k).cloned().unwrap_or_default();
            // The lambda body's frames were compiled against an EMPTY operand base; rebase each onto the
            // host operand-stack prefix sitting below the lambda value (e.g. a `map` destination). Empty
            // for `forEach`/`fold`/`takeIf`; `splice_unified` only returns `Some` here for a branchy body.
            let op_prefix: Vec<VerifType> = bs
                .lambda_stack_prefix
                .get(k)
                .and_then(|p| p.as_ref())
                .map(|p| p.iter().map(vtype_to_verif).collect())
                .unwrap_or_default();
            for (fb, locals, stack) in frames {
                let off = bs.lambda_byte_starts[k] + fb;
                let merged = self.merge_lambda_frame_locals(base, top_local, &host_ctx, locals);
                let mut st = op_prefix.clone();
                st.extend(stack.iter().cloned());
                let l = code.new_label();
                code.bind_at(l, off);
                code.add_frame_if_new(l, merged, st);
            }
        }
        // Register the spliced body's relocated exception handlers (try/catch/finally from `use`/
        // `synchronized`/`runCatching`). The handler frames are already bound above (each handler is a
        // StackMapTable target in `bs.frames`); here we add the guarded-range entries to the caller's
        // exception table via labels bound at the absolute spliced offsets.
        for &(start, end, handler, catch_type) in &bs.handlers {
            let (ls, le, lh) = (code.new_label(), code.new_label(), code.new_label());
            code.bind_at(ls, start);
            code.bind_at(le, end);
            code.bind_at(lh, handler);
            code.add_exception(ls, le, lh, catch_type);
        }
        code.set_needs_stackmap();
        // Host stack must cover the host body PLUS the deepest spliced lambda body (safe upper bound).
        code.splice_inline(
            &bs.bytes,
            body.max_stack + lam_max_stack,
            top_local,
            arg_words,
            ret_words,
        );
        if bs.join_required {
            let join = code.new_label();
            code.bind(join);
            let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
            code.add_frame_if_new(join, prefix, join_stack);
        }
        true
    }

    /// Full locals for a frame INSIDE a spliced lambda body: the caller's locals (`0..base`), then the
    /// HOST's live body locals at the invoke (`host_ctx`, slots `base..` — for a loop host the loop
    /// iterator/accumulator, not just params), then the lambda's own slots (`top_local..`) from its
    /// scratch frame. All three are slot-expanded, overlaid, and re-collapsed.
    fn merge_lambda_frame_locals(
        &mut self,
        base: u16,
        top_local: u16,
        host_ctx: &[crate::jvm::inline::VType],
        lam_locals: &[VerifType],
    ) -> Vec<VerifType> {
        let mut slots = self.verif_slots_upto(base); // 0..base caller locals (slot-indexed)
                                                     // The host's live locals at `base..` (slot-indexed), then pad to `top_local` with `Top`.
        let host_collapsed: Vec<VerifType> = host_ctx.iter().map(vtype_to_verif).collect();
        slots.extend(expand_collapsed_locals(&host_collapsed));
        slots.truncate(top_local as usize);
        while slots.len() < top_local as usize {
            slots.push(VerifType::Top);
        }
        // The lambda's own slots (`top_local..`): expand the scratch frame, take from `top_local`.
        for s in expand_collapsed_locals(lam_locals)
            .into_iter()
            .skip(top_local as usize)
        {
            slots.push(s);
        }
        collapse_locals(&slots)
    }

    /// Slot-indexed caller locals for `0..upto` (long/double take two slots; `Top` fills the gaps).
    fn verif_slots_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
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

    /// Attempt to splice a cross-module `inline fun`'s compiled body at the call site (the bytecode
    /// inliner; the callee body comes from [`MethodBodies::body`]). Returns `true` if spliced; `false`
    /// means the caller must report an inline backend gap rather than silently treating this as an
    /// ordinary call-resolution fallback.
    /// The reified type substitution (type-parameter name → JVM internal name) for the value expression
    /// `e` being emitted, from [`IrFile::reified_call_subst`]. Empty for a call that isn't a
    /// `<reified T>` classpath-extension splice — the common case. Fed to `splice_unified` so a
    /// `reifiedOperationMarker`/`T::class` in the spliced body specializes to the concrete type.
    fn reified_type_map(&self, e: u32) -> HashMap<String, String> {
        self.ir
            .reified_call_subst
            .get(&e)
            .map(|subst| {
                subst
                    .iter()
                    .filter_map(|(name, ty)| {
                        // `kotlin_class_internal` (not `obj_internal`): a reified type arg inferred from a
                        // receiver arrives as a bare `Ty::Int`/`Ty::String` variant whose `obj_internal()`
                        // is `None` — the boxed reified array element is `java/lang/Integer` etc.
                        let internal = ty.kotlin_class_internal()?.render();
                        let internal = crate::jvm::jvm_class_map::to_jvm_internal(&internal);
                        Some((name.clone(), internal.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Splice `owner.name` whose REAL (body-fetch) descriptor is `descriptor`, mapping the body's locals
    /// per `splice_desc`. For an ordinary static they are equal; for an INSTANCE inline method spliced
    /// through this path, `splice_desc` PREPENDS the receiver as the first parameter (`this` = local 0)
    /// and `args[0]` is that receiver — so the body's `aload_0`/`aload_1`/… map to receiver/params.
    fn try_inline_static_as(
        &mut self,
        target: InlineStaticTarget<'_>,
        args: &[u32],
        code: &mut CodeBuilder,
        allow_owner_bridge: bool,
        reified: &HashMap<String, String>,
    ) -> bool {
        let InlineStaticTarget {
            owner,
            name,
            descriptor,
            splice_desc,
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
            &mut |o, n, d| self.bodies.member_is_private(o, n, d),
        ) {
            crate::trace_compiler!(
                "splice",
                "private member reference in {owner}.{name}{descriptor}"
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
        let base = self.next_slot.max(code.max_locals);
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
            // If the body INVOKES the lambda parameter (`FunctionN.invoke`), splice the lambda body at
            // those sites. If the lambda is used only as a VALUE — passed to a call/constructor, as in the
            // `Continuation(ctx){…}` fake-constructor's `new …$Continuation$1(ctx, resumeWith)` — there is
            // no invoke site to splice into, so fall through to MATERIALIZE the lambda as a `Function1`
            // object (`emit_operands`) and splice the body verbatim (the param slot binds to that object).
            let body_invokes_lambda =
                crate::jvm::inline::disassemble(&body.code).is_some_and(|insns| {
                    !crate::jvm::inline::function_invoke_sites(&insns, &body.source_cp).is_empty()
                });
            if body_invokes_lambda {
                return self.try_inline_unified(descriptor, args, &body, base, code, reified);
            }
        }
        // A function-typed parameter whose argument isn't a literal lambda (a passed `Function`
        // value, `t?.let(x)`) needs NO invoke-site substitution: the verbatim splice below binds the
        // param slot to the materialized `Function` object, and the body's own
        // `FunctionN.invoke` interface calls dispatch on it — exactly kotlinc's inlined shape.
        let ret_words = descriptor_ret_words(descriptor);
        let top_local = base + body.max_locals;
        // ONE splicer for every no-lambda body (`splice_unified` subsumes the old branchless + branchy
        // paths). Probe at offset 0 to learn `join_required` (a branchless body has no switch, so its
        // layout is position-independent); a branchy body is then RE-spliced at its real method offset so
        // any `tableswitch`/`lookupswitch` pads correctly.
        let Some(probe) =
            crate::jvm::inline::splice_unified(&body, splice_desc, base, &[], 0, self.cw, reified)
        else {
            crate::trace_compiler!(
                "splice",
                "splice_unified probe failed for {owner}.{name}{descriptor} (splice_desc={splice_desc})"
            );
            return false;
        };
        let arg_words: i32 = args
            .iter()
            .map(|&a| slot_words(self.value_ty(a)) as i32)
            .sum();
        if !probe.join_required {
            // Branchless: append the bytes, no frames. A DIVERGING body (ends in `athrow`, e.g.
            // `error(msg)`) leaves NOTHING on the stack — its post-splice height is the baseline.
            self.emit_operands(args, code);
            let diverges = probe.bytes.last() == Some(&0xbf);
            let ret_words = if diverges { 0 } else { ret_words };
            code.splice_inline(
                &probe.bytes,
                body.max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // Branchy body: needs an empty operand-stack baseline (the relocated frames carry no stack
        // prefix); a sub-expression inline call (non-empty stack) falls back to a normal call.
        if code.stack_height() != 0 {
            return false;
        }
        self.emit_operands(args, code);
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            &body,
            splice_desc,
            base,
            &[],
            splice_start,
            self.cw,
            reified,
        ) else {
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        code.set_needs_stackmap();
        code.splice_inline(&bs.bytes, body.max_stack, top_local, arg_words, ret_words);
        // Join frame: the redirected returns land at the continuation right after the spliced body.
        let join = code.new_label();
        code.bind(join);
        let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
        code.add_frame_if_new(join, prefix, join_stack);
        true
    }

    /// Caller-local verification types for slots `0..upto` (collapsing `long`/`double` to one entry),
    /// NOT trimming trailing `Top` — the prefix a spliced branchy body's frames are concatenated onto
    /// (the body's own locals occupy slots `upto..`).
    fn verif_locals_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        if self.this_uninitialized && !raw.is_empty() {
            raw[0] = VerifType::UninitializedThis;
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        out
    }

    fn emit(&mut self, e: u32, code: &mut CodeBuilder) {
        match self.ir.expr(e).clone() {
            IrExpr::Block { stmts, value } => {
                // Scope block-locals: restore the slot *map* after the block (keeping next_slot
                // monotonic) so a local declared here doesn't leak into a later merge-point frame
                // (its slot must read as `Top` once out of scope — else a sibling branch that never
                // initialized it fails verification).
                let saved = self.slots.clone();
                self.block_depth += 1;
                let mut dead = false;
                for s in stmts {
                    // A statement root carrying a source line starts a `LineNumberTable` entry at
                    // its first instruction (kotlinc's per-statement mapping).
                    if let Some(&l) = self.ir.expr_lines.get(&s) {
                        code.mark_line(l);
                    }
                    // See the value-context `Block` arm: a statement nets zero, so reset the tracked
                    // height afterward to undo an approximate branchy-splice drift.
                    let base = code.stack_height();
                    self.emit(s, code);
                    if self.diverges(s) {
                        dead = true;
                        break;
                    } // rest of the block is unreachable
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        // A trailing value is a statement in source terms — start its
                        // `LineNumberTable` entry like one.
                        if let Some(&l) = self.ir.expr_lines.get(&v) {
                            code.mark_line(l);
                        }
                        self.emit_discarding(v, code);
                    }
                }
                self.close_scope_locals(code);
                self.block_depth -= 1;
                self.slots = saved;
            }
            IrExpr::Return(v) => match v {
                Some(v) => {
                    let ret = self.ret;
                    self.emit_value_as(v, &ret, code);
                    // `return <diverging>` (`return throw e`, `return error(..)`): the value already
                    // transferred control (athrow / a `Nothing`-returning call), so the trailing return
                    // opcode is unreachable dead code the verifier rejects (no stack-map frame). Skip it.
                    if !self.diverges(v) {
                        emit_return(self.ret, code);
                    }
                }
                None => code.ret_void(),
            },
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                // Emit the initializer BEFORE allocating the slot, so the variable's slot isn't
                // claimed in StackMapTable frames recorded inside a branchy initializer (where the
                // verifier still sees it as `top`).
                let jt = ir_ty_to_jvm(&ty);
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
                if let Some(i) = init {
                    self.emit_value(i, code);
                    emit_num_conv(self.value_ty(i), jt, code);
                    let slot = reuse.unwrap_or_else(|| {
                        let s = self.next_slot;
                        self.next_slot += slot_words(jt);
                        s
                    });
                    self.slots.insert(index, (slot, jt));
                    store(jt, slot, code);
                    // A source local becomes visible after its initializing store.
                    if let Some(name) = self.ir.value_names.get(&e).filter(|_| self.record_locals) {
                        if code.bytes.len() <= u16::MAX as usize {
                            self.open_locals.push((
                                self.block_depth,
                                slot,
                                code.bytes.len() as u16,
                                name.clone(),
                                local_variable_desc(jt),
                            ));
                        }
                    }
                } else {
                    let slot = reuse.unwrap_or_else(|| {
                        let s = self.next_slot;
                        self.next_slot += slot_words(jt);
                        s
                    });
                    self.slots.insert(index, (slot, jt));
                }
            }
            IrExpr::SetValue { var, value } => {
                let Some(&(slot, jt)) = self.slots.get(&var) else {
                    self.run.set_emit_error(
                        "assignment references a value slot that was never declared".to_string(),
                    );
                    return;
                };
                // `i = i + k` / `i = k + i` / `i = i - k` on an `Int` local with a small constant `k`
                // compiles to `iinc slot, k` (kotlinc's form), not load/const/add/store.
                let delta: Option<i32> = if jt == Ty::Int {
                    if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(value) {
                        let cint = |e: u32| match self.ir.expr(e) {
                            IrExpr::Const(IrConst::Int(k)) => Some(*k),
                            _ => None,
                        };
                        let isvar =
                            |e: u32| matches!(self.ir.expr(e), IrExpr::GetValue(v) if *v == var);
                        match op {
                            IrBinOp::Add if isvar(lhs) => cint(rhs),
                            IrBinOp::Add if isvar(rhs) => cint(lhs),
                            IrBinOp::Sub if isvar(lhs) => cint(rhs).map(|k| -k),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match delta {
                    Some(d) if (-128..=127).contains(&d) => code.iinc(slot, d as i8),
                    _ => {
                        self.emit_value(value, code);
                        emit_num_conv(self.value_ty(value), jt, code);
                        store(jt, slot, code);
                    }
                }
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => {
                let c = &self.ir.classes[class as usize];
                let name = instance_field_jvm_name(self.ir, c, &c.fields[index as usize]);
                let fty = c.fields[index as usize].ty.clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name();
                if static_storage(self.ir, c) {
                    // A static-storage object field: no instance operand (evaluate the receiver only
                    // for its effects), and a branchy value runs on an already-clean stack.
                    if !matches!(self.ir.expr(receiver), crate::ir::IrExpr::GetValue(_)) {
                        self.emit_value(receiver, code);
                        code.pop();
                    }
                    self.emit_value(value, code);
                    let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                    code.putstatic(fref, slot_words(jt) as i32);
                } else {
                    // A branchy value emits a merge frame; with the receiver already on the stack the
                    // verifier sees a non-empty baseline it can't reconcile (krusty's frames carry no stack
                    // prefix). Spill the value to a temp first — its branches then run on a clean stack —
                    // then load the receiver and the temp. (Plain values keep the direct receiver,value order.)
                    if self.records_frame(value) {
                        let temps = self.spill_to_temps(&[value], code);
                        self.emit_value(receiver, code);
                        let (slot, t, key) = temps[0];
                        load(t, slot, code);
                        self.slots.remove(&key);
                    } else {
                        self.emit_value(receiver, code);
                        self.emit_value(value, code);
                    }
                    let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                    code.putfield(fref, slot_words(jt) as i32);
                }
            }
            IrExpr::SetStatic { index, value } => {
                let s = &self.ir.statics[index as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                self.emit_value(value, code);
                // Within the facade write the field directly; from another class go through `setX()` —
                // or, for a PRIVATE top-level property (no public setter), the `access$set<X>$p` bridge.
                let private = self.ir.statics[index as usize].visibility.is_private();
                // A static declaring an OWNER lives on that class (a companion property is a static
                // field on the outer class). Within the owner write the (private) field directly;
                // from another class — the companion's delegating setter — go through the owner's
                // PUBLIC synthetic `access$set<X>$cp` bridge (kotlinc's hoisted-companion shape).
                if let Some(owner) = self.ir.statics[index as usize].owner {
                    let owner_name = owner.render();
                    if self.owner == owner_name || !self.ir.is_jvm_companion_hoisted_static(index) {
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
                self.frame(start, vec![], code);
                code.bind(start);
                // A pre-test loop checks the condition before the body; a `do…while` skips this and
                // tests at the bottom (`cont`), so the body always runs once.
                if !post_test && self.emit_cond_branch(cond, end, false, code) {
                    // `while (false)`: the jump-out is unconditional, so the body/update/back-edge
                    // that would follow are unreachable — emitting them leaves frameless dead code
                    // the verifier rejects. kotlinc emits no body for a never-entered loop either.
                    self.frame(end, vec![], code);
                    code.bind(end);
                    return;
                }
                // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
                self.loop_stack.push((cont, end, label.clone()));
                self.emit(body, code);
                // The body block restored the slot map, so framing `cont`/`start` here captures the
                // loop's outer locals — a `continue` jumping in from a deeper scope stays compatible.
                self.frame(cont, vec![], code);
                code.bind(cont);
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
                } else {
                    self.frame(start, vec![], code);
                    code.goto(start);
                }
                self.frame(end, vec![], code);
                code.bind(end);
            }
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(&label);
                code.goto(end);
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(&label);
                code.goto(cont);
            }
            other => {
                self.emit_discarding_node(e, &other, code);
            }
        }
    }

    /// Close locals declared in the current nested block.
    fn close_scope_locals(&mut self, code: &mut CodeBuilder) {
        if self.block_depth <= 1 {
            return;
        }
        let end = code.bytes.len().min(u16::MAX as usize) as u16;
        let depth = self.block_depth;
        let mut i = 0;
        while i < self.open_locals.len() {
            if self.open_locals[i].0 >= depth {
                let (_, slot, start, name, desc) = self.open_locals.remove(i);
                code.add_local_entry(start, Some(end.saturating_sub(start)), slot, &name, &desc);
            } else {
                i += 1;
            }
        }
    }

    fn emit_discarding(&mut self, e: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(e).clone();
        self.emit_discarding_node(e, &node, code);
    }

    fn emit_discarding_node(&mut self, e: u32, node: &IrExpr, code: &mut CodeBuilder) {
        self.emit_value_node(e, node, code);
        // A `Nothing`-returning call leaves a physical `Void` and must terminate the path (it would
        // otherwise fall through with a stray value); the throw replaces the discard.
        if self.terminate_if_nothing_call(e, node, code) {
            return;
        }
        // A successfully spliced bottom-typed expression has already transferred control (for
        // example, an inline lambda's non-local `return`). It leaves no value to discard. The
        // semantic type is retained on the IR expression even when the selected callable's physical
        // descriptor returns `Object`.
        if self.diverges(e) {
            return;
        }
        discard(self.value_ty(e), code);
    }

    fn emit_value(&mut self, e: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(e).clone();
        self.emit_value_node(e, &node, code);
        self.terminate_if_nothing_call(e, &node, code);
    }

    /// Emit `e` and then narrow it to the CONSUMPTION type `expected` — the `checkcast` kotlinc inserts
    /// when a value out of an ERASED slot (a type parameter's `Object`, a generic `Array<T>`'s `Object[]`)
    /// flows to a more specific reference (a `return`/argument/receiver of that type). Keyed on the value's
    /// ACTUAL physical type: a concrete source (already the target, or an unrelated concrete type such as a
    /// value class's unboxed underlying) is left alone — the backend owns this erasure decision.
    fn emit_value_as(&mut self, e: u32, expected: &Ty, code: &mut CodeBuilder) {
        self.emit_value(e, code);
        let src = self.value_ty(e);
        self.narrow_on_stack(src, expected, code);
    }

    /// Narrow the value on top of the stack (whose actual type is `src`) to the CONSUMPTION type
    /// `expected` — the `checkcast` kotlinc inserts when an ERASED value (a type parameter's `Object`, a
    /// generic `Array<T>`'s `Object[]`) flows to a more specific reference. Keyed on `src`: a concrete
    /// source (already the target, or an unrelated concrete type such as a value class's unboxed
    /// underlying) is left alone.
    /// Realize `IrExpr::PropertyRead` — the one place that decides what reading a Kotlin property
    /// COMPILES to. The owner's class file names the accessor or field (via `@Metadata`'s
    /// `JvmPropertySignature`, so a `@JvmName` or value-class-mangled spelling is honoured, never guessed)
    /// and says whether it takes a receiver. A class this compilation is still emitting has no class file
    /// to ask, so it falls back to the JVM convention kotlinc itself follows: `get<Name>()`.
    fn emit_property_read(&mut self, operation: PropertyOperation<'_>, code: &mut CodeBuilder) {
        use crate::jvm::inline::PropertyAccess;
        let receiver_ty = self.value_ty(operation.receiver);
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
        let array_realization = jvm_array_actual_realization(
            crate::types::type_name(operation.owner),
            operation.name,
            receiver_ty,
            &[],
            declaration_ty,
        );
        crate::trace_compiler!(
            "emit",
            "property read owner={} name={} receiver={receiver_ty:?} declaration={:?} array_realization={array_realization:?}",
            operation.owner,
            operation.name,
            declaration_ty,
        );
        if array_realization == Some(JvmArrayActualRealization::Size) {
            self.emit_value(operation.receiver, code);
            code.arraylength();
            return;
        }
        // An exact field identity wins before any accessor realization.
        if operation.field.is_some() {
            let (access, exact_field) = self
                .selected_local_property_read_access(
                    operation.owner,
                    operation.name,
                    operation.field,
                )
                .expect("a selected property field must have a JVM realization");
            return self.emit_realized_property_read(
                operation.receiver,
                access,
                operation.ty,
                exact_field,
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
                operation.receiver,
                access,
                operation.ty,
                false,
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
                operation.receiver,
                access,
                operation.ty,
                false,
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
        self.emit_realized_property_read(operation.receiver, access, operation.ty, false, code)
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
            .declared_property_write_access(operation.owner, operation.name)
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
        // A branchy assigned value emits merge frames. It cannot do so with an instance receiver already
        // on the operand stack because those frames describe an empty baseline. Spill BOTH operands in
        // source evaluation order (receiver, then value), then reload them; spilling only the value would
        // reverse observable side effects.
        let spilled = if takes_receiver && self.records_frame(value) {
            Some(self.spill_to_temps(&[operation.receiver, value], code))
        } else {
            None
        };
        if let Some(temps) = &spilled {
            let (slot, receiver_ty, _) = temps[0];
            load(receiver_ty, slot, code);
            self.narrow_on_stack(receiver_ty, &Ty::obj(&access_owner), code);
        } else {
            let receiver_ty = accessor_receiver_ty(&access, &access_owner);
            self.emit_property_receiver(
                operation.receiver,
                &access_owner,
                takes_receiver,
                &receiver_ty,
                code,
            );
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
            for &(_, _, key) in temps {
                self.slots.remove(&key);
            }
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
            unbox_prim(
                self.cw,
                code,
                semantic_scalar_adapter(*operation.ty, target),
            );
        } else {
            self.narrow_on_stack(source, &target, code);
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
                code.invokestatic(m, words, 0);
            }
        }
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
            self.narrow_on_stack(self.value_ty(receiver), expected, code);
            return;
        }
        // A receiverless realization does not make the receiver expression disappear. Elide only an
        // expression that runs no code, or a singleton/static read of the very owner whose static access
        // initializes it anyway; every other receiver is evaluated and popped.
        let initializes_owner = match self.ir.expr(receiver) {
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
        let direct_field = self.direct_field_access(owner, declared, true);
        if let Some(declared) = declared.filter(|p| p.needs_access_bridge && self.owner != owner) {
            let ty = declared
                .backing_field
                .and_then(|i| class.fields.get(i as usize))
                .map_or(declared.ty, |f| f.ty);
            let d = type_descriptor(ir_ty_to_jvm(&ty));
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
                descriptor: method_descriptor(&[ir_ty_to_jvm(&f.params[0])], Ty::Unit),
                is_static: false,
                is_interface: class.is_interface,
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
            descriptor: method_descriptor(&[ir_ty_to_jvm(&f.params[0])], Ty::Unit),
            is_static: false,
            is_interface: class.is_interface,
        };
        if let Some(setter) = setter.filter(|_| !direct_field || field.is_none()) {
            return Some(accessor(setter));
        }
        let field = field?;
        if !direct_field {
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: setter_name,
                descriptor: method_descriptor(&[ir_ty_to_jvm(&field.ty)], Ty::Unit),
                is_static: false,
                is_interface: class.is_interface,
            });
        }
        Some(PropertyAccess::Field {
            owner: owner.to_string(),
            name: instance_field_jvm_name(self.ir, class, field),
            descriptor: type_descriptor(ir_ty_to_jvm(&field.ty)),
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
        let interface = class.is_interface || selected_interface;
        // A property that DECLARES an accessor (computed, delegated, or `field`-using) is always read
        // through it — the accessor is user code, and a direct field load would skip it. Only a plain
        // backing-field property may be read directly, and only from inside the declaring class.
        let declared = class.properties.iter().find(|p| p.name == name);
        let direct_field = self.direct_field_access(owner, declared, false);
        if let Some(getter) = declared.and_then(|p| p.getter) {
            let f = &self.ir.functions[getter as usize];
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: f.name.clone(),
                descriptor: method_descriptor(&f.params, ir_ty_to_jvm(&f.ret)),
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
        let accessor_name = declared
            .and_then(|p| p.getter_jvm_name.clone())
            .or_else(|| selected_accessor.map(str::to_string))
            .unwrap_or_else(|| crate::names::property_getter_name(name));
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
            (named && f.params.is_empty()).then_some(f)
        });
        if let Some(accessor) = accessor.filter(|_| !direct_field || field.is_none()) {
            return Some(PropertyAccess::Accessor {
                owner: owner.to_string(),
                name: accessor.name.clone(),
                descriptor: method_descriptor(&[], ir_ty_to_jvm(&accessor.ret)),
                is_static: false,
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
                descriptor: format!("(L{owner};){}", type_descriptor(ir_ty_to_jvm(&ty))),
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
                descriptor: method_descriptor(&[], ir_ty_to_jvm(&stored_value_ty(ty))),
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
                        .unwrap_or_else(|| ir_ty_to_jvm(&field.ty)),
                ),
                is_static: false,
                is_interface: interface,
            });
        }
        Some(PropertyAccess::Field {
            owner: owner.to_string(),
            name: instance_field_jvm_name(self.ir, class, field),
            descriptor: type_descriptor(ir_ty_to_jvm(&field.ty)),
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
    fn direct_field_access(
        &self,
        owner: &str,
        declared: Option<&crate::ir::IrProperty>,
        writable: bool,
    ) -> bool {
        self.owner == owner
            && !declared.is_some_and(|p| p.is_open && !p.is_private && (!writable || p.is_var))
    }

    /// Select the property-read realization available directly from semantic IR: an exact field target
    /// recorded by resolution wins, otherwise a declaration emitted by this compilation supplies its own
    /// field/accessor/bridge realization. `None` deliberately means the external bytecode-provider path
    /// must decide. The boolean records the exact-field case because its erased descriptor may require a
    /// post-read narrowing cast. Both bytecode emission and frame analysis consume this helper; duplicating
    /// the precedence here would let them disagree about whether the inline `lateinit` guard exists.
    fn selected_local_property_read_access(
        &self,
        owner: &str,
        name: &str,
        field: Option<&crate::libraries::InstanceFieldRef>,
    ) -> Option<(crate::jvm::inline::PropertyAccess, bool)> {
        use crate::jvm::inline::PropertyAccess;
        if let Some(field) = field {
            return Some((
                PropertyAccess::Field {
                    owner: field.owner.render(),
                    name: field.name.clone(),
                    descriptor: field.descriptor.clone(),
                    is_static: false,
                },
                true,
            ));
        }
        self.declared_property_read_access(owner, name, None, false)
            .map(|access| (access, false))
    }

    /// Is `owner.name` a `lateinit` backing field of a class THIS compilation is emitting? Only such a
    /// field carries the inline uninitialized guard, so the read emission and [`Self::records_frame`]
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
    fn lateinit_direct_field_read(
        &self,
        owner: &str,
        name: &str,
        field: Option<&crate::libraries::InstanceFieldRef>,
    ) -> bool {
        use crate::jvm::inline::PropertyAccess;
        let Some((access, _)) = self.selected_local_property_read_access(owner, name, field) else {
            return false;
        };
        matches!(&access, PropertyAccess::Field { owner, name, .. }
            if self.is_lateinit_field(owner, name))
    }

    /// Emit one already-chosen realization of a property read: push the receiver (or drop it, when the
    /// realization takes none), perform the field load or accessor call, and bridge the physical result to
    /// the property read's Kotlin type.
    fn emit_realized_property_read(
        &mut self,
        receiver: crate::ir::ExprId,
        access: crate::jvm::inline::PropertyAccess,
        ty: &Ty,
        exact_field: bool,
        code: &mut CodeBuilder,
    ) {
        use crate::jvm::inline::PropertyAccess;
        let access_owner = match &access {
            PropertyAccess::Field { owner, .. }
            | PropertyAccess::Accessor { owner, .. }
            | PropertyAccess::AccessBridge { owner, .. } => owner.clone(),
        };
        let takes_receiver = accessor_takes_receiver(&access);
        let receiver_ty = accessor_receiver_ty(&access, &access_owner);
        self.emit_property_receiver(receiver, &access_owner, takes_receiver, &receiver_ty, code);
        let physical = match access {
            PropertyAccess::Field {
                owner,
                name,
                descriptor,
                is_static,
            } => {
                let jt = ty_from_field_descriptor(&descriptor);
                let lateinit = self.is_lateinit_field(&owner, &name);
                let fref = self.cw.fieldref(&owner, &name, &descriptor);
                if is_static {
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    code.getfield(fref, slot_words(jt) as i32);
                }
                // A `lateinit var` read throws while the field is still null, wherever it is read from.
                if lateinit {
                    code.dup();
                    let lbl = code.new_label();
                    code.ifnonnull(lbl);
                    code.push_string(&name, self.cw);
                    let m = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(m, 1, 0);
                    let st = self.verif_stack(jt);
                    self.frame(lbl, st, code);
                    code.bind(lbl);
                }
                jt
            }
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static,
                is_interface,
            } => {
                // A `void` accessor (a `Unit` property) leaves NOTHING on the stack — `descriptor_ret_words`
                // is the authority on that, since `ty_from_descriptor_ret` maps `V` to a 1-word `Unit` for
                // type flow. Nothing is left, so there is nothing to bridge.
                let words = descriptor_ret_words(&descriptor);
                let m = if is_interface {
                    self.cw.interface_methodref(&owner, &name, &descriptor)
                } else {
                    self.cw.methodref(&owner, &name, &descriptor)
                };
                if is_static {
                    code.invokestatic(m, 0, words);
                } else if is_interface {
                    code.invokeinterface(m, 0, words);
                } else {
                    code.invokevirtual(m, 0, words);
                }
                if words == 0 {
                    return;
                }
                ty_from_descriptor_ret(&descriptor)
            }
            PropertyAccess::AccessBridge {
                owner,
                name,
                descriptor,
            } => {
                // The receiver is already on the stack as the bridge's sole argument.
                let words = descriptor_ret_words(&descriptor);
                let m = self.cw.methodref(&owner, &name, &descriptor);
                code.invokestatic(m, 1, words);
                if words == 0 {
                    return;
                }
                ty_from_descriptor_ret(&descriptor)
            }
        };
        // The realization's result is the PHYSICAL one — erased to `Object` for a type parameter, a bare
        // primitive for an `Int` property. The node's `ty` is the logical Kotlin type the read has at this
        // site (`Int?` in a safe-call chain). Bridge the two exactly as any other physical result is
        // bridged: box, unbox, or narrow.
        let logical = ir_ty_to_jvm(&stored_value_ty(*ty));
        if physical.is_jvm_scalar() && !logical.is_jvm_scalar() && logical.is_reference() {
            box_prim_free(self.cw, code, semantic_scalar_adapter(*ty, physical));
        } else if !physical.is_jvm_scalar() && logical.is_jvm_scalar() {
            // `ty` is the substituted semantic result and `logical` its JVM carrier. Choosing the
            // adapter from `logical` alone turns `UInt` into `Integer`; retain the semantic type until
            // after the `Object` boundary has been bridged.
            unbox_prim(self.cw, code, semantic_scalar_adapter(*ty, logical));
        } else if exact_field
            && physical.is_reference()
            && logical.is_reference()
            && type_descriptor(physical) != type_descriptor(logical)
        {
            // A generic Java field's descriptor erases to its formal bound (`CharSequence` for
            // `T : CharSequence`), while this applied read may be `String`. Preserve the selected
            // field descriptor for `getfield`, then narrow its result to the logical binding.
            let internal = ref_internal(logical);
            if internal != "java/lang/Object" {
                let class = self.cw.class_ref(&internal);
                code.checkcast(class);
            }
        } else if !self.is_value_class_ty(ty) {
            // A value class has no runtime type of its own — its values ARE the erased underlying — so
            // narrowing to one would `checkcast` to a class the value is not an instance of.
            self.narrow_on_stack(physical, ty, code);
        }
    }

    /// Whether `ty` names a `@JvmInline value class`, whose values are represented as their underlying.
    fn is_value_class_ty(&self, ty: &Ty) -> bool {
        ty.non_null().obj_internal().is_some_and(|fq_name| {
            self.ir
                .classes
                .iter()
                .any(|c| c.is_value && c.fq_name == fq_name)
                || self.ir.has_external_value_class_name(fq_name)
        })
    }

    fn narrow_on_stack(&mut self, src: Ty, expected: &Ty, code: &mut CodeBuilder) {
        let s = ir_ty_to_jvm(&src);
        if !jvm_is_erased_top(s) {
            return;
        }
        let exp = ir_ty_to_jvm(expected);
        if !exp.is_reference() || type_descriptor(s) == type_descriptor(exp) {
            return;
        }
        let internal = ref_internal(exp);
        if internal != "java/lang/Object" {
            let ci = self.cw.class_ref(&internal);
            code.checkcast(ci);
        }
    }

    /// A `Nothing`-returning REAL-invoke call (`exit(): Nothing`) physically leaves a `java/lang/Void`
    /// on the stack and falls through — unlike `throw`/`return`, which terminate. kotlinc makes the path
    /// truly diverge: discard the `Void`, then `throw KotlinNothingValueException()`. Mirror that so a
    /// `Nothing` call used in a branch (`if (c) … else exit()`, a diverging `catch`) terminates instead of
    /// leaking a `Void` into the merge/handler frame. Inline-spliced `Nothing` calls (`error(...)`) already
    /// end in `athrow` and are excluded. Returns whether the terminating throw was emitted.
    fn terminate_if_nothing_call(
        &mut self,
        expression: u32,
        node: &IrExpr,
        code: &mut CodeBuilder,
    ) -> bool {
        let declared_nothing = self.is_real_nothing_call(node);
        let inferred_nothing = self.is_real_call(node)
            && self
                .ir
                .logical_types
                .get(&expression)
                .is_some_and(|ty| !ty.is_nullable() && ty.non_null() == Ty::Nothing);
        if !declared_nothing && !inferred_nothing {
            return false;
        }
        // The invoke was emitted with ZERO result words — `slot_words(Nothing)` is 0 because a
        // `Nothing` call yields no VALUE — yet it physically leaves one `Void` word. Re-declare that
        // word before discarding it: otherwise the tracked height sits one below the real stack from
        // the invoke onwards, `max_stack` is undercounted by whatever this path pushes on top
        // (`println(boom())` needs the `PrintStream` receiver underneath), and the JVM rejects the
        // method with "Operand stack overflow".
        if declared_nothing {
            code.set_stack((code.stack_height().max(0) + 1) as u16);
        }
        code.pop();
        let cls = self.cw.class_ref("kotlin/KotlinNothingValueException");
        code.new_obj(cls);
        code.dup();
        let ctor = self
            .cw
            .methodref("kotlin/KotlinNothingValueException", "<init>", "()V");
        code.invokespecial(ctor, 0, 0);
        code.athrow();
        true
    }

    /// A call that physically returns (real `invoke`, leaving a `java/lang/Void`) yet is typed `Nothing`.
    /// Excludes inline-spliced (`error`/`require`) and intrinsic callees, which already end
    /// the path in `athrow` and leave nothing to discard.
    fn is_real_nothing_call(&self, node: &IrExpr) -> bool {
        match node {
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                ret_is_nothing(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(fid)
                | Callee::LocalDefault(fid)
                | Callee::ClassStatic { function: fid, .. } => {
                    ret_is_nothing(&self.ir.functions[*fid as usize].ret)
                }
                Callee::CrossFile { ret, .. } => ret_is_nothing(ret),
                Callee::Special { descriptor, .. } => descriptor.ends_with(")Ljava/lang/Void;"),
                Callee::Virtual {
                    descriptor, params, ..
                } => match params {
                    Some((_, ret)) => ret_is_nothing(ret),
                    None => descriptor.ends_with(")Ljava/lang/Void;"),
                },
                Callee::Static {
                    descriptor, inline, ..
                } => !inline.can_inline() && descriptor.ends_with(")Ljava/lang/Void;"),
                Callee::Intrinsic { .. } => false,
            },
            _ => false,
        }
    }

    /// Whether this node emitted a real invocation that physically returns one reference word.
    /// Inline/intrinsic calls realize their own control flow and must not receive a synthetic throw.
    fn is_real_call(&self, node: &IrExpr) -> bool {
        match node {
            IrExpr::MethodCall { .. } => true,
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(_)
                | Callee::LocalDefault(_)
                | Callee::ClassStatic { .. }
                | Callee::CrossFile { .. } => true,
                Callee::Special { .. } | Callee::Virtual { .. } => true,
                Callee::Static { inline, .. } => !inline.can_inline(),
                Callee::Intrinsic { .. } => false,
            },
            _ => false,
        }
    }

    /// Where a non-virtual call to an interface member must go under `-jvm-default=disable`.
    ///
    /// `super.f()` and a call to a private interface member both push the receiver first and then
    /// `invokespecial` the interface. Under `disable` the interface holds no body, so the call has to
    /// become `invokestatic <Iface>$DefaultImpls.f(LIface;…)` — the receiver already on the stack is
    /// exactly the holder static's parameter 0. Returns `None` when the call should stay as it is.
    fn holder_call(
        &self,
        owner: &str,
        descriptor: &str,
        current_source_body: bool,
    ) -> Option<(String, String)> {
        if self.jvm_default != JvmDefaultMode::Disable || !current_source_body {
            return None;
        }
        let holder_descriptor = descriptor
            .strip_prefix('(')
            .map(|rest| format!("(L{owner};{rest}"))?;
        Some((format!("{owner}$DefaultImpls"), holder_descriptor))
    }

    fn emit_value_node(&mut self, e: u32, node: &IrExpr, code: &mut CodeBuilder) {
        match node {
            // `break`/`continue` are `Nothing`-typed: in value position (e.g. `x ?: break`) they diverge
            // — emit the jump and push nothing; the consuming branch is dead past this point.
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(label);
                code.goto(end);
                return;
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(label);
                code.goto(cont);
                return;
            }
            IrExpr::Const(c) => match c {
                IrConst::Boolean(b) => code.push_int(if *b { 1 } else { 0 }, self.cw),
                IrConst::Int(v) => code.push_int(*v, self.cw),
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
                field,
                operation,
            } => {
                let (receiver, owner, name, ty, interface, field, operation) = (
                    *receiver,
                    owner.render(),
                    name.clone(),
                    *ty,
                    *interface,
                    field.as_deref(),
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
                        field,
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
                        field: None,
                    },
                    value,
                    code,
                );
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                let c = &self.ir.classes[*class as usize];
                let source_name = c.fields[*index as usize].name.clone();
                let name = instance_field_jvm_name(self.ir, c, &c.fields[*index as usize]);
                let fty = c.fields[*index as usize].ty.clone();
                let jt = ir_ty_to_jvm(&fty);
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
                    // At the join the field value (non-null on the taken path) is on the stack.
                    let st = self.verif_stack(jt);
                    self.frame(lbl, st, code);
                    code.bind(lbl);
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
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name();
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                code.getfield(fref, slot_words(jt) as i32);
            }
            IrExpr::GetStatic(i) => {
                let s = &self.ir.statics[*i as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                // A static declaring an OWNER lives on that class, not the facade. Within the owner
                // read the (private) field directly; from any other class — the companion's
                // delegating accessors — go through the owner's PUBLIC synthetic `access$get<X>$cp`
                // bridge, kotlinc's hoisted-companion-property access shape.
                if let Some(owner) = self.ir.statics[*i as usize].owner {
                    let owner_name = owner.render();
                    if self.owner == owner_name || !self.ir.is_jvm_companion_hoisted_static(*i) {
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
            } => {
                let owner = internal.render();
                let args = args.clone();
                // The constructor descriptor + its argument-word count come from ONE source, identified by
                // the owner NAME (no same-file/other-file/classpath control-flow split):
                //  - a verbatim descriptor (`ctor_desc`) for a classpath ctor whose signature isn't modeled
                //    as `Ty`s — arg words come from each argument's own value type; OR
                //  - the known parameter types: the node's `ctor_params`, else the named in-IR class's
                //    primary-ctor field types.
                let (desc, use_accessor) = if let Some(d) = ctor_desc {
                    (d.clone(), false)
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
                    // PUBLIC|SYNTHETIC accessor `(…args, DefaultConstructorMarker)`. Construction from
                    // ANOTHER class routes through the accessor (a trailing `null`) — JVM `private` is
                    // a per-CLASS boundary (independent of file/package), so the test is `self.owner !=
                    // owner`. Same-class construction (a secondary ctor, `box-impl`) keeps the primary.
                    // A SECONDARY ctor with value-class params has the same private+marker ABI —
                    // the checker-selected `ctor_params` identify it by erased shape.
                    let vc_secondary = ctor_params.as_ref().is_some_and(|ps| {
                        let want = jvm_tys(ps);
                        self.ir
                            .class_id_by_name(*internal)
                            .map(|cid| &self.ir.classes[cid as usize])
                            .is_some_and(|target| {
                                target
                                    .secondary_ctors
                                    .iter()
                                    .any(|sc| sc.vc_params && jvm_tys(&sc.params) == want)
                            })
                    });
                    let use_accessor = self.owner != owner
                        && ((ctor_params.is_none() && self.ir.has_value_param_ctor(&owner))
                            || vc_secondary);
                    if use_accessor {
                        field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                    }
                    (method_descriptor(&field_tys, Ty::Unit), use_accessor)
                };
                let physical_params =
                    parse_descriptor_params(&desc).expect("constructor descriptor must be valid");
                let aw = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack — its merge frame
                    // would omit them. Evaluate all args into temps first (clean stack), then build.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for (i, &(slot, t, _)) in temps.iter().enumerate() {
                        load(t, slot, code);
                        self.adapt_physical_operand(t, physical_params[i], code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for (i, &a) in args.iter().enumerate() {
                        self.emit_value(a, code);
                        self.adapt_physical_operand(self.value_ty(a), physical_params[i], code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
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
                let param_tys = jvm_tys(&f.params);
                let ret = ir_ty_to_jvm(&f.ret);
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
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            Some(a) => {
                                self.emit_value(*a, code);
                                if let Some(vc) = boxed.get(&i) {
                                    emit_box_impl(self.ir, self.cw, vc, code);
                                }
                            }
                            None => {
                                push_zero(stub_param_tys[i], code, self.cw);
                                let li = i
                                    .checked_sub(recv_offset)
                                    .expect("an extension receiver cannot be omitted");
                                masks[li / 32] |= default_mask_bit(li);
                            }
                        }
                    }
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
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                    return;
                }
                let call_args: Vec<u32> = args.iter().map(|a| a.unwrap()).collect();
                // An argument-count/descriptor mismatch can only come from a pass that rewrote the
                // callee's ABI without fixing this call site (a suspend call the coroutine flattener
                // failed to thread a continuation into — an unmodeled shape). Never emit the
                // unverifiable call: bail the file (the gate SKIPS it), pushing a typed zero so the
                // dead code that follows still assembles.
                if call_args.len() != param_tys.len() {
                    crate::trace_compiler!(
                        "emit",
                        "call arity mismatch for {owner}.{name} ({} args vs {} params)",
                        call_args.len(),
                        param_tys.len()
                    );
                    self.run.set_inline_bail("call arity mismatch");
                    if ret != Ty::Unit {
                        push_zero(ret, code, self.cw);
                    }
                    return;
                }
                self.emit_virtual_operands(&owner, *receiver, &call_args, code);
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&param_tys, ret);
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
                    if let Some((holder, holder_desc)) = is_iface
                        .then(|| self.holder_call(&owner, &desc, true))
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        code.invokestatic(m, aw + 1, slot_words(ret) as i32);
                    } else {
                        let m = if is_iface {
                            self.cw.interface_methodref(&owner, &name, &desc)
                        } else {
                            self.cw.methodref(&owner, &name, &desc)
                        };
                        code.invokespecial(m, aw, slot_words(ret) as i32);
                    }
                } else if is_iface {
                    // Dispatch through an interface — `invokeinterface I.m`.
                    let m = self.cw.interface_methodref(&owner, &name, &desc);
                    code.invokeinterface(m, aw, slot_words(ret) as i32);
                } else {
                    let m = self.cw.methodref(&owner, &name, &desc);
                    code.invokevirtual(m, aw, slot_words(ret) as i32);
                }
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => match callee {
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys = jvm_tys(&f.params);
                    let ret = ir_ty_to_jvm(&f.ret);
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
                    // Same arity/descriptor net as `MethodCall` above: an unthreaded suspend call
                    // (the CPS transform appended a `Continuation` param this site never passes)
                    // must bail the file, never emit an unverifiable call.
                    if args.len() != param_tys.len() {
                        crate::trace_compiler!(
                            "emit",
                            "call arity mismatch for {}.{name} ({} args vs {} params)",
                            self.facade,
                            args.len(),
                            param_tys.len()
                        );
                        self.run.set_inline_bail("call arity mismatch");
                        if ret != Ty::Unit {
                            push_zero(ret, code, self.cw);
                        }
                        return;
                    }
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::ClassStatic { owner, function } => {
                    let f = &self.ir.functions[*function as usize];
                    let param_tys = jvm_tys(&f.params);
                    let ret = ir_ty_to_jvm(&f.ret);
                    if args.len() != param_tys.len() {
                        crate::trace_compiler!(
                            "emit",
                            "class-static call arity mismatch for {}.{} ({} args vs {} params)",
                            owner,
                            f.name,
                            args.len(),
                            param_tys.len()
                        );
                        self.run.set_inline_bail("call arity mismatch");
                        if ret != Ty::Unit {
                            push_zero(ret, code, self.cw);
                        }
                        return;
                    }
                    self.emit_operands(args, code);
                    let argument_words: i32 =
                        param_tys.iter().map(|ty| slot_words(*ty) as i32).sum();
                    let descriptor = method_descriptor(&param_tys, ret);
                    let owner = owner.render();
                    let method = if self.bodies.owner_is_interface(&owner) {
                        self.cw.interface_methodref(&owner, &f.name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &f.name, &descriptor)
                    };
                    code.invokestatic(method, argument_words, slot_words(ret) as i32);
                }
                Callee::LocalDefault(fid) => {
                    // The `foo$default(realparams, mask..., Object marker)` synthetic on the self facade
                    // (emitted by `emit_facade_default_stub`). Args already include mask words + marker.
                    let f = &self.ir.functions[*fid as usize];
                    let mut param_tys = jvm_tys(&f.params);
                    let recv_offset = usize::from(self.ir.extension_receiver_fns.contains(fid));
                    let logical_param_count = f
                        .params
                        .len()
                        .checked_sub(recv_offset)
                        .expect("an extension receiver is a leading physical parameter");
                    param_tys.extend(std::iter::repeat_n(
                        Ty::Int,
                        default_mask_count(logical_param_count),
                    ));
                    param_tys.push(Ty::obj("java/lang/Object"));
                    let ret = ir_ty_to_jvm(&f.ret);
                    let name = format!("{}$default", f.name);
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Intrinsic { operation, .. } => match operation {
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
                        self.emit_string_plus(dispatch_receiver.unwrap(), args[0], code)
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
                    crate::ir::IrIntrinsic::PrimitiveArrayNew { element } => {
                        self.emit_value(args[0], code);
                        code.newarray(prim_newarray_atype(*element));
                    }
                },
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                } => {
                    // A top-level function from another file → `invokestatic <facade>.<name>(desc)`.
                    let param_tys = jvm_tys(params);
                    let ret = ir_ty_to_jvm(ret);
                    let (facade, name) = (facade.render(), name.clone());
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    // A static method declared on an INTERFACE (`@Serializable(with=X) interface I` whose
                    // synthetic `serializer()` is static) needs an InterfaceMethodref constant, even for
                    // `invokestatic` (else `IncompatibleClassChangeError`).
                    let owner_is_interface = self
                        .ir
                        .classes
                        .iter()
                        .any(|c| c.fq_name_matches(&facade) && c.is_interface);
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
                    code.invokestatic(m, aw, slot_words(ret) as i32);
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
                    let reified = self.reified_type_map(e);
                    // `@InlineOnly`/non-public inline functions must splice. Public inline functions have
                    // callable bytecode, so a failed optional splice can fall back to a real call. An
                    // ordinary `$default` synthetic is an ABI dispatcher whose mask prologue must run
                    // as emitted; only a call carrying a reified substitution is splice-only.
                    if inline.can_inline() && (!name.ends_with("$default") || !reified.is_empty()) {
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
                            };
                            self.try_inline_static_as(target, &all, code, true, &reified)
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
                            };
                            self.try_inline_static_as(
                                target,
                                &args,
                                code,
                                inline.must_inline() || has_lambda_arg,
                                &reified,
                            )
                        };
                        if spliced {
                            return;
                        }
                        // A `@InlineOnly` (`must_inline`) callee has no callable body — a failed splice
                        // must bail. So must a REIFIED inline (non-empty reified substitution): its
                        // compiled body carries a `reifiedOperationMarker` and throws
                        // `throwUndefinedForReified` when invoked directly, so a direct-call fallback is a
                        // miscompile, not a legal call. Bail (skip the file) instead.
                        if inline.must_inline() || !reified.is_empty() {
                            crate::trace_compiler!(
                                "emit",
                                "inline splice failed for {owner}.{name}{descriptor}"
                            );
                            self.run.set_inline_bail("inline splice failed");
                        }
                    }
                    let physical_params = parse_descriptor_params(&descriptor)
                        .expect("static call descriptor must be valid");
                    let physical_args = match dispatch_receiver.as_ref() {
                        Some(&recv) if physical_params.len() == args.len() + 1 => {
                            let mut all = Vec::with_capacity(args.len() + 1);
                            all.push(recv);
                            all.extend(args.iter().copied());
                            all
                        }
                        _ => args,
                    };
                    self.emit_descriptor_operands(&physical_args, &physical_params, code);
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
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
                            self.emit_descriptor_operands(&operands, &physical_params, code);
                            let argument_words: i32 = physical_params
                                .iter()
                                .map(|ty| slot_words(*ty) as i32)
                                .sum();
                            let physical_ret = ty_from_descriptor_ret(&realization.descriptor);
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
                        let interface = *interface;
                        let ptys = jvm_tys(param_tys);
                        let ret = ir_ty_to_jvm(ret_ty);
                        let descriptor = method_descriptor(&ptys, ret);
                        let mut ops = vec![recv];
                        ops.extend(args.iter().copied());
                        self.emit_operands(&ops, code);
                        let aw: i32 = ptys.iter().map(|t| slot_words(*t) as i32).sum();
                        if interface {
                            let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                            code.invokeinterface(m, aw, slot_words(ret) as i32);
                        } else {
                            let m = self.cw.methodref(&owner, &name, &descriptor);
                            code.invokevirtual(m, aw, slot_words(ret) as i32);
                        }
                        return;
                    }
                    let (owner, name, descriptor, interface) =
                        (owner.render(), name.clone(), descriptor.clone(), *interface);
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
                        self.emit_descriptor_operands(&args, &physical_params, code);
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let ret = ty_from_descriptor_ret(&descriptor);
                        let m = self.cw.methodref(&owner, &name, &descriptor);
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
                        self.emit_descriptor_operands(&physical_args, &physical_params, code);
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let ret = ty_from_descriptor_ret(&descriptor);
                        let m = self.cw.methodref(&owner, &name, &descriptor);
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
                    self.emit_descriptor_virtual_operands(
                        &owner,
                        recv,
                        &args,
                        &physical_params,
                        code,
                    );
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let jvm_name = crate::jvm::names::mapped_builtin_virtual_name(&owner, &name);
                    if interface {
                        let m = self.cw.interface_methodref(&owner, jvm_name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, jvm_name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                    interface,
                    source_member,
                } => {
                    let (owner, name, descriptor, interface) =
                        (owner.render(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("special call needs a receiver");
                    let args = args.clone();
                    let physical_params = parse_descriptor_params(&descriptor)
                        .expect("special call descriptor must be valid");
                    self.emit_descriptor_virtual_operands(
                        &owner,
                        recv,
                        &args,
                        &physical_params,
                        code,
                    );
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    // A diamond `super.f()` to a superinterface DEFAULT method: `invokespecial` on an
                    // `InterfaceMethodref` (JVM allows a direct-superinterface default this way) —
                    // unless `disable` moved that body to the holder, where it is a plain static.
                    if let Some((holder, holder_desc)) = interface
                        .then(|| self.holder_call(&owner, &descriptor, source_member.is_some()))
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        code.invokestatic(m, aw + 1, slot_words(ret) as i32);
                    } else {
                        let m = if interface {
                            self.cw.interface_methodref(&owner, &name, &descriptor)
                        } else {
                            self.cw.methodref(&owner, &name, &descriptor)
                        };
                        code.invokespecial(m, aw, slot_words(ret) as i32);
                    }
                }
            },
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => {
                // A primitive target of `instanceof`/`checkcast` (`x is Int`) tests the boxed wrapper.
                let jvm_ty = ir_ty_to_jvm(type_operand);
                let internal = if jvm_ty.is_jvm_scalar() {
                    crate::jvm::jvm_class_map::wrapper_internal(jvm_ty)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| ref_internal(jvm_ty))
                } else {
                    ref_internal(jvm_ty)
                };
                crate::trace_compiler!(
                    "value_classes",
                    "emit type op={op:?} arg={arg} {:?} arg_ty={:?} operand={type_operand:?} jvm={jvm_ty:?} internal={internal}",
                    self.ir.expr(*arg),
                    self.value_ty(*arg),
                );
                if let IrExpr::Block { stmts, value } = self.ir.expr(*arg) {
                    crate::trace_compiler!(
                        "value_classes",
                        "type op block arg={arg} stmts={:?} value={:?}",
                        stmts
                            .iter()
                            .map(|&expression| (expression, self.ir.expr(expression)))
                            .collect::<Vec<_>>(),
                        value.map(|expression| (expression, self.ir.expr(expression))),
                    );
                }
                self.emit_value(*arg, code);
                match op {
                    IrTypeOp::InstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                    }
                    IrTypeOp::NotInstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                        code.push_int(1, self.cw);
                        code.ixor();
                    }
                    IrTypeOp::Cast => {
                        // The emitter owns erasure: a `checkcast` to `java/lang/Object` (an unbounded `as T`)
                        // is a no-op, and so is one whose target descriptor already equals the value's
                        // actual (physical) descriptor — an erasure-narrowing tag where the value is already
                        // that type (`List<T>` read tagged `List<Int>`). kotlinc emits neither.
                        let physical_arg = self
                            .ir
                            .physical_types
                            .get(arg)
                            .copied()
                            .unwrap_or_else(|| self.value_ty(*arg));
                        let redundant = type_descriptor(physical_arg) == type_descriptor(jvm_ty);
                        if internal != "java/lang/Object" && !redundant {
                            let ci = self.cw.class_ref(&internal);
                            code.checkcast(ci);
                        }
                    }
                    IrTypeOp::CastNonNull => {
                        // Null-check (throws on null) then checkcast — matching kotlinc's `as T`.
                        let kotlin_name = match type_operand.non_null() {
                            Ty::Obj(fq_name, _) => fq_name.replace('/', "."),
                            Ty::TyParam(name, _) => {
                                crate::types::type_parameter_source_name(name).to_string()
                            }
                            _ => "kotlin.Any".to_string(),
                        };
                        code.dup();
                        code.push_string(
                            &format!("null cannot be cast to non-null type {kotlin_name}"),
                            self.cw,
                        );
                        let m = self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNull",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
                        code.invokestatic(m, 2, 0);
                        // Erased bound `java/lang/Object` (an `<T : Any>` cast) needs no `checkcast`.
                        if internal != "java/lang/Object" {
                            let ci = self.cw.class_ref(&internal);
                            code.checkcast(ci);
                        }
                    }
                    // Box a primitive into a reference target, unbox a wrapper into a primitive, or
                    // widen/narrow between primitive numeric types (`Int`→`Long`, `Double`→`Int`, …).
                    IrTypeOp::ImplicitCoercion => {
                        let at = self.value_ty(*arg);
                        let target = ir_ty_to_jvm(type_operand);
                        crate::trace_compiler!(
                            "value_classes",
                            "coerce at={at:?} target={target:?} type_operand={type_operand:?}"
                        );
                        if at.is_jvm_scalar() && target.is_reference() {
                            let semantic = self.ir.logical_types.get(arg).copied().unwrap_or(at);
                            box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, at));
                        } else if at.is_reference() && target.is_jvm_scalar() {
                            // `type_operand` is the semantic target selected before JVM erasure.
                            // It alone owns wrapper identity (`UInt`), while `target` supplies only
                            // the physical carrier (`int`). Using the already-erased stack type here
                            // can mix `checkcast Integer` with `UInt.unbox-impl`, which is invalid.
                            unbox_prim(
                                self.cw,
                                code,
                                semantic_scalar_adapter(*type_operand, target),
                            );
                        } else if at.is_jvm_scalar() && target.is_jvm_scalar() && at != target {
                            emit_num_conv(at, target, code);
                        } else if at.is_reference()
                            && target.is_reference()
                            && type_descriptor(at) != type_descriptor(target)
                        {
                            let internal = ref_internal(target);
                            if internal != "java/lang/Object" {
                                let class = self.cw.class_ref(&internal);
                                code.checkcast(class);
                            }
                        }
                    }
                    IrTypeOp::SafeCast => {}
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
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
                    if parts.iter().any(|&p| self.records_frame(p)) {
                        let temps = self.spill_to_temps(&parts, code);
                        code.new_obj(sb);
                        code.dup();
                        code.invokespecial(init, 0, 0);
                        for &(slot, t, _) in &temps {
                            load(t, slot, code);
                            self.append_top(t, code);
                        }
                        for &(_, _, key) in &temps {
                            self.slots.remove(&key);
                        }
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
            IrExpr::EnumEntry { class, index } => {
                let c = &self.ir.classes[*class as usize];
                let entry = c.enum_entries[*index as usize].name.clone();
                let fq_name = c.fq_name();
                let desc = format!("L{fq_name};");
                let f = self.cw.fieldref(&fq_name, &entry, &desc);
                code.getstatic(f, 1);
            }
            IrExpr::StaticInstance { owner, ty, field } => {
                let owner_fq = self.ir.classes[*owner as usize].fq_name();
                let ty_fq = self.ir.classes[*ty as usize].fq_name();
                let f = self.cw.fieldref(&owner_fq, field, &format!("L{ty_fq};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticInstance { owner, ty, field } => {
                let owner = owner.render();
                let ty = ty.render();
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
            IrExpr::SetExternalStaticField {
                owner,
                name,
                descriptor,
                value,
            } => {
                let owner = owner.render();
                let f = self.cw.fieldref(&owner, name, descriptor);
                let words = if descriptor == "J" || descriptor == "D" {
                    2
                } else {
                    1
                };
                self.emit_value(*value, code);
                code.putstatic(f, words);
            }
            IrExpr::EnumValues { class } => {
                let fq = self.ir.classes[*class as usize].fq_name();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::ReifiedClassMarker { name, erased } => {
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
            IrExpr::EnumValueOf { class, arg } => {
                let fq = self.ir.classes[*class as usize].fq_name();
                self.emit_value(*arg, code);
                let m = self
                    .cw
                    .methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                code.invokestatic(m, 1, 1);
            }
            IrExpr::When { branches } => self.emit_when(branches, code),
            // Block in value position: run its statements for effect, leave the trailing value on the
            // stack. Scope block-locals (restore the slot map) so they don't leak into outer frames.
            IrExpr::Block { stmts, value } => {
                let saved = self.slots.clone();
                self.block_depth += 1;
                let mut dead = false;
                for s in stmts {
                    // A statement root carrying a source line starts a `LineNumberTable` entry.
                    if let Some(&l) = self.ir.expr_lines.get(s) {
                        code.mark_line(l);
                    }
                    // A statement nets zero on the operand stack (its value is stored/discarded). Reset
                    // the tracked height to that baseline afterward: a branchy lambda splice (`takeIf`)
                    // tracks its internal branches only approximately and can leave `cur_stack` drifted
                    // above the real (verified-balanced) height, which would make a LATER branchy splice
                    // in the same block falsely see a non-empty baseline and bail.
                    let base = code.stack_height();
                    self.emit(*s, code);
                    if self.diverges(*s) {
                        dead = true;
                        break;
                    }
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        if let Some(&l) = self.ir.expr_lines.get(v) {
                            code.mark_line(l);
                        }
                        self.emit_value(*v, code);
                    }
                }
                self.close_scope_locals(code);
                self.block_depth -= 1;
                self.slots = saved;
            }
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } => {
                // This lambda becomes a REAL closure (`invokedynamic` referencing its impl method) — record
                // it so the dead-lambda pass keeps the impl. An INLINED lambda never reaches this arm.
                self.run.used_lambdas.borrow_mut().insert(*impl_fn);
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params = jvm_tys(&f.params);
                let impl_ret = ir_ty_to_jvm(&f.ret);
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
                    Some((iface, method, descriptor)) => {
                        // `samMethodType` is the INTERFACE method's (erased) descriptor — NOT the
                        // lambda's — so a SAM with parameters (or a generic SAM erased to `Object`)
                        // matches the abstract method the metafactory must implement.
                        let sam_desc = descriptor.clone().unwrap_or_else(|| {
                            self.ir
                                .classes
                                .iter()
                                .find(|c| c.fq_name_matches(iface))
                                .and_then(|c| {
                                    c.methods
                                        .iter()
                                        .map(|&m| &self.ir.functions[m as usize])
                                        .find(|f| f.name == *method)
                                })
                                .map(|f| ir_method_desc(&f.params, &f.ret))
                                .unwrap_or_else(|| method_descriptor(lam_tys, impl_ret))
                        });
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
                        let inst_desc = crate::jvm::names::parse_method_descriptor(&sam_desc)
                            .filter(|(sam_params, _)| sam_params.len() == lam_tys.len())
                            .map(|(sam_params, sam_ret)| {
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
                                let ret = if descriptor_is_reference(sam_ret) {
                                    boxed_descriptor(impl_ret)
                                } else {
                                    type_descriptor(impl_ret)
                                };
                                format!("({params}){ret}")
                            })
                            // A malformed or temporarily incomplete provider descriptor must not
                            // invent slot alignment. Preserve the prior implementation-signature
                            // fallback; normal resolved SAMs always supply a parseable descriptor.
                            .unwrap_or_else(|| method_descriptor(lam_tys, impl_ret));
                        (iface.clone(), method.clone(), sam_desc, inst_desc)
                    }
                    None => {
                        let iface = format!("kotlin/jvm/functions/Function{arity}");
                        let inst_params: Vec<String> =
                            lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc =
                            format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (
                            iface,
                            "invoke".to_string(),
                            sam_descriptor(*arity),
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
                let impl_owner = self
                    .ir
                    .classes
                    .iter()
                    .find(|c| c.methods.contains(impl_fn))
                    .map(|c| c.fq_name())
                    .unwrap_or_else(|| self.facade.clone());
                if self.lambdas == LambdaMode::Class {
                    // `box$lambda$0` names the enclosing declaration and this lambda's index within
                    // it, which is exactly what the synthetic class name is built from.
                    let (enclosing, index) = impl_name
                        .split_once("$lambda$")
                        .map(|(head, tail)| (head.to_string(), tail.parse::<u32>().unwrap_or(0)))
                        .unwrap_or_else(|| (impl_name.clone(), 0));
                    // kotlinc names the class after the declaration the lambda initializes
                    // (`LKt$box$plain$1`), falling back to a 1-based index among the enclosing
                    // declaration's lambdas when it initializes nothing (`C$m$1`).
                    let bound_name = self.ir.exprs.iter().enumerate().find_map(|(id, node)| {
                        match node {
                            IrExpr::Variable {
                                index: value_index,
                                init: Some(init),
                                named: true,
                                ..
                            } if *init == e => self.ir.value_names.get(value_index).cloned(),
                            _ => None,
                        }
                        .filter(|_| id as u32 != e)
                    });
                    let internal = match &bound_name {
                        Some(name) => format!("{impl_owner}${enclosing}${name}$1"),
                        None => format!("{impl_owner}${enclosing}${}", index + 1),
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
                        owner_is_interface: self
                            .ir
                            .classes
                            .iter()
                            .any(|c| c.fq_name_matches(&impl_owner) && c.is_interface),
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
                let impl_mh = self
                    .cw
                    .method_handle_static(&impl_owner, &impl_name, &impl_desc);
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
                // The CPS pass (`jvm/suspend.rs`) rewrites every `CurrentContinuation` to a `GetValue` of
                // the continuation slot before emit; reaching here means it was emitted outside a suspend
                // function, which the front end forbids.
                unreachable!("CurrentContinuation must be resolved by the CPS pass before emit")
            }
            IrExpr::NotNullAssert { operand } => {
                self.emit_value(*operand, code);
                code.dup();
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNull",
                    "(Ljava/lang/Object;)V",
                );
                code.invokestatic(m, 1, 0);
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
                let jt = self.value_ty(*operand);
                let st = self.verif_stack(jt);
                self.frame(lbl, st, code);
                code.bind(lbl);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            // `return v` in value position (`x ?: return v`): emit the return; control transfers away, so
            // (like `throw`) nothing is left for the surrounding merge.
            IrExpr::Return(ret_val) => match ret_val {
                Some(rv) => {
                    let ret = self.ret;
                    self.emit_value_as(*rv, &ret, code);
                    if !self.diverges(*rv) {
                        emit_return(self.ret, code);
                    }
                }
                None => code.ret_void(),
            },
            IrExpr::Vararg {
                array_type,
                elements,
                spreads,
            } => {
                let et = array_jvm_element(array_type);
                let elements = elements.clone();
                let spreads = spreads.clone();
                if spreads.len() != elements.len() {
                    self.run.set_emit_error(
                        "vararg spread flags do not match the element list".to_string(),
                    );
                    return;
                }
                if spreads.iter().any(|&spread| spread) {
                    if et.is_jvm_scalar() {
                        let Some((builder, add_desc, array_desc)) = primitive_spread_builder(et)
                        else {
                            self.run.set_emit_error(
                                "primitive vararg spread has no platform builder".to_string(),
                            );
                            return;
                        };
                        let class = self.cw.class_ref(builder);
                        code.new_obj(class);
                        code.dup();
                        code.push_int(elements.len() as i32, self.cw);
                        let init = self.cw.methodref(builder, "<init>", "(I)V");
                        code.invokespecial(init, 1, 0);
                        // `[builder, builder]` stays live across each element (the `dup` is the `add`
                        // receiver), so a branchy element must frame them — see `emit_value_over`.
                        let held = self.held_pair(builder);
                        for (index, &element) in elements.iter().enumerate() {
                            code.dup();
                            self.emit_value_over(element, &held, code);
                            if spreads.get(index).copied().unwrap_or(false) {
                                let add_spread = self.cw.methodref(
                                    "kotlin/jvm/internal/PrimitiveSpreadBuilder",
                                    "addSpread",
                                    "(Ljava/lang/Object;)V",
                                );
                                code.invokevirtual(add_spread, 1, 0);
                            } else {
                                let add = self.cw.methodref(builder, "add", add_desc);
                                code.invokevirtual(add, slot_words(et) as i32, 0);
                            }
                        }
                        let to_array =
                            self.cw
                                .methodref(builder, "toArray", &format!("(){array_desc}"));
                        code.invokevirtual(to_array, 0, 1);
                    } else {
                        let builder = "kotlin/jvm/internal/SpreadBuilder";
                        let class = self.cw.class_ref(builder);
                        code.new_obj(class);
                        code.dup();
                        code.push_int(elements.len() as i32, self.cw);
                        let init = self.cw.methodref(builder, "<init>", "(I)V");
                        code.invokespecial(init, 1, 0);
                        let box_elem = boxed_prim_of(et);
                        let held = self.held_pair(builder);
                        for (index, &element) in elements.iter().enumerate() {
                            code.dup();
                            self.emit_value_over(element, &held, code);
                            let method = if spreads.get(index).copied().unwrap_or(false) {
                                self.cw
                                    .methodref(builder, "addSpread", "(Ljava/lang/Object;)V")
                            } else {
                                if let Some(primitive) = box_elem {
                                    box_prim_free(self.cw, code, primitive);
                                }
                                self.cw.methodref(builder, "add", "(Ljava/lang/Object;)V")
                            };
                            code.invokevirtual(method, 1, 0);
                        }
                        code.push_int(0, self.cw);
                        let element_class = self.cw.class_ref(&ref_internal(et.non_null()));
                        code.anewarray(element_class);
                        let to_array = self.cw.methodref(
                            builder,
                            "toArray",
                            "([Ljava/lang/Object;)[Ljava/lang/Object;",
                        );
                        code.invokevirtual(to_array, 1, 1);
                        let array_class = self
                            .cw
                            .class_ref(&type_descriptor(ir_ty_to_jvm(array_type)));
                        code.checkcast(array_class);
                    }
                    return;
                }
                code.push_int(elements.len() as i32, self.cw);
                let reference_array = array_type.is_reference_array();
                if et.is_jvm_scalar() && !reference_array {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    // Nullability does not change the reference array class.
                    let ci = self.cw.class_ref(&ref_internal(et.non_null()));
                    code.anewarray(ci);
                }
                let (op, w) = array_store_op(et, reference_array);
                // A boxed-primitive element array (`arrayOf(1,2,3)` → `Integer[]`): box each primitive
                // value before the `aastore` (mirrors `kotlin/Array.set`).
                let box_elem = reference_array.then(|| boxed_prim_of(et)).flatten();
                // `[array, array, index]` stays live across each element — a branchy one must frame
                // them (see `emit_value_over`).
                let array_v = self.verif_single(ir_ty_to_jvm(array_type));
                let held = [array_v.clone(), array_v, VerifType::Integer];
                for (i, &el) in elements.iter().enumerate() {
                    code.dup();
                    code.push_int(i as i32, self.cw);
                    self.emit_value_over(el, &held, code);
                    if let Some(p) = box_elem {
                        box_prim_free(self.cw, code, p);
                    }
                    code.array_store(op, w);
                }
            }
            IrExpr::NewArray { array_type, size } => {
                let et = array_jvm_element(array_type);
                self.emit_value(*size, code);
                if et.is_jvm_scalar() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    // Peel a nullable element's `?`: `Array<Int?>` = `Integer[]`, so the `anewarray` class
                    // is `java/lang/Integer` (the `?` only tells `Array.get`/`.set` to keep it boxed).
                    let ci = self.cw.class_ref(&ref_internal(et.non_null()));
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
                let result = result.clone();
                self.emit_try(*body, &catches, *finally, &result, code);
            }
            IrExpr::RefNew { elem, init } => {
                let (cls, fdesc) = ref_class(elem);
                let ew = slot_words(ir_ty_to_jvm(elem)) as i32;
                // A branchy initializer can't run with `[holder, holder]` on the stack — spill it.
                if self.records_frame(*init) {
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
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
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
                if ejvm.is_reference() && ref_internal(ejvm) != "java/lang/Object" {
                    let cc = self.cw.class_ref(&ref_internal(ejvm));
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
                if self.records_frame(*value) {
                    let temps = self.spill_to_temps(&[*value], code);
                    self.emit_value(*holder, code);
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
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
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with the function value on the stack — its merge
                    // frame would omit it. Evaluate the function + args into temps first (in order),
                    // then load and box.
                    let mut all = vec![*func];
                    all.extend(args.iter().copied());
                    let temps = self.spill_to_temps(&all, code);
                    load(temps[0].1, temps[0].0, code);
                    for (i, &(slot, t, _)) in temps[1..].iter().enumerate() {
                        load(t, slot, code);
                        let semantic = params.get(i).copied().unwrap_or(t);
                        box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, t));
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    self.emit_value(*func, code);
                    for (i, &arg) in args.iter().enumerate() {
                        self.emit_value(arg, code);
                        let at = self.value_ty(arg);
                        let semantic = params.get(i).copied().unwrap_or(at);
                        // `FunctionN` parameters are erased `Object`, but wrapper selection is a
                        // semantic operation. Retaining `params` on the IR node prevents an unsigned
                        // argument from being boxed as the signed wrapper of its shared carrier.
                        box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, at));
                    }
                }
                let iface = format!("kotlin/jvm/functions/Function{n}");
                let m = self
                    .cw
                    .interface_methodref(&iface, "invoke", &sam_descriptor(n as u8));
                code.invokeinterface(m, n as i32, 1);
                // The interface returns `Object`; cast/unbox to the function's declared return type.
                // Select a scalar adapter from that semantic return before `ir_ty_to_jvm` reduces an
                // unsigned type to its signed carrier. This is the common consumer for real lambdas,
                // callable references, and property references, independent of which producer object
                // supplied the `FunctionN` implementation.
                let rt = ir_ty_to_jvm(ret);
                if rt.is_jvm_scalar() {
                    unbox_prim(self.cw, code, semantic_scalar_adapter(*ret, rt));
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
            let array_verification = self.verif_single(self.value_ty(array));
            self.emit_value_over(index, &[array_verification], code);
        }
        let (operation, words) = array_load_op(element, reference_array);
        code.array_load(operation, words);
        if let Some(primitive) = reference_array.then(|| boxed_prim_of(element)).flatten() {
            unbox_prim(self.cw, code, primitive);
        }
    }

    fn emit_array_set(&mut self, array: u32, index: u32, value: u32, code: &mut CodeBuilder) {
        let element = self.array_elem(array);
        let reference_array = self.value_ty(array).is_reference_array();
        if self.must_spill_across(index) || self.must_spill_across(value) {
            self.emit_operands(&[array, index, value], code);
        } else {
            self.emit_value(array, code);
            let array_verification = self.verif_single(self.value_ty(array));
            self.emit_value_over(index, std::slice::from_ref(&array_verification), code);
            let index_verification = self.verif_single(self.value_ty(index));
            self.emit_value_over(value, &[array_verification, index_verification], code);
        }
        if let Some(primitive) = reference_array.then(|| boxed_prim_of(element)).flatten() {
            box_prim_free(self.cw, code, primitive);
        }
        let (operation, words) = array_store_op(element, reference_array);
        code.array_store(operation, words);
    }

    fn append(&mut self, e: u32, code: &mut CodeBuilder) {
        // A part boxed only to satisfy `String.plus(Any?)` (`"…" + port` wraps the `Int` through a
        // reference coercion) appends via the PRIMITIVE overload — kotlinc's StringBuilder
        // specialization (`append(I)`), never `valueOf` + `append(Object)`. The box is
        // concat-invisible, so see through it.
        let e = match self.ir.expr(e) {
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
                if !unsigned_inner
                    && !ir_ty_to_jvm(&inner_ty).is_reference()
                    && ir_ty_to_jvm(&self.value_ty(e)).is_reference()
                {
                    inner
                } else {
                    e
                }
            }
            _ => e,
        };
        let ty = self.value_ty(e);
        self.emit_value(e, code);
        self.append_top(ty, code);
    }

    fn emit_string_plus(&mut self, recv: u32, arg: u32, code: &mut CodeBuilder) {
        let sb = self.cw.class_ref("java/lang/StringBuilder");
        // A branchy operand (`when`/`try`) can't be emitted with the `StringBuilder` on the stack — its
        // merge frames would omit it. Spill such operands to temps first.
        if self.records_frame(recv) || self.records_frame(arg) {
            let temps = self.spill_to_temps(&[recv, arg], code);
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
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            code.new_obj(sb);
            code.dup();
            let init = self
                .cw
                .methodref("java/lang/StringBuilder", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            self.append(recv, code);
            self.append(arg, code);
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
        if parts.iter().any(|&p| self.records_frame(p)) {
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
    /// (`pending_stack` must not be used across it): a `try` in the subtree — the JVM clears the
    /// operand stack on handler entry, so a held value would be lost (and its handler frame mistyped) —
    /// or an inlinable call, whose splice expects the stack baseline its frames were recorded at.
    /// Conservative: the spill path is always correct, only byte-parity with kotlinc is deferred.
    fn must_spill_across(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Try { .. } => true,
            IrExpr::Call {
                callee: Callee::Static { inline, .. },
                ..
            } if inline.can_inline() => true,
            _ => {
                let mut spill = false;
                crate::ir::for_each_child(&self.ir.exprs, e, &mut |c| {
                    spill = spill || self.must_spill_across(c);
                });
                spill
            }
        }
    }

    /// Whether emitting `e` records a StackMapTable frame anywhere in its subtree. An operand
    /// sequence uses this semantic fact either to spill earlier values to locals or, where the
    /// instruction shape must keep them live, to describe them through [`Self::emit_value_over`].
    fn records_frame(&self, e: u32) -> bool {
        use IrBinOp::*;
        match self.ir.expr(e) {
            IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. } => true,
            // The multi-part `StringConcat` itself spills branchy parts internally, so as a whole it
            // leaves only its `String` result — but a parent operand sequence still must treat it as
            // frame-recording if any part does (it builds the StringBuilder mid-stack otherwise).
            IrExpr::StringConcat(parts) => parts.iter().any(|&p| self.records_frame(p)),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_jvm_scalar())
                    // `===`/`!==` always emits a branch+merge frame — the `if_acmp*` path (references)
                    // and the value-compare path it remaps to for primitives both do.
                    || matches!(op, RefEq | RefNe)
                    // `x == null`/`x != null` emits an `ifnull`/`ifnonnull` branch+merge frame.
                    || (matches!(op, Eq | Ne)
                        && (matches!(self.ir.expr(*lhs), IrExpr::Const(IrConst::Null))
                            || matches!(self.ir.expr(*rhs), IrExpr::Const(IrConst::Null))))
                    || self.records_frame(*lhs) || self.records_frame(*rhs)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => {
                // An inline call whose SPLICED body records StackMapTable frames — a branchy lambda body,
                // or a branchy host body (a loop HOF like `map`/`filter`, or an `@InlineOnly` `require`/
                // `check`) — records frames at THIS position. So a parent operand sequence must spill the
                // earlier operands to temps (keeping the splice at an empty baseline), exactly as for
                // `when`/`try`. Without this, an inline HOF used as a non-first operand
                // (`sb.append(xs.map { … }))`) would splice at a non-empty baseline and bail to a real call.
                let splice_records = match callee {
                    Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline,
                    } if inline.can_inline() => {
                        args.iter().any(|&a| {
                            matches!(self.ir.expr(a),
                                IrExpr::Lambda { inline_body: Some(b), .. } if self.records_frame(*b))
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
                splice_records
                    || dispatch_receiver.map_or(false, |r| self.records_frame(r))
                    || args.iter().any(|&a| self.records_frame(a))
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                self.records_frame(*receiver)
                    || args
                        .iter()
                        .any(|a| a.map_or(false, |x| self.records_frame(x)))
            }
            IrExpr::New { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            // A `lateinit` FIELD read carries its own uninitialized guard (`dup; ifnonnull L; ldc name;
            // invokestatic throwUninitializedPropertyAccessException; L:`), whose join records a frame
            // typing only the field value — so an operand already on the stack must be spilled first. A
            // read through a getter records nothing here: the guard lives inside the accessor body.
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                self.records_frame(*receiver)
                    || self.ir.classes[*class as usize].fields[*index as usize].is_lateinit()
            }
            IrExpr::PropertyRead {
                receiver,
                owner,
                name,
                field,
                ..
            } => {
                self.records_frame(*receiver)
                    || self.lateinit_direct_field_read(&owner.render(), name, field.as_deref())
            }
            IrExpr::SetField {
                receiver, value, ..
            }
            | IrExpr::PropertyWrite {
                receiver, value, ..
            } => self.records_frame(*receiver) || self.records_frame(*value),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.records_frame(*value)
            }
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => {
                self.records_frame(*arg)
            }
            IrExpr::NotNullAssert { operand } => self.records_frame(*operand),
            // A `lateinit` read emits an `ifnonnull` merge frame, so a parent must spill other operands
            // first (else the frame at the join would omit them).
            IrExpr::LateinitCheck { .. } => true,
            IrExpr::RefGet { holder, .. } => self.records_frame(*holder),
            IrExpr::RefSet { holder, value, .. } => {
                self.records_frame(*holder) || self.records_frame(*value)
            }
            IrExpr::RefNew { init, .. } => self.records_frame(*init),
            IrExpr::Throw { operand } => self.records_frame(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.records_frame(a)),
            IrExpr::NewArray { size, .. } => self.records_frame(*size),
            IrExpr::Return(v) => v.map_or(false, |x| self.records_frame(x)),
            IrExpr::Variable { init, .. } => init.map_or(false, |i| self.records_frame(i)),
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| self.records_frame(s))
                    || value.map_or(false, |v| self.records_frame(v))
            }
            _ => false, // Const, GetValue, GetStatic, EnumEntry, EnumValues — no frames
        }
    }

    /// Push `ops` onto the stack in order. If any op after the first records a frame (so an earlier
    /// op would be live on the stack across that frame), evaluate all ops into temps first, then load
    /// them — keeping the stack empty while each frame-recording op runs.
    fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        self.emit_operands_adapted(ops, code, |_, _, _| {});
    }

    /// Adapt one semantic value to the physical slot named by a JVM descriptor. Wrapper identity and
    /// primitive carriers are backend facts: core resolution has already selected the callable and
    /// never needs to know whether this boundary boxes, unboxes, narrows, or performs a numeric JVM
    /// conversion.
    fn adapt_physical_operand(&mut self, source: Ty, physical: Ty, code: &mut CodeBuilder) {
        // `source` comes from `value_ty`/a spill slot and is already the verifier-visible stack type.
        // Re-running semantic erasure here would turn the boxed `Obj("kotlin/Int")` back into scalar
        // `Int` and box it a second time.
        let source_jvm = source;
        crate::trace_compiler!(
            "value_classes",
            "descriptor operand source={source:?} jvm={source_jvm:?} physical={physical:?}"
        );
        if source_jvm.is_jvm_scalar() && physical.is_reference() {
            let semantic = if physical.non_null().is_unsigned() {
                physical.non_null()
            } else {
                source
            };
            box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, source_jvm));
        } else if source_jvm.is_reference() && physical.is_jvm_scalar() {
            unbox_prim(self.cw, code, semantic_scalar_adapter(source, physical));
        } else if source_jvm.is_jvm_scalar() && physical.is_jvm_scalar() {
            emit_num_conv(source_jvm, physical, code);
        } else if source_jvm.is_reference() && physical.is_reference() {
            self.narrow_on_stack(source_jvm, &physical, code);
        }
    }

    fn emit_descriptor_operands(&mut self, ops: &[u32], physical: &[Ty], code: &mut CodeBuilder) {
        assert_eq!(
            ops.len(),
            physical.len(),
            "selected call argument count must match its JVM descriptor"
        );
        let mut index = 0usize;
        self.emit_operands_adapted(ops, code, |this, source, code| {
            let target = physical[index];
            index += 1;
            this.adapt_physical_operand(source, target, code);
        });
    }

    fn emit_descriptor_virtual_operands(
        &mut self,
        owner: &str,
        receiver: u32,
        args: &[u32],
        physical_params: &[Ty],
        code: &mut CodeBuilder,
    ) {
        assert_eq!(
            args.len(),
            physical_params.len(),
            "selected member call argument count must match its JVM descriptor"
        );
        let mut ops = Vec::with_capacity(args.len() + 1);
        ops.push(receiver);
        ops.extend(args.iter().copied());
        let mut physical = Vec::with_capacity(physical_params.len() + 1);
        let owner_ty = Ty::obj(owner);
        physical.push(if owner_ty.scalar_value_repr().is_some() {
            Ty::nullable(owner_ty)
        } else {
            owner_ty
        });
        physical.extend_from_slice(physical_params);
        self.emit_descriptor_operands(&ops, &physical, code);
    }

    /// Frame-safe operand sequencing with one representation adapter applied immediately after each
    /// value is pushed. Keeping the adapter inside the shared spill/load loop is essential for
    /// category-changing bridges such as primitive boxing: a wide left operand cannot be repaired
    /// after a right operand has landed above it, and a branchy right operand still requires both
    /// source expressions to be evaluated with an empty stack. Consumers supply only the boundary
    /// adapter; evaluation order, frame safety, temporary ownership, and cleanup remain centralized.
    fn emit_operands_adapted<F>(&mut self, ops: &[u32], code: &mut CodeBuilder, mut adapt: F)
    where
        F: FnMut(&mut Self, Ty, &mut CodeBuilder),
    {
        if ops.iter().skip(1).any(|&o| self.records_frame(o)) {
            let temps = self.spill_to_temps(ops, code);
            for &(slot, t, _) in &temps {
                load(t, slot, code);
                adapt(self, t, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            for &o in ops {
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

    /// Emit `e` as a value while `held` operand-stack entries (bottom-first) are ALREADY pushed below
    /// it — the array being filled element-wise by a `Vararg`, the `SpreadBuilder` an element is
    /// `add`ed to, the receiver+index of an `Array.set`. Those positions can't spill to a temp the way
    /// `emit_operands` does (the store instruction needs its operands underneath), so the held entries
    /// are handed to `frame` through `pending_stack` instead: a stack-map frame must type the FULL
    /// operand stack the verifier sees, not just what the sub-expression itself leaves.
    ///
    /// Without this a branchy element (`listOf(x == y, x != y)`) records `stack = []` at its merge
    /// label while the verifier sees `[array, array, int]` there. The class file is emitted fine and
    /// only fails at link time with "Inconsistent stackmap frames at branch target N".
    fn emit_value_over(&mut self, e: u32, held: &[VerifType], code: &mut CodeBuilder) {
        if held.is_empty() || !self.records_frame(e) {
            self.emit_value(e, code);
            return;
        }
        // Preserve an outer held-operand context exactly. Nested emission may itself use this helper;
        // restoring a saved depth makes the scope rule explicit and cannot accidentally retain or
        // remove entries based on the nested call's final length.
        let outer_depth = self.pending_stack.len();
        self.pending_stack.extend_from_slice(held);
        self.emit_value(e, code);
        self.pending_stack.truncate(outer_depth);
    }

    /// The two `[receiver, receiver]` stack entries a `dup`-then-call sequence holds. The receiver
    /// must already be INITIALIZED — right after `new C; dup` the verification type is
    /// `Uninitialized(offset)`, not the class, so this is wrong for that position.
    fn held_pair(&mut self, owner: &str) -> [VerifType; 2] {
        let v = self.verif_single(Ty::obj(owner));
        [v.clone(), v]
    }

    /// Push the two operands of a referential `===`/`!==` that compares object refs, BOXING whichever
    /// side is a primitive right where it lands — kotlinc's shape for a mixed pair (`aload_0; iload_1;
    /// Integer.valueOf; if_acmpne`), which it accepts with only an "identity equality … can be unstable
    /// because of implicit boxing" warning. Boxing has to happen per operand rather than once at the
    /// end: a `Long`/`Double` left operand occupies two stack words, so a boxed right operand cannot be
    /// swapped past it. The shared adapted-operand path owns evaluation order, frame-aware spilling,
    /// and temporary cleanup; identity supplies only the primitive-to-reference adapter.
    fn emit_identity_operands(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        self.emit_operands_adapted(&[lhs, rhs], code, Self::box_scalar_operand);
    }

    fn emit_virtual_operands(
        &mut self,
        owner: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        let recv_ty = self.value_ty(recv);
        let box_recv_as = wrapper_owner_primitive(owner).filter(|_| recv_ty.is_jvm_scalar());
        // A member call on a value whose static type is `owner` but whose ERASED physical type is a top
        // (`Object`) needs the `checkcast owner` kotlinc inserts before the dispatch verifies.
        let narrow_recv = |e: &mut Self, src: Ty, code: &mut CodeBuilder| {
            if box_recv_as.is_none() {
                e.narrow_on_stack(src, &Ty::obj(owner), code);
            }
        };
        if args.iter().any(|&o| self.records_frame(o)) {
            let mut ops = vec![recv];
            ops.extend(args.iter().copied());
            let temps = self.spill_to_temps(&ops, code);
            for (i, &(slot, t, _)) in temps.iter().enumerate() {
                load(t, slot, code);
                if i == 0 {
                    if let Some(box_ty) = box_recv_as {
                        box_prim_free(self.cw, code, box_ty);
                    } else {
                        narrow_recv(self, t, code);
                    }
                }
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            self.emit_value(recv, code);
            if let Some(box_ty) = box_recv_as {
                box_prim_free(self.cw, code, box_ty);
            } else {
                narrow_recv(self, recv_ty, code);
            }
            for &arg in args {
                self.emit_value(arg, code);
            }
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
            unbox_prim(self.cw, code, owner_prim);
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

    /// Evaluate each of `ops` into a fresh temp slot, in order. Each temp is registered in `self.slots`
    /// (so a *later* op's frames see the earlier temps as live, not `Top`); the caller loads them and
    /// then removes them (they're dead once loaded). Returns `(slot, ty, slots-key)` per op.
    fn spill_to_temps(&mut self, ops: &[u32], code: &mut CodeBuilder) -> Vec<(u16, Ty, u32)> {
        let mut temps = Vec::new();
        for &o in ops {
            self.emit_value(o, code);
            let t = self.value_ty(o);
            crate::trace_compiler!(
                "splice",
                "spill inline operand expression={o} node={:?} type={t:?}",
                self.ir.expr(o)
            );
            let slot = self.next_slot;
            self.next_slot += slot_words(t);
            store(t, slot, code);
            let key = 2_000_000 + slot as u32;
            self.slots.insert(key, (slot, t));
            temps.push((slot, t, key));
        }
        temps
    }

    fn emit_binop(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                // A branchy RHS (`result*31 + <nullable-field hashCode ternary>`): keep the numeric LHS on
                // the operand stack across the RHS's branch — matching kotlinc — by typing it into the
                // RHS's stack-map frames via `pending_stack`, instead of spilling it to a temp. The LHS of
                // arithmetic is always a numeric scalar, so the pending verif type interns no `Class`. A
                // non-branchy RHS (or a branchy LHS) keeps the ordinary `emit_operands` path (spill only if
                // needed) — bytecode unchanged for the common case. NOT applicable when the RHS can enter
                // an exception handler or splice inline bytecode (`must_spill_across`): a handler CLEARS
                // the operand stack (the held LHS would be lost and its handler frame mistyped), and a
                // splice expects its recorded baseline.
                if self.records_frame(rhs)
                    && !self.records_frame(lhs)
                    && !self.must_spill_across(rhs)
                {
                    self.emit_value(lhs, code);
                    let lv = self.verif_single(lt);
                    // Use the same scoped pending-stack path as container fills and array
                    // subscripts; all held-operand frames now share one restoration invariant.
                    self.emit_value_over(rhs, &[lv], code);
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
            }
            And | Or => {
                // Evaluate lhs, hold it in a temp while rhs is emitted (rhs may record frames that
                // must see the temp as live), then combine. The temp is dead afterwards, so remove it
                // from the slot map so it doesn't leak into later merge frames (next_slot stays
                // monotonic — no reuse). Without this, a `false`/`else` path that never assigned the
                // temp reaches a merge whose frame claims it's defined → VerifyError.
                self.emit_value(lhs, code);
                let tmp = self.next_slot;
                self.next_slot += 1;
                let key = 1_000_000 + tmp as u32;
                self.slots.insert(key, (tmp, Ty::Boolean));
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And {
                    code.iand()
                } else {
                    code.ior()
                }
                self.slots.remove(&key);
            }
            BitAnd | BitOr | BitXor => {
                self.emit_operands(&[lhs, rhs], code);
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
        self.frame(end, vec![VerifType::Integer], code);
        code.goto(end);
        code.bind(f);
        code.set_stack(merged);
        code.push_int(0, self.cw);
        code.bind(end);
    }

    /// Emit a conditional jump to `target`, taken exactly when `cond` evaluates to `jump_when_true`.
    /// When `cond` is a primitive/reference comparison it is FUSED into the branch (`if_icmpge`,
    /// `ifnull`, `if_acmpeq`, `lcmp;ifge`, …) instead of materializing a 0/1 boolean and testing it
    /// with `ifeq`/`ifne` — the bytecode kotlinc emits for every `if`/`while`/`for` over a comparison.
    ///
    /// Returns `true` when the jump was emitted UNCONDITIONALLY (a constant condition that always
    /// takes it): the caller's fall-through path is then statically unreachable, and whatever it would
    /// emit next lands after a `goto` with nothing branching to it — dead code with no stack-map frame,
    /// which the verifier rejects outright ("Expecting a stack map frame"). Such a caller must emit
    /// nothing on that path. kotlinc likewise emits no body for a never-entered branch.
    #[must_use = "an unconditionally-taken jump makes the fall-through path dead — emitting there \
                  leaves frameless code the verifier rejects"]
    fn emit_cond_branch(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        // A constant condition folds: `while (true)` (a `Boolean(true)` pre-test, jump-out-when-false)
        // emits NO branch — a spurious `ifeq end` to the method end leaves a branch target with no
        // stack-map frame. An always-taken branch becomes an unconditional `goto`.
        if let IrExpr::Const(IrConst::Boolean(b)) = *self.ir.expr(cond) {
            // Frame the target regardless (callers — `when`/loop emission — rely on the branch target
            // having a stack-map frame), but only emit the jump when the constant actually takes it.
            self.frame(target, vec![], code);
            if b == jump_when_true {
                code.goto(target);
                return true;
            }
            return false;
        }
        if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(cond) {
            use IrBinOp::*;
            if matches!(op, Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe) {
                self.emit_compare_branch(op, lhs, rhs, target, jump_when_true, code);
                return false;
            }
        }
        // Fuse `x is T` / `x !is T` (a reference target) into `instanceof; if{ne,eq}` — no 0/1 boolean is
        // materialized (kotlinc's shape, e.g. a data class `equals`' `instanceof; ifne <ok>`).
        let inst_fuse = if let IrExpr::TypeOp {
            op: to,
            arg,
            type_operand,
        } = self.ir.expr(cond)
        {
            if matches!(to, IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf) {
                let jvm_ty = ir_ty_to_jvm(type_operand);
                (!jvm_ty.is_jvm_scalar()).then(|| (*to, *arg, ref_internal(jvm_ty)))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((to, arg, internal)) = inst_fuse {
            self.emit_value(arg, code);
            let ci = self.cw.class_ref(&internal);
            code.instance_of(ci);
            self.frame(target, vec![], code);
            // Stack holds 1 iff `arg instanceof T`. The condition is true on `instanceof` for `InstanceOf`
            // and on `!instanceof` for `NotInstanceOf`; jump when the condition equals `jump_when_true`.
            let jump_on_instance = if matches!(to, IrTypeOp::InstanceOf) {
                jump_when_true
            } else {
                !jump_when_true
            };
            if jump_on_instance {
                code.ifne(target);
            } else {
                code.ifeq(target);
            }
            return false;
        }
        self.emit_value(cond, code);
        self.frame(target, vec![], code);
        if jump_when_true {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
        false
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
        self.frame(target, vec![], code);
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
            self.frame(target, vec![], code);
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
            self.emit_operands_adapted(&[operand], code, Self::box_scalar_operand);
            self.frame(target, vec![], code);
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
        if matches!(op, Eq | Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            return false;
        }
        self.emit_numeric_compare_branch(op, lhs, rhs, target, jt, code);
        true
    }

    /// Put the null-safe structural equality result for two references on the operand stack.
    fn emit_structural_equality(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        // Spill if rhs is branchy (`x == when { ... }`) so lhs is not live across its merge frames.
        self.emit_operands(&[lhs, rhs], code);
        let m = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "areEqual",
            "(Ljava/lang/Object;Ljava/lang/Object;)Z",
        );
        code.invokestatic(m, 2, 1);
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
        self.frame(target, vec![], code);
        match cmp0_int {
            Some(o) => cmp0_branch(o, jt, target, code),
            None if !int_cat => cmp0_branch(op, jt, target, code),
            None => icmp_branch(op, jt, target, code),
        }
    }

    fn emit_when(&mut self, branches: &[(Option<u32>, u32)], code: &mut CodeBuilder) {
        let end = code.new_label();
        // The operand-stack height BEFORE any branch (the conditions consume their own operands). Each
        // subsequent branch is reached by a JUMP from the previous condition, so it starts at THIS height,
        // not the height the previous branch left after pushing its value (the linear counter carries the
        // prior branch's value across `bind(next)`); reset it so a branch body emits on the right baseline
        // (else e.g. an inline HOF splice in the SECOND branch sees a phantom operand and bails).
        let entry_height = code.stack_height().max(0) as u16;
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        // A `when` with no `else`, or one whose value is `Unit`, is a statement: branch values are
        // discarded and nothing reaches the operand stack at `end`.
        let is_stmt = !has_else || self.value_ty_of_when(branches) == Ty::Unit;
        let result_stack = if is_stmt {
            vec![]
        } else {
            self.verif_stack(self.value_ty_of_when(branches))
        };
        // `end` is reachable if any branch falls through to it (i.e. doesn't return/throw). A
        // no-`else` statement always has the implicit no-match fallthrough.
        let mut end_reachable = !has_else;
        for (cond, body) in branches {
            match cond {
                Some(c) => {
                    // Skip to the next branch when this condition is false (fused comparison branch).
                    let next = code.new_label();
                    // A condition that folds to a constant `false` never selects this branch, and the
                    // skip above is then an unconditional `goto next` — so the body would be laid down
                    // after it, unreachable and unframed ("Expecting a stack map frame"). The suspend
                    // flattener builds exactly that shape: a `do … while (false)` loop dragged into the
                    // state machine (by a labeled jump crossing out of it) becomes a header state whose
                    // `when` tests the literal `false` (see docs/SPEC.md). Emit nothing for it.
                    if self.emit_cond_branch(*c, next, false, code) {
                        // Skipping the CODE must not skip the merge-point accounting: `diverges` does
                        // not fold constant conditions, so a `when` whose only falling-through branch is
                        // this dead one still reports as falling through, and the caller keeps emitting
                        // at `end`. Leaving `end` unframed just moves the same VerifyError there —
                        // `if (FALSE_CONST) "a" else return "b"` failed at the merge instead of at the
                        // dead body. Mirror what the emitted path does, minus the code.
                        if !self.diverges(*body) {
                            end_reachable = true;
                        }
                        code.bind(next);
                        code.set_stack(entry_height);
                        continue;
                    }
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        // A diverging branch (e.g. an inlined `error(...)`) left nothing and ended in
                        // `athrow` — don't discard (nothing to pop) and don't jump to `end`.
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        // Only a falling-through branch jumps to (and needs a frame at) `end`.
                        self.frame(end, result_stack.clone(), code);
                        code.goto(end);
                        end_reachable = true;
                    }
                    code.bind(next);
                    // `next` is reached only via the conditional jump above, where the stack is back at the
                    // pre-branch baseline — reset the linear counter (the just-emitted branch body left its
                    // value on the counter, but not on this control path).
                    code.set_stack(entry_height);
                }
                None => {
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        end_reachable = true;
                    }
                    // The else is last — it falls through to `end` (no goto needed).
                }
            }
        }
        // Frame `end` only when it's actually reachable; if every branch diverges, `end` is dead
        // (no jump targets it) and a frame there would be "Expecting a stack map frame".
        if end_reachable {
            self.frame(end, result_stack, code);
        }
        code.bind(end);
    }

    /// `try { body } catch (v: E) { … } …` (no `finally`). The body value (and each catch value) is
    /// stored into a result temp, then loaded at the merge — mirroring kotlinc. The protected region
    /// `[start, end)` covers the body+store; each catch is an exception-table handler whose frame has
    /// the caught exception on the stack and the pre-`try` locals (the result temp/catch var read as
    /// `top` there, since an exception may occur before they are assigned).
    fn emit_try(
        &mut self,
        body: u32,
        catches: &[crate::ir::IrCatch],
        finally: Option<u32>,
        result: &Ty,
        code: &mut CodeBuilder,
    ) {
        let rt = ir_ty_to_jvm(result);
        let is_stmt = matches!(rt, Ty::Unit | Ty::Nothing);
        let result_slot = if is_stmt {
            None
        } else {
            let s = self.next_slot;
            self.next_slot += slot_words(rt);
            Some(s)
        };
        const RESULT_KEY: u32 = 3_000_000;
        // A `finally` that diverges (`finally { throw }`) never falls through to `after`.
        let fin_diverges = finally.map_or(false, |f| self.diverges(f));

        let start = code.new_label();
        let end = code.new_label();
        let after = code.new_label();

        code.bind(start);
        let body_diverges = self.diverges(body);
        if is_stmt || body_diverges {
            // Statement, or a diverging body (`throw`/`return`): no value reaches the result temp.
            self.emit(body, code);
        } else {
            self.emit_value(body, code);
            store(rt, result_slot.unwrap(), code);
        }
        code.bind(end);
        let mut after_reachable = false;
        if !body_diverges {
            if let Some(f) = finally {
                self.emit(f, code);
            } // `finally` inlined on the normal path
            if !fin_diverges {
                code.goto(after);
                after_reachable = true;
            }
        }

        // The `finally` catch-all must protect the body and each catch BODY, but NOT the inlined finally
        // code (normal-path, per-catch, or its own) — otherwise an exception thrown inside an inlined
        // finally re-enters the handler and the finally runs twice. Collect each catch body's range
        // (`[cbody_start, cbody_end)`, ending before that catch's inlined finally).
        let mut fin_ranges: Vec<(Label, Label)> = vec![(start, end)];
        for c in catches {
            let handler = code.new_label();
            // A handler is entered over the exception edge, not by a branch — and a diverging `try`
            // body leaves the stream dead exactly here, so binding must revive on the range it guards
            // rather than on an incoming branch.
            code.bind_handler(handler, &[(start, end)]);
            let exc_internal = c.exc_internal.render();
            let exc_ci = self.cw.class_ref(&exc_internal);
            // Handler entry: the exception is the sole stack value; locals are the pre-`try` state.
            self.frame(handler, vec![VerifType::Object(exc_ci)], code);
            let exc_ty = Ty::obj(&exc_internal);
            let cslot = self.next_slot;
            self.next_slot += 1;
            self.slots.insert(c.var, (cslot, exc_ty));
            store(exc_ty, cslot, code);
            let local_start =
                (code.bytes.len() <= u16::MAX as usize).then_some(code.bytes.len() as u16);
            let cbody_start = code.new_label();
            code.bind(cbody_start);
            let cbody_diverges = self.diverges(c.body);
            if is_stmt || cbody_diverges {
                self.emit(c.body, code);
            } else {
                self.emit_value(c.body, code);
                store(rt, result_slot.unwrap(), code);
            }
            self.slots.remove(&c.var);
            // The catch body is protected by the finally handler (a throw in a catch runs the finally),
            // but the catch's own inlined finally (below) is not.
            let cbody_end = code.new_label();
            code.bind(cbody_end);
            if self.record_locals {
                if let (Some(name), Some(start_pc)) = (c.name.as_deref(), local_start) {
                    let end_pc = code.bytes.len().min(u16::MAX as usize) as u16;
                    code.add_local_entry(
                        start_pc,
                        Some(end_pc.saturating_sub(start_pc)),
                        cslot,
                        name,
                        &local_variable_desc(exc_ty),
                    );
                }
            }
            if finally.is_some() {
                fin_ranges.push((cbody_start, cbody_end));
            }
            if !cbody_diverges {
                if let Some(f) = finally {
                    self.emit(f, code);
                } // `finally` inlined after the catch
                if !fin_diverges {
                    code.goto(after);
                    after_reachable = true;
                }
            }
            code.add_exception(start, end, handler, exc_ci);
        }

        // `finally` catch-all: any exception not handled above (in the body or a catch body) runs the
        // `finally` then re-throws. It protects only the body + catch bodies (`fin_ranges`), NOT the
        // inlined finally code — which lies past those ranges, so it doesn't re-catch itself.
        if let Some(f) = finally {
            let fin_handler = code.new_label();
            // Exception edge — see the `catch` handler above; this one guards the body and every
            // catch body (`fin_ranges`), which are complete by now.
            code.bind_handler(fin_handler, &fin_ranges);
            let thr_ci = self.cw.class_ref("java/lang/Throwable");
            self.frame(fin_handler, vec![VerifType::Object(thr_ci)], code);
            let thr_ty = Ty::obj("java/lang/Throwable");
            let tslot = self.next_slot;
            self.next_slot += 1;
            store(thr_ty, tslot, code);
            // The caught exception is LIVE in `tslot` across the whole inlined `finally` (it is re-raised
            // after it). Register it so any StackMapTable frame recorded WHILE emitting the finally —
            // e.g. a `finally` that itself contains a `try`/`catch` — lists `tslot` as an initialized
            // local; otherwise the trailing `aload tslot; athrow` reads a slot the verifier sees as `top`.
            // Keyed by the slot number (unique, and disjoint from small value indices) so nested catch-all
            // handlers each register their own live exception.
            let thr_key = 4_000_000 + tslot as u32;
            self.slots.insert(thr_key, (tslot, thr_ty));
            self.emit(f, code);
            self.slots.remove(&thr_key);
            // Re-raise the caught exception after the `finally` — unless the `finally` itself transfers
            // control (`finally { return … }` / `finally { throw … }`), in which case the rethrow is
            // unreachable and emitting it would leave a dead instruction without a stackmap frame.
            if !fin_diverges {
                load(thr_ty, tslot, code);
                code.athrow();
            }
            // `catch_type` 0 = catch-all (any throwable), matching kotlinc's `finally` table entry.
            for (rs, re) in fin_ranges {
                code.add_exception(rs, re, fin_handler, 0);
            }
        }

        if after_reachable {
            if let Some(slot) = result_slot {
                self.slots.insert(RESULT_KEY, (slot, rt));
            }
            self.frame(after, vec![], code);
            code.bind(after);
            if let Some(slot) = result_slot {
                load(rt, slot, code);
                self.slots.remove(&RESULT_KEY);
            }
        } else {
            // Every path diverges — `after` is dead; bind it so any stray reference resolves, but emit
            // no frame (nothing reaches it) and leave no value (the `try` is `Nothing`-typed).
            code.bind(after);
        }
    }

    /// Whether emitting `e` as a value always transfers control away (returns/throws), so control
    /// Resolve a `break`/`continue` target to `(continue_label, break_label)`. `None` → the innermost
    /// loop; `Some(l)` → the nearest enclosing loop carrying `l@`. Falls back to the innermost if the
    /// label isn't found (a compilable program always has the labeled loop in scope).
    fn loop_target(&self, label: &Option<String>) -> (Label, Label) {
        let entry = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl)| sl.as_deref() == Some(l.as_str()))
                .or_else(|| self.loop_stack.last()),
            None => self.loop_stack.last(),
        };
        let (cont, end, _) = entry.expect("break/continue outside loop");
        (*cont, *end)
    }

    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        if self
            .ir
            .logical_types
            .get(&e)
            .is_some_and(|ty| !ty.is_nullable() && ty.non_null() == Ty::Nothing)
        {
            return true;
        }
        self.ir.expr_diverges_by(e, &|expression, value| {
            matches!(value, IrExpr::Call { .. } | IrExpr::MethodCall { .. })
                && self.value_ty(expression) == Ty::Nothing
        })
    }

    /// The element `Ty` of an array-typed IR expression.
    fn array_elem(&self, e: u32) -> Ty {
        self.value_ty(e).array_elem().unwrap_or(Ty::Error)
    }

    fn value_ty_of_when(&self, branches: &[(Option<u32>, u32)]) -> Ty {
        // No `else` → the `when` is a Unit statement.
        if !branches.iter().any(|(c, _)| c.is_none()) {
            return Ty::Unit;
        }
        // The value type comes from a branch that *falls through* — a diverging branch (`else ->
        // return …`/`throw`) contributes nothing to the merge, so its `Unit`/`Nothing` must not make
        // the whole `when` look like a statement.
        let last = branches
            .iter()
            .rev()
            .find(|(_, b)| !self.diverges(*b))
            .map(|(_, b)| self.value_ty(*b))
            .unwrap_or(Ty::Unit);
        // A `null`/`Nothing` branch carries no concrete type and would verify-type the merge stack as
        // `top`; use a concrete fall-through branch type instead (`null` is assignable to any reference).
        if matches!(last, Ty::Null | Ty::Nothing | Ty::Error) {
            for (_, b) in branches {
                if self.diverges(*b) {
                    continue;
                }
                let t = self.value_ty(*b);
                if !matches!(t, Ty::Null | Ty::Nothing | Ty::Error) {
                    return t;
                }
            }
        }
        // When the falling-through branches are references of DIFFERENT classes (`if (c) Foo() else Bar()`,
        // joined by the checker to `Any`), the merge-point stack type must be a common supertype — krusty
        // uses `Object`. Each branch value is a subtype, so the merge frame (`Object`) verifies; the last
        // branch's own (more specific) class would mismatch the other predecessor's value (a VerifyError).
        if last.is_reference() {
            // Compare by the JVM internal name (`String` and `Obj("java/lang/String")` are the same type
            // but distinct `Ty` values), so only a genuinely differing class triggers the `Object` merge.
            let internal = |t: &Ty| -> Option<String> {
                match *t {
                    Ty::String => Some("java/lang/String".to_string()),
                    _ if t.is_array() => Some(type_descriptor(*t)),
                    Ty::Obj(n, _) => Some(n.to_string()),
                    _ => None,
                }
            };
            let mut names = branches
                .iter()
                .filter(|(_, b)| !self.diverges(*b))
                .map(|(_, b)| self.value_ty(*b))
                .filter(|t| !matches!(t, Ty::Null | Ty::Nothing | Ty::Error))
                .filter_map(|t| internal(&t));
            if let Some(first) = names.next() {
                if names.any(|n| n != first) {
                    return Ty::obj("kotlin/Any");
                }
            }
        }
        last
    }

    fn frame(&mut self, label: Label, stack: Vec<VerifType>, code: &mut CodeBuilder) {
        let locals = self.verif_locals();
        // Prepend any operand held on the stack below the current expression (an arithmetic LHS across a
        // branchy RHS), so the frame types the full operand stack the verifier sees.
        let stack = if self.pending_stack.is_empty() {
            stack
        } else {
            let mut full = self.pending_stack.clone();
            full.extend(stack);
            full
        };
        code.add_frame_if_new(label, locals, stack);
    }

    fn verif_locals(&mut self) -> Vec<VerifType> {
        self.verif_locals_with(&[])
    }

    fn verif_locals_with(&mut self, extra: &[(u16, Ty)]) -> Vec<VerifType> {
        let max = self.next_slot as usize;
        let mut raw = vec![VerifType::Top; max];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        for (slot, ty) in extra.iter().copied() {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        if self.this_uninitialized && !raw.is_empty() {
            raw[0] = VerifType::UninitializedThis;
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    }

    fn verif_single(&mut self, ty: Ty) -> VerifType {
        // Object types are recorded by NAME (`VerifType::ObjectName`), NOT interned here — the class is
        // interned only when a WRITTEN StackMapTable frame lists it (`build_stackmap`). A frame that
        // compresses to `same_frame` drops its locals, so a class appearing only in dropped frames (e.g.
        // a `copy$default` mask-branch param) never enters the pool — matching kotlinc, no orphan.
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
            Ty::Nullable(_) | Ty::PlatformNullable(_) => VerifType::ObjectName(ref_internal(ty)),
            Ty::Null => VerifType::Null,
            _ => VerifType::Top,
        }
    }

    fn verif_stack(&mut self, ty: Ty) -> Vec<VerifType> {
        match ty {
            Ty::Unit | Ty::Nothing | Ty::Error => vec![],
            _ => vec![self.verif_single(ty)],
        }
    }

    fn value_ty(&self, e: u32) -> Ty {
        match self.ir.expr(e) {
            IrExpr::StringConcat(_) => Ty::String,
            // A class literal `T::class` is a `java/lang/Class` constant — a reference, so `==`/`!=` on
            // two class literals routes to reference equality, not the primitive `if_icmpeq`.
            IrExpr::ClassConst { .. } => Ty::obj("java/lang/Class"),
            IrExpr::Const(c) => match c {
                IrConst::Boolean(_) => Ty::Boolean,
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
            IrExpr::GetField { class, index, .. } => {
                ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].ty)
            }
            IrExpr::PropertyRead { ty, .. } => {
                // A property read always yields a stored value. `Unit` therefore occupies the
                // `kotlin/Unit` reference slot; only a function's control-flow return uses `V`.
                ir_ty_to_jvm(&stored_value_ty(*ty))
            }
            // A write is a statement: it leaves nothing on the stack, so nothing is discarded after it.
            IrExpr::PropertyWrite { .. } => Ty::Unit,
            // A store leaves nothing on the stack; typing it as a value made statement position
            // emit a `pop` against an empty stack (`VerifyError: Operand stack underflow`).
            IrExpr::SetExternalStaticField { .. } => Ty::Unit,
            IrExpr::GetStatic(i) => ir_ty_to_jvm(&self.ir.statics[*i as usize].ty),
            IrExpr::New { internal, .. } => Ty::obj_name(*internal),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                call_ret_ty(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(fid)
                | Callee::LocalDefault(fid)
                | Callee::ClassStatic { function: fid, .. } => {
                    call_ret_ty(&self.ir.functions[*fid as usize].ret)
                }
                Callee::CrossFile { ret, .. } => call_ret_ty(ret),
                Callee::Intrinsic { ret, .. } => call_ret_ty(ret),
                Callee::Static { owner, name, .. } if name == "box-impl" => {
                    Ty::nullable(Ty::obj_name(*owner))
                }
                Callee::Static { descriptor, .. } | Callee::Special { descriptor, .. } => {
                    // A kotlin `Nothing` return is a `java/lang/Void` JVM descriptor — report it as
                    // `Nothing` so a diverging (inlined `error(...)`) call is treated as never returning
                    // (no value, no dead epilogue after the spliced `athrow`).
                    if descriptor.ends_with(")Ljava/lang/Void;") {
                        Ty::Nothing
                    } else {
                        ty_from_descriptor_ret(descriptor)
                    }
                }
                // A user (sibling-file) method carries its return as a `Ty`; a classpath one via descriptor.
                Callee::Virtual {
                    descriptor, params, ..
                } => match params {
                    Some((_, ret)) => call_ret_ty(ret),
                    None if descriptor.ends_with(")Ljava/lang/Void;") => Ty::Nothing,
                    None => ty_from_descriptor_ret(descriptor),
                },
            },
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
                    boxed_prim_of(t).unwrap_or(t)
                }
            },
            IrExpr::PrimitiveNeg { ty, .. } => ir_ty_to_jvm(ty),
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::EnumEntry { class, .. } | IrExpr::EnumValueOf { class, .. } => {
                Ty::obj(&self.ir.classes[*class as usize].fq_name())
            }
            IrExpr::StaticInstance { ty, .. } => Ty::obj(&self.ir.classes[*ty as usize].fq_name()),
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
            IrExpr::EnumValues { class } => {
                Ty::array(Ty::obj(&self.ir.classes[*class as usize].fq_name()))
            }
            IrExpr::ReifiedClassMarker { .. } => Ty::obj("java/lang/Class"),
            IrExpr::ReifiedTypeOp { cast, erased, .. } => {
                if *cast {
                    Ty::obj_name(*erased)
                } else {
                    Ty::Boolean
                }
            }
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            IrExpr::Lambda { arity, .. } => {
                Ty::obj(&format!("kotlin/jvm/functions/Function{arity}"))
            }
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            IrExpr::NotNullAssert { operand } => self.value_ty(*operand),
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
fn descriptor_is_reference(descriptor: &str) -> bool {
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

fn emit_num_conv(from: Ty, to: Ty, code: &mut CodeBuilder) {
    if from == to {
        return;
    }
    let wide = |t: Ty| match t {
        Ty::Byte | Ty::Short | Ty::Char | Ty::Int => Ty::Int,
        o => o,
    };
    match (wide(from), wide(to)) {
        (Ty::Int, Ty::Long) => code.i2l(),
        (Ty::Int, Ty::Float) => code.i2f(),
        (Ty::Int, Ty::Double) => code.i2d(),
        (Ty::Long, Ty::Int) => code.l2i(),
        (Ty::Long, Ty::Float) => code.l2f(),
        (Ty::Long, Ty::Double) => code.l2d(),
        (Ty::Float, Ty::Int) => code.f2i(),
        (Ty::Float, Ty::Long) => code.f2l(),
        (Ty::Float, Ty::Double) => code.f2d(),
        (Ty::Double, Ty::Int) => code.d2i(),
        (Ty::Double, Ty::Long) => code.d2l(),
        (Ty::Double, Ty::Float) => code.d2f(),
        _ => {} // same wide category (e.g. Byte→Int): the value is already correct on the stack
    }
    match to {
        Ty::Byte => code.i2b(),
        Ty::Short => code.i2s(),
        Ty::Char => code.i2c(),
        _ => {}
    }
}

fn ref_internal(t: Ty) -> String {
    match t {
        Ty::String => "java/lang/String".to_string(),
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) if inner.is_unsigned() => match *inner {
            Ty::UByte => "kotlin/UByte".to_string(),
            Ty::UShort => "kotlin/UShort".to_string(),
            Ty::UInt => "kotlin/UInt".to_string(),
            Ty::ULong => "kotlin/ULong".to_string(),
            _ => unreachable!("is_unsigned accepts only the four unsigned scalar types"),
        },
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner
            .boxed_ref()
            .and_then(Ty::obj_internal)
            .map(|name| crate::jvm::names::classfile_internal_name(&name.render()))
            .unwrap_or_else(|| ref_internal(*inner)),
        // An array's reference identity is its descriptor (`[I`, `[Ljava/lang/String;`) — checked before
        // the `Obj` arm since arrays are now `Obj("kotlin/Array")`/`Obj("kotlin/IntArray")` too.
        t if t.is_array() => type_descriptor(t),
        // Erase a Kotlin built-in name (`kotlin/collections/MutableList`) to its JVM identity here at the
        // bytecode boundary, so `instanceof`/`checkcast`/method-owner refs never leak a Kotlin-only name.
        Ty::Obj(n, _) => crate::jvm::names::classfile_internal_name(&n.render()),
        // A function type's reference identity is its `kotlin/jvm/functions/FunctionN` interface, so
        // `x is Function1<*, *>` / `x as (A) -> B` test/cast against that class, not `Object`.
        Ty::Fun(_) => t
            .function_interface_internal()
            .unwrap_or("java/lang/Object")
            .to_string(),
        _ => "java/lang/Object".to_string(),
    }
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
/// If `t` is the boxed-reference form of a primitive (the element of a `Array<Int>` etc., carried as
/// `Obj("kotlin/Int")`), the underlying primitive `Ty`. Used to insert box/unbox at the boxed-array
/// element boundary (`a[i]` yields an unboxed `Int`; `a[i] = v` boxes the `Int`).
fn boxed_prim_of(t: Ty) -> Option<Ty> {
    t.unboxed_primitive()
}

fn array_load_op(elem: Ty, reference_array: bool) -> (u8, i32) {
    if reference_array {
        return (0x32, 1);
    }
    match elem {
        // Unsigned arrays are the unboxed underlying primitive array (`UIntArray` = `[I`,
        // `ULongArray` = `[J`), so they load with `iaload`/`laload`.
        Ty::Int | Ty::UInt => (0x2e, 1),
        Ty::Long | Ty::ULong => (0x2f, 2),
        Ty::Float => (0x30, 1),
        Ty::Double => (0x31, 2),
        Ty::Boolean | Ty::Byte => (0x33, 1),
        Ty::Char => (0x34, 1),
        Ty::Short => (0x35, 1),
        _ => (0x32, 1), // aaload
    }
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

fn array_store_op(elem: Ty, reference_array: bool) -> (u8, i32) {
    if reference_array {
        return (0x53, 1);
    }
    match elem {
        // Unsigned arrays store into the unboxed underlying primitive array (`[I`/`[J`).
        Ty::Int | Ty::UInt => (0x4f, 1),
        Ty::Long | Ty::ULong => (0x50, 2),
        Ty::Float => (0x51, 1),
        Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1),
        Ty::Char => (0x55, 1),
        Ty::Short => (0x56, 1),
        _ => (0x53, 1), // aastore
    }
}

/// `newarray` atype for a primitive element (JVMS Table 6.5.newarray-A).
fn prim_newarray_atype(elem: Ty) -> u8 {
    match elem {
        Ty::Boolean => 4,
        Ty::Char => 5,
        Ty::Float => 6,
        Ty::Double => 7,
        Ty::Byte => 8,
        Ty::Short => 9,
        Ty::Long => 11,
        _ => 10, // int
    }
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

pub fn ir_ty_to_jvm(t: &Ty) -> Ty {
    // A nullable PRIMITIVE is a JVM reference — its boxed wrapper (`Int?` → `java/lang/Integer`, a
    // 1-slot reference), NOT the unboxed scalar. Map it before peeling `?`, so descriptors, slots and
    // stackmap frames all see the reference. A nullable REFERENCE keeps its descriptor (peel below).
    if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = t {
        if **inner == Ty::Nothing {
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
        // Bare scalar/`String` variants are already JVM types — pass through. (Front-end/`ir_lower` types
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
        // semantics live in the intrinsic calls (`Integer.compareUnsigned`, …) ir_lower already inserted.
        Ty::UByte => Ty::Byte,
        Ty::UShort => Ty::Short,
        Ty::UInt => Ty::Int,
        Ty::ULong => Ty::Long,
        Ty::Obj(fq_name, type_args) => match () {
            _ if fq_name.matches("kotlin/Int") => Ty::Int,
            _ if fq_name.matches("kotlin/Long") => Ty::Long,
            _ if fq_name.matches("kotlin/Short") => Ty::Short,
            _ if fq_name.matches("kotlin/Byte") => Ty::Byte,
            _ if fq_name.matches("kotlin/Boolean") => Ty::Boolean,
            _ if fq_name.matches("kotlin/Char") => Ty::Char,
            _ if fq_name.matches("kotlin/Double") => Ty::Double,
            _ if fq_name.matches("kotlin/Float") => Ty::Float,
            _ if fq_name.matches("kotlin/String") => Ty::String,
            // Arrays are regular class types the JVM backend lowers to JVM array types here.
            _ if fq_name.matches("kotlin/IntArray") => Ty::array(Ty::Int),
            _ if fq_name.matches("kotlin/LongArray") => Ty::array(Ty::Long),
            _ if fq_name.matches("kotlin/DoubleArray") => Ty::array(Ty::Double),
            _ if fq_name.matches("kotlin/FloatArray") => Ty::array(Ty::Float),
            _ if fq_name.matches("kotlin/BooleanArray") => Ty::array(Ty::Boolean),
            _ if fq_name.matches("kotlin/CharArray") => Ty::array(Ty::Char),
            _ if fq_name.matches("kotlin/ByteArray") => Ty::array(Ty::Byte),
            _ if fq_name.matches("kotlin/ShortArray") => Ty::array(Ty::Short),
            // Unsigned arrays are `inline class`es over the signed primitive array; at the JVM level they
            // ARE that array (`UIntArray` = `[I`). The unsigned element semantics are a source/checker
            // concern already resolved before emit, so collapse to the physical signed array here.
            _ if fq_name.matches("kotlin/UIntArray") => Ty::array(Ty::Int),
            _ if fq_name.matches("kotlin/ULongArray") => Ty::array(Ty::Long),
            // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
            // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
            _ if fq_name.matches("kotlin/Array") => Ty::array(
                type_args
                    .first()
                    .map(|e| {
                        // A projection is valid here as the ARRAY classifier's type argument, even
                        // though it is never a value type of its own. Erase it at this boundary:
                        // `out X` has the readable element `X`; `in X` can only be read as `Any`.
                        let et = match e.non_null() {
                            Ty::OutProjection(inner) => ir_ty_to_jvm(inner),
                            Ty::InProjection(_) => Ty::obj("kotlin/Any"),
                            _ => ir_ty_to_jvm(e),
                        };
                        let boxed = reference_array_element(et);
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
        },
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`. A `suspend`
        // function type carries a trailing `Continuation` parameter, so its arity is one greater.
        Ty::Fun(s) => Ty::obj(&format!(
            "kotlin/jvm/functions/Function{}",
            s.params.len() + usize::from(s.suspend)
        )),
        // JVM erasure of a type parameter: collapse `T` to its declared upper bound (which itself
        // erases to `java/lang/Object` for an `Any` bound). This is the ONE place `T` becomes a
        // concrete JVM type.
        Ty::TyParam(_, bound) => ir_ty_to_jvm(bound),
        _ => Ty::Error,
    }
}

pub(crate) fn jvm_tys(tys: &[Ty]) -> Vec<Ty> {
    tys.iter()
        .map(|t| match ir_ty_to_jvm(t) {
            Ty::Nothing => Ty::obj("kotlin/Any"),
            other => other,
        })
        .collect()
}

/// Whether a JVM type is an ERASED TOP reference — the `java/lang/Object` a type parameter erases to, or
/// an `Object[]` a generic `Array<T>` erases to (recursively). A value of this type is a candidate for the
/// narrowing `checkcast` at a consumption site; a concrete type (`String`, `Integer`, `IntArray`, a value
/// class) is not.
fn jvm_is_erased_top(t: Ty) -> bool {
    match t.obj_internal() {
        Some(n) if n.matches("java/lang/Object") || n.matches("kotlin/Any") => true,
        _ => t.array_elem().is_some_and(jvm_is_erased_top),
    }
}

fn ir_type_desc(t: &Ty) -> String {
    type_descriptor(ir_ty_to_jvm(t))
}

fn local_variable_desc(t: Ty) -> String {
    type_descriptor(if t == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        t
    })
}

fn ir_method_desc(params: &[Ty], ret: &Ty) -> String {
    method_descriptor(&jvm_tys(params), ir_ty_to_jvm(ret))
}

fn field_jvm_tys(fields: &[IrField]) -> Vec<Ty> {
    fields.iter().map(|f| ir_ty_to_jvm(&f.ty)).collect()
}

fn ctor_arg_jvm_tys(args: &[IrCtorArg]) -> Vec<Ty> {
    args.iter().map(|a| ir_ty_to_jvm(&a.ty)).collect()
}

fn class_ctor_jvm_tys(c: &IrClass) -> Vec<Ty> {
    if c.ctor_args.is_empty() {
        field_jvm_tys(&c.fields[..c.ctor_param_count as usize])
    } else {
        ctor_arg_jvm_tys(&c.ctor_args)
    }
}

fn super_ctor_jvm_tys(
    ir: &IrFile,
    c: &IrClass,
    superclass: &str,
    mut value_ty: impl FnMut(u32) -> Ty,
) -> (Vec<Ty>, bool) {
    let mut params = if crate::jvm::jvm_class_map::to_jvm_internal(superclass) == "java/lang/Object"
    {
        Vec::new()
    } else if let Some(sc) = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name_matches(superclass))
    {
        class_ctor_jvm_tys(sc)
    } else {
        c.super_args.iter().map(|&arg| value_ty(arg)).collect()
    };
    if params.is_empty() && !c.super_args.is_empty() {
        params = c.super_args.iter().map(|&arg| value_ty(arg)).collect();
    }
    let uses_accessor = ir.has_value_param_ctor(superclass)
        || ir
            .classes
            .iter()
            .any(|candidate| candidate.fq_name_matches(superclass) && candidate.is_sealed);
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

/// `true` if a lowered IR type is a nullable reference (`String?` etc.).
fn ir_ty_nullable(t: &Ty) -> bool {
    t.is_nullable()
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

fn wrapper_owner_primitive(owner: &str) -> Option<Ty> {
    Some(match owner {
        "java/lang/Integer" | "kotlin/Int" => Ty::Int,
        "java/lang/Long" | "kotlin/Long" => Ty::Long,
        "java/lang/Double" | "kotlin/Double" => Ty::Double,
        "java/lang/Float" | "kotlin/Float" => Ty::Float,
        "java/lang/Boolean" | "kotlin/Boolean" => Ty::Boolean,
        "java/lang/Character" | "kotlin/Char" => Ty::Char,
        "java/lang/Byte" | "kotlin/Byte" => Ty::Byte,
        "java/lang/Short" | "kotlin/Short" => Ty::Short,
        _ => return None,
    })
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
mod fail_soft_tests {
    use super::*;
    use crate::ir::{Callee, IrExpr, IrFile, IrFunction};
    use crate::jvm::classreader::MethodCode;
    use crate::jvm::inline::MethodBodies;
    use crate::types::Ty;

    struct NoBodies;
    impl MethodBodies for NoBodies {
        fn body(&self, _o: &str, _n: &str, _d: &str) -> Option<MethodCode> {
            None
        }
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
        anonymous.enclosing_function = Some(selected);

        let (resolved_owner, function) =
            anonymous_scope(&ir, &anonymous, "IgnoredFacade").expect("bound enclosure");
        assert_eq!(resolved_owner, "demo/Owner");
        assert_eq!(
            ir_method_desc(&function.params, &function.ret),
            "(Ljava/lang/String;)V"
        );
    }

    #[test]
    fn nested_metadata_uses_declaration_identity_not_generated_name_shape() {
        let mut ir = IrFile::default();
        let mut outer = crate::plugins::synthetic_class("demo/Outer");
        outer.is_source_declared = true;
        let outer_id = ir.add_class(outer);

        // A digit is valid in a source identifier; the former generated-name heuristic dropped it.
        let mut declared = crate::plugins::synthetic_class("demo/Outer$Node2");
        declared.is_source_declared = true;
        ir.add_class(declared);

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

    // A `GetValue` of a value slot that was never allocated is malformed IR (e.g. an unsupported
    // suspend shape the lowering should have bailed on). The emitter must SKIP the file
    // (`emit_all` -> `None`), never panic — a compiler must not crash on its own IR.
    #[test]
    fn getvalue_of_unallocated_slot_skips_not_panics() {
        let symbols = crate::frontend::FrontendSymbols::default();
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
        assert!(emit_all(&ir, "TestKt", &NoBodies, None, &symbols).is_none());
    }

    #[test]
    fn arity_failure_exposes_category_without_owner_or_callable_name() {
        let symbols = crate::frontend::FrontendSymbols::default();
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: None,
        });
        let callee = ir.add_fun(IrFunction {
            name: "realCallableName".into(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: Some(unit),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        let mismatched_call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(callee),
            dispatch_receiver: None,
            args: vec![],
        });
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(mismatched_call),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });

        // The trace may identify `SensitiveFacade.realCallableName`, but the result read by the CLI
        // and survey is deliberately a stable category with neither source nor JVM owner spelling.
        let run = EmitRun::default();
        assert!(emit_all_with_opts(
            &ir,
            "SensitiveFacade",
            &NoBodies,
            None,
            &EmitOptions::default(),
            &run,
            &symbols,
        )
        .is_none());
        assert_eq!(run.inline_bail().as_deref(), Some("call arity mismatch"));
    }
}
