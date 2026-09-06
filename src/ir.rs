//! `krusty-ir` — the backend-agnostic, typed common IR.
//!
//! This is the shared layer between the front end (lex/parse/resolve) and the platform backends
//! (JVM today; WASM/JS future — see `docs/ARCHITECTURE.md`). It deliberately mirrors the **Kotlin
//! IR** node taxonomy (`IrClass`/`IrFunction`/`IrCall`/`IrWhen`/…) rather than inventing a novel
//! design, and it is **not** a low-level IR like LLVM — the JVM/JS/WASM targets are managed VMs that
//! need Kotlin's types, nullability, and object model preserved (which LLVM/MLIR discard too early).
//!
//! Representation choices (primitive vs boxed, erasure, calling conventions) are **not** encoded
//! here — they are decided by each backend's lowering of these nodes. Types are expressed in Kotlin
//! terms (`Ty`), never JVM descriptors.
//!
//! Storage follows krusty's index-based invariant: nodes live in parallel `Vec` arenas keyed by
//! `u32` ids (no `Box`/`Rc` graphs; bulk-freeable). Lowering (`ast → ir`) and the JVM backend
//! consuming IR are the next phases; today this module defines the node set + a builder + a printer.

use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName, TypeNameList};

pub type ExprId = u32;
pub type FunId = u32;
pub type ClassId = u32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrNodeOrigin {
    Fir(crate::fir::OriginId),
    Synthetic {
        cause: crate::fir::OriginId,
        kind: crate::fir::SyntheticOriginKind,
    },
}

/// A compiler-supplied operation selected from a real semantic declaration. This is an operation
/// identity, not a library name: backends implement it without recovering signature facts from text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrIntrinsic {
    /// Kotlin's checked `assert` operation. Arguments are the Boolean condition followed by its
    /// optional zero-argument message function. A backend must guard/elide the whole operation
    /// before evaluating either child according to `mode`.
    Assert {
        mode: crate::types::AssertionMode,
    },
    ArrayGet,
    ArraySet,
    ArraySize,
    StringGet,
    StringLength,
    StringPlus,
    NullableAnyToString,
    /// Kotlin's compiler-supplied `enumValueOf<T>(name)`. `classifier` may remain a declaration-owned
    /// reified type parameter in the emitted inline template; call-site inline specialization turns
    /// it into the exact enum classifier without reopening resolution.
    EnumValueOf {
        classifier: Ty,
    },
    /// Result of an exact builtin scalar `compareTo` declaration. `operand` is the semantic common
    /// carrier selected by the frontend, not a JVM descriptor type.
    PrimitiveCompare {
        operand: Ty,
    },
    /// Read the context from the current suspend continuation. The JVM coroutine pass replaces
    /// this operation with the continuation parameter's `Continuation.getContext()` call.
    CoroutineContext,
    UnsignedToString {
        source: Ty,
    },
    PrimitiveArrayNew {
        element: Ty,
    },
    /// Kotlin data-class equality for one primary-constructor property. Backends preserve Kotlin's
    /// scalar, floating-point, nullable, array-reference, and value-class equality semantics.
    DataClassFieldEquals {
        ty: Ty,
    },
    /// Kotlin data-class hash contribution for one primary-constructor property.
    DataClassFieldHash {
        ty: Ty,
    },
    /// Kotlin's content rendering for an array stored in a data-class property.
    DataClassArrayToString {
        ty: Ty,
    },
}

/// The target of an `IrExpr::Call`. `Local` references a function defined in this IR file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    Local(FunId),
    /// A static method defined on a class in this IR file. Unlike [`Callee::Local`], whose owner is
    /// the file facade, this carries the declaring class explicitly. The `FunId` keeps the call tied
    /// to the function's semantic parameter/return types through backend ABI transformations.
    ClassStatic {
        owner: TypeName,
        function: FunId,
    },
    /// A checked call to a static source function with omitted semantic parameters. The argument
    /// vector contains only supplied values in declaration order; defaults names omitted ordinals.
    /// A backend chooses its own default-argument calling convention.
    ClassStaticWithDefaults {
        owner: TypeName,
        function: FunId,
        defaults: Box<[u32]>,
    },
    /// `$default` companion of a static function owned by a class in this IR file.
    ClassStaticDefault {
        owner: TypeName,
        function: FunId,
    },
    /// The `$default` synthetic of a same-file top-level function/extension (`FunId`) — emitted as
    /// `invokestatic <facade>.<name>$default(realparams, int mask, Object marker)ret`. Like `Local` the
    /// facade is resolved at emit (`self.facade`); the descriptor appends the trailing `I` mask +
    /// `Object` marker to the real function's parameters. Used when a call omits a (possibly non-const)
    /// defaulted argument, mirroring kotlinc's default-argument ABI.
    LocalDefault(FunId),
    /// A checked call to a source function in this IR file with omitted semantic parameters.
    /// The argument vector contains only supplied values in declaration order.
    LocalWithDefaults {
        function: FunId,
        defaults: Box<[u32]>,
    },
    Intrinsic {
        operation: IrIntrinsic,
        ret: Ty,
    },
    /// A top-level function defined in ANOTHER source file of the same multi-file compilation —
    /// `invokestatic <facade>.<name>(params)ret`. Carries the signature as backend-agnostic `Ty`s
    /// (the JVM backend builds the descriptor), so `ir_lower` needn't know JVM descriptors. Distinct
    /// from `Local` (same IrFile, by index) and `Static` (a resolved classpath/library method).
    CrossFile {
        facade: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        /// Exact current-module declaration that produced this realized cross-file edge. Legacy
        /// common lowering may construct a physical cross-file call directly and therefore has no
        /// stable declaration identity.
        module_target: Option<crate::fir::CallableId>,
        /// Whether this edge invokes the declaration's default-argument synthetic. Kept separate
        /// from the JVM spelling so representation passes never infer semantics from `$default`.
        module_default_call: bool,
    },
    /// A top-level callable in another source unit of the current module. The stable target is
    /// backend-neutral; a module realization pass maps its declaring [`SourceFileId`](crate::fir::SourceFileId)
    /// to the target's physical file container without repeating semantic selection.
    Module {
        target: crate::fir::CallableId,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// A checked same-module call with omitted semantic parameters. Unlike Module, this form
    /// contains no target ABI masks, marker parameter, synthetic name, or placeholder values.
    /// The argument vector contains supplied values in declaration order; defaults identifies holes.
    ModuleWithDefaults {
        target: crate::fir::CallableId,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        defaults: Box<[u32]>,
        /// Semantic dispatch-receiver type for a member default call. It remains separate from the
        /// declared parameter list and is transformed before a backend places that receiver in its
        /// physical default-call convention.
        dispatch_receiver_ty: Option<Ty>,
        /// Checked position of an extension receiver in params. A backend that numbers default
        /// masks independently from the receiver consumes this fact directly.
        extension_receiver_parameter: Option<u32>,
    },
    /// A dependency callable selected by the frontend. The opaque declaration identity is realized
    /// by the target provider after common lowering; semantic parameters/results remain available to
    /// backend-neutral passes without exposing an owner or descriptor.
    External {
        target: crate::fir::ExternalCallableId,
        /// Dependency declaration that supplies inherited defaults for this selected target.
        /// Present only as an opaque checked identity; a backend owns its physical realization.
        default_provider: Option<crate::fir::ExternalCallableId>,
        params: Vec<Ty>,
        ret: Ty,
        /// Final checked type substitutions, keyed by the provider-owned declaration parameter.
        /// A target backend may translate the stable ordinal to its physical metadata name; no
        /// common-IR consumer performs lookup or inference from these values.
        substitutions: Vec<IrCheckedSubstitution>,
        /// Final semantic parameter ordinals omitted at the source call site. A target backend uses
        /// its provider-owned default bridge; common lowering never reconstructs that ABI.
        defaults: Vec<u32>,
        /// Checked position of a member-extension receiver in `params`/`args`. Default-mask
        /// ordinals exclude this receiver; target realization consumes this exact semantic fact
        /// instead of inferring source shape from a physical provider descriptor.
        extension_receiver_parameter: Option<u32>,
    },
    /// A resolved classpath static method — `invokestatic owner.name:descriptor`. Used for stdlib
    /// extension/top-level functions resolved from the classpath (`StringsKt.repeat`, `RangesKt.until`),
    /// carrying the exact JVM descriptor so no name is hardcoded in the backend.
    /// `inline` carries the callee's inline-ness in one field (was `inline` + `must_inline`):
    /// [`InlineKind::CanInline`] => a Kotlin `inline` function whose compiled body the JVM backend may
    /// splice here instead of emitting the `invokestatic`; [`InlineKind::MustInline`] => a NON-PUBLIC
    /// `@InlineOnly` callee (`require`/`check`/`error`) with no legal `invokestatic` fallback, so the
    /// backend MUST splice the body (a body it can't splice — e.g. branchy on a non-empty operand stack —
    /// skips the whole file, never miscompiled).
    Static {
        owner: TypeName,
        name: String,
        descriptor: String,
        inline: InlineKind,
    },
    /// An instance method (or property accessor) — `invokevirtual`/`invokeinterface owner.name(sig)` on
    /// the `dispatch_receiver`; `interface` ⇒ `invokeinterface`. `owner` is the receiver's static type.
    /// The SOLE virtual-dispatch callee (classpath, same-file, and sibling-file all unified — no
    /// cp/module/local split). The descriptor comes from ONE source, like [`IrExpr::New`]:
    /// - `params: Some((param_tys, ret))` — a user method (typically sibling-file) whose descriptor the
    ///   JVM backend builds from `Ty`s, and whose value-class name-mangle/erasure the pass applies.
    /// - else `descriptor` — a verbatim JVM descriptor (classpath, or an already-resolved same-file form).
    Virtual {
        owner: TypeName,
        name: String,
        descriptor: String,
        params: Option<(Vec<Ty>, Ty)>,
        interface: bool,
    },
    /// A non-virtual instance call — `invokespecial owner.name:descriptor` on the `dispatch_receiver`.
    /// Used for `super.method(…)`, which dispatches to the named base-class method directly (skipping the
    /// receiver's override). `owner` is the base class declaring the method.
    /// A `super`-qualified call before target realization: the checker fixed one supertype
    /// declaration and dispatch is non-virtual, but the physical descriptor and whether the body
    /// lives in a JVM-default holder are target choices. `jvm::module_calls` realizes this into
    /// [`Callee::Special`].
    Super {
        owner: TypeName,
        /// Exact classifier whose instance supplies the nonvirtual dispatch receiver.
        dispatch_owner: TypeName,
        /// The call appears in a different lexical classifier and therefore needs a target-specific
        /// owner bridge; emitting `invokespecial` directly from the inner class is verifier-invalid.
        enclosing_dispatch: bool,
        kind: crate::fir::FirSuperCallKind,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        interface: bool,
        /// Exact provider realization selected before common lowering. Targets consume this opaque
        /// fact; they do not rediscover a holder/static shape from owner spellings.
        realization: crate::libraries::MemberRealization,
        /// Provider-owned physical descriptor when one exists; source declarations leave it empty
        /// and the target backend derives its ABI from `params`/`ret`.
        descriptor: String,
        /// Exact source callable owning checked default expressions, when this super declaration is
        /// part of the current module.
        source: Option<crate::fir::CallableId>,
        /// Final semantic parameter ordinals omitted at the checked call site.
        defaults: Vec<u32>,
        source_member: Option<crate::libraries::SourceMember>,
    },
    Special {
        owner: TypeName,
        name: String,
        descriptor: String,
        /// `owner` is an INTERFACE (a diamond `super.f()` dispatched to a superinterface's DEFAULT method):
        /// the method reference must be an `InterfaceMethodref` and the call an `invokespecial` on it.
        interface: bool,
        /// Exact source declaration selected for a current-compilation interface body. A JVM backend
        /// may relocate that body according to the requested output mode; dependency realizations
        /// arrive as `Callee::Static` instead and leave this unset.
        source_member: Option<crate::libraries::SourceMember>,
        /// Stable current-module callable when this special call realizes a source declaration.
        /// This remains present in compact-header Pass 2 even when the legacy source-member
        /// coordinate is deliberately absent.
        source: Option<crate::fir::CallableId>,
    },
}

impl Callee {
    /// The function declaration stored in this IR file that owns this call's semantic signature.
    ///
    /// Default-dispatch calls still point at the source function; only their emitted entry point is
    /// synthetic. Keeping that classification here prevents every IR consumer from maintaining its
    /// own list of local/static and ordinary/default variants.
    pub(crate) fn source_function(&self) -> Option<FunId> {
        match self {
            Callee::Local(function)
            | Callee::LocalWithDefaults { function, .. }
            | Callee::LocalDefault(function)
            | Callee::ClassStatic { function, .. }
            | Callee::ClassStaticWithDefaults { function, .. }
            | Callee::ClassStaticDefault { function, .. } => Some(*function),
            _ => None,
        }
    }
}

/// A compile-time constant (`IrConst` in Kotlin IR).
#[derive(Clone, Debug, PartialEq)]
pub enum IrConst {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// A Kotlin `Char` — one UTF-16 code UNIT, not a code point. Lone surrogates (D800..DFFF) are
    /// legal `Char` values (`Char.MIN_HIGH_SURROGATE`), so this cannot be a Rust `char`: converting
    /// through `char::from_u32` rejects them and silently folds them to NUL.
    Char(u16),
    /// A Kotlin `String` — a sequence of UTF-16 code units. Same reason as `Char`: `"\uD800"` and
    /// `"😀"` have no Rust `String` spelling one code unit at a time.
    String(crate::kt_string::KtString),
    Null,
}

/// One checker-selected argument after source-order evaluation has been preserved. Parameter
/// ordinals and omitted/default/vararg decisions are semantic facts; no later phase remaps them.
#[derive(Clone, Debug, PartialEq)]
pub enum IrCheckedArgument {
    Expression {
        parameter: u32,
        value: ExprId,
    },
    Default {
        parameter: u32,
    },
    Vararg {
        parameter: u32,
        array_type: Ty,
        elements: Vec<(ExprId, bool)>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrCheckedSubstitution {
    pub parameter: crate::fir::FirTypeParameterRef,
    pub value: Ty,
    pub additional_bounds: Vec<Ty>,
}

/// One source constructor after checked FIR has been consumed. The stable declaration ordinal
/// distinguishes the primary constructor (`0`) from secondary constructors (`1..`). Delegation is
/// retained as an already-selected checked operation; no later phase may repeat constructor lookup
/// or argument mapping.
#[derive(Clone, Debug)]
pub struct IrCheckedConstructorBody {
    pub class: ClassId,
    pub ordinal: u32,
    /// Fully checked annotations on this source constructor. The transient Pass-2 declaration
    /// metadata handoff attaches them by stable declaration identity before finalization turns a
    /// secondary constructor into its durable common-IR form.
    pub annotations: DeclarationAnnotations,
    pub parameters: Vec<(String, Ty)>,
    pub defaults: Vec<Option<ExprId>>,
    pub delegation: Option<ExprId>,
    pub body: Option<ExprId>,
    /// The ordinary constructor FIR has been consumed. Signature defaults may be attached to the
    /// predeclared constructor before this becomes true.
    pub body_attached: bool,
}

/// One source property declaration and the checked bodies that realize its language semantics.
/// Storage and accessor naming remain backend choices; stable property/class identities and final
/// types are already fixed here.
#[derive(Clone, Debug)]
pub struct IrCheckedProperty {
    pub declaration: crate::fir::DeclarationId,
    /// Source declaration line accepted while the bounded Pass-2 syntax unit is live. This is
    /// output metadata, not a source locator: property realization copies it to the common-IR
    /// declarations it creates after the syntax unit has already been dropped.
    pub decl_line: u32,
    /// Exact semantic position among the owning class's property initializers and `init` blocks.
    /// This is copied from the stable FIR declaration header, never reconstructed from source.
    pub initialization_order: Option<u32>,
    pub class: Option<ClassId>,
    pub name: String,
    pub ty: Ty,
    /// Checked explicit backing-field type, distinct from the public property/accessor type.
    pub storage_ty: Option<Ty>,
    pub visibility: crate::types::Visibility,
    pub flags: crate::fir::DeclarationFlags,
    pub initializer: Option<ExprId>,
    pub delegate: Option<ExprId>,
    pub delegate_plan: Option<crate::fir::FirPropertyDelegatePlan>,
    pub getter: Option<ExprId>,
    pub setter: Option<ExprId>,
}

/// Common-IR declaration layout for a source property. This records which semantic storage and
/// accessor declarations were materialized, but deliberately does not choose how an ordinary
/// property access uses them. That choice belongs to the target realization pass.
#[derive(Clone, Debug)]
pub enum IrLocalPropertyLayout {
    TopLevelStorage {
        storage: u32,
        getter: Option<FunId>,
        setter: Option<FunId>,
        /// Semantic singleton qualifier for a classifier-associated constant. `None` denotes a
        /// genuinely receiverless package property.
        qualifier: Option<TypeName>,
    },
    TopLevelAccessor {
        getter: FunId,
        setter: Option<FunId>,
        receiver: Option<Ty>,
        context_parameters: Vec<Ty>,
    },
    Member {
        class: ClassId,
        owner: TypeName,
        backing_field: Option<u32>,
        getter: Option<FunId>,
        setter: Option<FunId>,
        interface: bool,
        name: String,
        ty: Ty,
        mutable: bool,
        private: bool,
        context_parameters: Vec<Ty>,
        property: u32,
    },
    MemberExtension {
        owner: TypeName,
        interface: bool,
        name: String,
        getter: FunId,
        setter: Option<FunId>,
        receiver: Ty,
        ty: Ty,
        context_parameters: Vec<Ty>,
    },
}

#[derive(Clone, Debug)]
pub struct IrCheckedClassInitializer {
    pub declaration: crate::fir::DeclarationId,
    pub initialization_order: u32,
    pub class: ClassId,
    pub body: ExprId,
}

#[derive(Clone, Debug)]
pub struct IrCheckedEnumEntryBody {
    pub declaration: crate::fir::DeclarationId,
    pub class: ClassId,
    pub ordinal: u32,
    pub name: String,
    pub construction: ExprId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrCheckedConstructorTarget {
    Module(crate::fir::CallableId),
    External {
        declaration: crate::fir::ExternalCallableId,
        classifier: TypeName,
        parameters: Vec<Ty>,
    },
}

/// Exact dependency constructor selected by checked FIR, with an optional backend realization.
///
/// `declaration` is provider-neutral and survives common lowering. A target backend fills
/// `descriptor` from that identity before emission; common lowering never derives a physical ABI
/// from the call site's specialized semantic parameter types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrExternalConstructorTarget {
    pub declaration: crate::fir::ExternalCallableId,
    pub descriptor: Option<String>,
}

/// Representation selected for a value-class result crossing a coroutine suspension boundary.
/// A carrier that cannot preserve the value class's null semantics in an erased `Object` is wrapped;
/// a directly representable carrier crosses unchanged. This is produced by a target value-class pass
/// and consumed by its coroutine pass, after common lowering has finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrValueClassSuspendResult {
    Boxed { classifier: TypeName, carrier: Ty },
    Carrier(Ty),
}

impl IrValueClassSuspendResult {
    /// Type physically present in the continuation's erased result slot before the already-lowered
    /// call-site representation wrapper consumes it.
    pub fn boundary_ty(self) -> Ty {
        match self {
            Self::Boxed { classifier, .. } => Ty::obj_name(classifier),
            Self::Carrier(carrier) => carrier,
        }
    }
}

/// Semantic behavior of one non-call coroutine suspension point retained through common IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrIntrinsicSuspensionKind {
    /// Invoke the block with the current continuation directly.
    Unintercepted,
    /// Invoke the block with Kotlin's one-shot safe, intercepted continuation.
    Safe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrIntrinsicSuspensionPoint {
    pub result: Ty,
    pub kind: IrIntrinsicSuspensionKind,
}

impl IrExternalConstructorTarget {
    pub fn unresolved(declaration: crate::fir::ExternalCallableId) -> Self {
        Self {
            declaration,
            descriptor: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrPropertyDispatch {
    Ordinary,
    Super { owner: TypeName, interface: bool },
}

/// Operations whose semantic target and call shape were finalized in checked FIR. Common lowering
/// translates body-local operands only; target realization belongs to the backend/module emitter.
#[derive(Clone, Debug, PartialEq)]
pub enum IrCheckedOperation {
    Call {
        target: crate::fir::CallableId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: Vec<IrCheckedArgument>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    ConstructorDelegation {
        target: IrCheckedConstructorTarget,
        outer_parameter: Option<Ty>,
        outer_receiver: Option<ExprId>,
        arguments: Vec<IrCheckedArgument>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    PropertyRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        context_arguments: Vec<ExprId>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    PropertyWrite {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        context_arguments: Vec<ExprId>,
        value: ExprId,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    /// Read a dependency property selected by checked FIR. `target` is an opaque provider-owned
    /// declaration identity; common lowering deliberately does not interpret it as a method or a
    /// field. The target backend realizes that choice after common IR has been produced.
    ExternalPropertyRead {
        target: crate::fir::ExternalPropertyId,
        dispatch: IrPropertyDispatch,
        receiver: Option<ExprId>,
        arguments: Vec<ExprId>,
        parameters: Vec<Ty>,
        result: Ty,
        source_receiver: Option<Ty>,
    },
    /// Write a dependency property selected by checked FIR. As with `ExternalPropertyRead`, the
    /// physical storage/accessor choice belongs exclusively to the target backend.
    ExternalPropertyWrite {
        target: crate::fir::ExternalPropertyId,
        dispatch: IrPropertyDispatch,
        receiver: Option<ExprId>,
        arguments: Vec<ExprId>,
        parameters: Vec<Ty>,
        result: Ty,
        source_receiver: Option<Ty>,
    },
    LateinitFieldRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
    },
    BackingFieldRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
    },
    BackingFieldWrite {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        value: ExprId,
    },
    RangeConstruction {
        operation: crate::fir::FirRangeOperation,
        start: ExprId,
        start_type: Ty,
        end: ExprId,
        end_type: Ty,
        result: Ty,
    },
    RangeContains {
        operation: crate::fir::FirRangeOperation,
        value: ExprId,
        start: ExprId,
        end: ExprId,
        negated: bool,
        counter: Ty,
    },
    RangeLoop {
        variable: u32,
        counter: Ty,
        operation: crate::fir::FirRangeOperation,
        start: ExprId,
        end: ExprId,
        body: ExprId,
        label: String,
    },
    CallableReference {
        target: crate::fir::FirCallableReferenceTarget,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        function_type: Ty,
        substitutions: Vec<IrCheckedSubstitution>,
        adaptation: Option<Box<crate::fir::FirReferenceAdaptation>>,
    },
    PropertyReference {
        target: crate::fir::FirPropertyReferenceTarget,
        /// This reference is the compiler-generated `KProperty` metadata argument of a delegated
        /// property, not a source-written callable-reference value. Backends may realize the two
        /// checked semantic shapes differently; common lowering still retains only the selected
        /// property identity and never chooses a platform representation.
        delegated: bool,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        mutable: bool,
        substitutions: Vec<IrCheckedSubstitution>,
        adaptation: Option<Box<crate::fir::FirReferenceAdaptation>>,
    },
}

impl IrConst {
    pub fn zero_for_value_type(ty: Ty) -> IrConst {
        match ty.canonical_semantic() {
            Ty::Boolean => IrConst::Boolean(false),
            Ty::Byte | Ty::UByte => IrConst::Byte(0),
            Ty::Short | Ty::UShort => IrConst::Short(0),
            Ty::Int | Ty::UInt => IrConst::Int(0),
            Ty::Long | Ty::ULong => IrConst::Long(0),
            Ty::Float => IrConst::Float(0.0),
            Ty::Double => IrConst::Double(0.0),
            Ty::Char => IrConst::Char(0),
            _ => IrConst::Null,
        }
    }
}

/// Checked semantic shape of an annotation constructor call. The common IR retains the annotation
/// interface and lexical scope; a backend chooses the concrete runtime implementation and name.
#[derive(Clone, Debug)]
pub struct IrAnnotationConstruction {
    pub interface: TypeName,
    pub members: Vec<(String, Ty)>,
    /// Constructor defaults as lowered declarations, not evaluated call operands.
    pub defaults: Vec<Option<ExprId>>,
    /// Lexical classifier containing this call. `None` means a top-level/file-facade scope.
    pub enclosing_class: Option<TypeName>,
}

/// An IR expression node (a subset of Kotlin IR's `IrExpression` hierarchy). Operands reference
/// other expressions by `ExprId` into the arena.
#[derive(Clone, Debug)]
pub enum IrExpr {
    Const(IrConst),
    /// A frontend-selected semantic operation. Backend realization consumes the stable declaration
    /// identity; it must not repeat lookup, overload selection, or argument mapping.
    Checked(IrCheckedOperation),
    /// A class-literal constant — `ldc class <internal>` (a `java.lang.Class`). Used e.g. for the
    /// `PropertyReference0Impl(Class, …)` argument in delegated-property setup. `internal = None`
    /// is the current-facade sentinel for places lowered before the facade name is known.
    ClassConst {
        internal: Option<TypeName>,
    },
    /// Kotlin `KClass` literal. An unbound literal carries its resolved classifier; a bound literal
    /// carries the checked value whose runtime class is requested. The backend chooses the platform
    /// class-token and reflection representation without repeating frontend lookup.
    KClassLiteral {
        classifier: Option<Ty>,
        value: Option<ExprId>,
    },
    /// Backend-neutral reflection value passed to local delegated-property conventions.
    LocalPropertyReference {
        name: Box<str>,
        property_type: Ty,
    },
    /// Checked Kotlin singleton value. Its classifier is the semantic identity selected by the
    /// frontend; a backend decides how that singleton is stored on its target platform.
    SingletonValue {
        classifier: TypeName,
    },
    /// Read a value parameter / variable by its declaration index.
    GetValue(u32),
    /// Assign to a variable (`IrSetValue`).
    SetValue {
        var: u32,
        value: ExprId,
    },
    /// A call to a function/constructor/operator/stdlib intrinsic (`IrCall`). The `callee` is a
    /// resolved [`Callee`]: a local function, or an intrinsic identified by Kotlin FqName that each
    /// backend maps to its platform (`kotlin.plus`, `kotlin.io.println`, …). This single node
    /// expresses every call — there is no dedicated node per stdlib operation.
    Call {
        callee: Callee,
        dispatch_receiver: Option<ExprId>,
        args: Vec<ExprId>,
    },
    /// A placeholder a compiler-extension plugin must specialize before emit. Core lowering produces
    /// it generically, without plugin-specific ABI details, and the plugin rewrites this arena slot into
    /// concrete IR in its body phase. `exprs` are already-lowered operands, `data` carries resolved
    /// name ids; the meaning of both is private to the named plugin. A node that survives to emit is
    /// declined by `jvm_can_emit`.
    PluginPlaceholder {
        /// Which plugin specializes this node.
        plugin: &'static str,
        /// The plugin-specific operation.
        kind: &'static str,
        /// Already-lowered operand expressions, in a plugin-defined order.
        exprs: Vec<ExprId>,
        /// Resolved name ids the plugin needs.
        data: Vec<TypeName>,
    },
    /// `IrReturn` from the enclosing function.
    Return(Option<ExprId>),
    /// `IrBlock` — a sequence of statements; value is the last expression (or Unit).
    Block {
        stmts: Vec<ExprId>,
        value: Option<ExprId>,
    },
    /// `IrWhen` — branches of (condition → result); the AST `if`/`when` lower here. `else` is the
    /// branch with a `None` condition.
    When {
        branches: Vec<(Option<ExprId>, ExprId)>,
    },
    /// `IrTypeOperatorCall` — `is`/`!is`/`as`/`as?`/implicit casts/coercions.
    TypeOp {
        op: IrTypeOp,
        arg: ExprId,
        type_operand: Ty,
    },
    /// `IrWhile` loop. `update` (if present) runs after `body` each iteration, at the `continue`
    /// target — it carries a `for`-loop's increment so `continue` advances the loop rather than
    /// skipping it. A plain `while` has `update: None` (then `continue` re-tests `cond`). `post_test`
    /// ⇒ a `do…while` (the body runs once before `cond` is first tested).
    While {
        cond: ExprId,
        body: ExprId,
        update: Option<ExprId>,
        post_test: bool,
        label: Option<String>,
    },
    /// `break` — exit the innermost enclosing loop, or the loop carrying `label` (`break@outer`).
    Break {
        label: Option<String>,
    },
    /// `continue` — jump to the innermost enclosing loop's `update`/condition (or the labeled loop's).
    Continue {
        label: Option<String>,
    },
    /// A local variable declaration (`IrVariable`), value optional (`lateinit`).
    Variable {
        index: u32,
        ty: Ty,
        init: Option<ExprId>,
        /// `true` for a NAMED source variable (`val x = …`, a destructuring component, a loop
        /// variable); `false` for a compiler-introduced temp (elvis/safe-call materialization,
        /// suspension hoists). The suspend state machine spills every named reference variable in
        /// scope at a suspension point (kotlinc's rule — liveness-irrelevant), but a temp only by
        /// LIVENESS: kotlinc holds those values on the operand stack, which is empty across a
        /// suspension unless the value is still needed.
        named: bool,
    },
    /// A built-in primitive binary operator (`+`/`-`/`<`/`==`/…) on numeric/boolean operands. One
    /// parameterized node (not one-per-intrinsic): Kotlin IR models these as `IrCall` to the
    /// operator function, but the built-in numeric/boolean ops are universal across backends, so a
    /// single node lets each emit the native instruction (JVM `iadd`, JS `+`).
    PrimitiveBinOp {
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// Built-in numeric unary negation.
    PrimitiveNeg {
        operand: ExprId,
        ty: Ty,
    },
    /// A Kotlin string template `"a${x}b"` as an ordered list of parts (string constants + interpolated
    /// values, with empty constant chunks dropped). The JVM backend emits it as kotlinc does: a single
    /// part → `String.valueOf(part)`; multiple parts → one `StringBuilder` with a typed `append` per part
    /// and a final `toString()` (vs the old `String.plus` chain, which made one StringBuilder per `+`).
    StringConcat(Vec<ExprId>),
    /// Read a PROPERTY of `owner` — the Kotlin operation, with no accessor in it. A property is a
    /// declaration, not a method: `Dispatchers.IO` names the property `IO` on `kotlinx/coroutines/
    /// Dispatchers`, and there is no `getIO` anywhere in the language. HOW the read is realized — a field
    /// load, an instance accessor, a receiverless static accessor, a `@JvmName`-spelled or value-class-
    /// mangled one — is the target's business, derived by the backend from the owner's declaration. The
    /// node is the same whatever the owner is (this file, a sibling file, the classpath) and whatever the
    /// receiver is; there is no per-origin property node. `ty` is the property's LOGICAL Kotlin type; a
    /// realization whose physical result is erased/boxed is bridged to it by the backend. `receiver` is
    /// the value the property is read on, and stays an expression the program evaluates even when the
    /// realization takes no receiver.
    PropertyRead {
        /// Dispatch receiver for an instance property. Receiver-less classifier/top-level
        /// properties use `None`; that semantic shape does not prescribe a static field or method.
        receiver: Option<ExprId>,
        owner: TypeName,
        name: String,
        ty: Ty,
        /// Semantic owner shape selected by resolution. A JVM backend normally reads this from the
        /// compiled declaration, but a sibling source file has no classfile in the shared classpath yet.
        interface: bool,
        /// Stable identity assigned by [`IrFile::add_expr`]. Backend passes can move/clone an operation
        /// into a new expression slot; this identity follows the node so side-table realization facts do
        /// not accidentally remain attached to the obsolete arena index.
        operation: Option<u32>,
    },
    /// Write a property. `ty` is the source type that the assigned value is bridged to. Physical
    /// storage or accessor realization is selected only by the target backend.
    PropertyWrite {
        receiver: Option<ExprId>,
        owner: TypeName,
        name: String,
        value: ExprId,
        ty: Ty,
        interface: bool,
        operation: Option<u32>,
    },
    /// Follow one language-level enclosing-instance edge of an `inner` classifier. Checked FIR has
    /// already selected the exact classifier path; this node preserves one edge without choosing a
    /// storage layout. A JVM backend may realize it with `this$0`, while another target may use a
    /// closure/environment link or no physical field at all.
    EnclosingInstance {
        receiver: ExprId,
        inner: TypeName,
        outer: TypeName,
    },
    /// Read an instance field (`IrGetField`): `receiver.<fields[index]>` of class `class`.
    GetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
    },
    /// The RAW value of a `lateinit` backing field, WITHOUT the throw-if-null guard every ordinary
    /// read of one carries. Exists so `::prop.isInitialized` can test the field a normal read would
    /// reject; lowering compares it against null, which is what kotlinc emits (no reflection, no
    /// `KProperty` value).
    LateinitInitialized {
        receiver: ExprId,
        class: ClassId,
        index: u32,
    },
    /// Write an instance field (`IrSetField`): `receiver.<fields[index]> = value` (statement).
    SetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
        value: ExprId,
    },
    /// Read a top-level (module) property — `statics[index]`, a static field on the file facade.
    GetStatic(u32),
    /// Write a top-level (module) property — `statics[index] = value` (statement).
    SetStatic {
        index: u32,
        value: ExprId,
    },
    /// Construct an instance (`IrConstructorCall`) of `class` with constructor `args` (in field order).
    /// Construct a class: `new <internal>; dup; <args>; invokespecial <init>`. The owner is named by
    /// `internal` — resolve the in-IR class (`classes.get`/`class_info_name`) only where a consumer needs
    /// its `ClassId`; a name with no in-IR class is an external (classpath or other-module-file)
    /// construction. This is the SOLE construction node — there is no cp/module/local variant split.
    ///
    /// The constructor descriptor comes from exactly one source, checked in order:
    /// - `ctor_desc: Some(d)` — a verbatim JVM descriptor for a classpath construction whose signature
    ///   krusty does not model as `Ty`s (erased/library types). Wins; `ctor_params` is then ignored.
    /// - else the owner is an in-IR class — `ctor_params: None` selects its primary constructor (descriptor
    ///   derived from the class, after the value-class pass), `Some(types)` a secondary with that list.
    /// - else (an other-file/module class not in this IR) — `ctor_params: Some(types)` gives the parameter
    ///   types krusty builds the descriptor from.
    New {
        internal: TypeName,
        /// Supplied constructor operands in declaration order. Omitted defaulted parameters are
        /// absent; `defaults` identifies their source-value ordinals.
        args: Vec<ExprId>,
        ctor_params: Option<Vec<Ty>>,
        ctor_desc: Option<String>,
        /// Exact dependency constructor declaration awaiting target realization. Mutually exclusive
        /// with `ctor_desc`; source/module constructions leave it absent.
        external_target: Option<crate::fir::ExternalCallableId>,
        /// Omitted source-value parameter ordinals. A backend chooses its own default-call
        /// convention; common IR contains no placeholders, masks, or marker operands.
        defaults: Box<[u32]>,
        /// Leading semantic operands that are not source value parameters, such as an inner-class
        /// outer receiver or local-class captures. Default ordinals begin after this prefix.
        default_prefix_count: u32,
    },
    /// A virtual call to a class instance method `methods[index]` of `class` on `receiver`. `args[i] =
    /// None` means parameter `i` is omitted and takes its default (`p.copy(y=5)`, `f(a)` of `f(a, b=…)`);
    /// the meaning is backend-agnostic — the JVM realizes omitted args via the `$default` stub + mask,
    /// another backend may fill them inline. All-`Some` is an ordinary full call.
    MethodCall {
        class: ClassId,
        index: u32,
        receiver: ExprId,
        args: Vec<Option<ExprId>>,
    },
    /// Read a checked enum entry constant. Classifier plus declaration-owned entry name is the
    /// backend-neutral semantic identity; a target chooses its physical representation.
    EnumEntry {
        classifier: TypeName,
        name: Box<str>,
    },
    /// Read a static field holding a singleton instance (Kotlin IR's `IrGetObjectValue`):
    /// `getstatic <owner>.<field>:L<ty>;`. An `object`'s `INSTANCE` (`owner == ty`), or a
    /// `companion`'s `Companion` field on the outer class (`owner` = outer, `ty` = companion).
    StaticInstance {
        owner: ClassId,
        ty: ClassId,
        field: &'static str,
    },
    /// Read a static field of a CLASSPATH class by name — `getstatic owner.name:descriptor`. Used for a
    /// classpath `object` referenced as a value (`EmptyCoroutineContext` → `getstatic kotlin/coroutines/
    /// EmptyCoroutineContext.INSTANCE:Lkotlin/coroutines/EmptyCoroutineContext;`). Unlike `StaticInstance`
    /// (a user `ClassId`) and `GetStatic` (a facade statics index), this names an external owner directly.
    ExternalStaticField {
        owner: TypeName,
        name: String,
        descriptor: String,
    },
    /// Call a static method of a class (`Enum.values()`, `Enum.valueOf(s)`).
    EnumValues {
        classifier: TypeName,
    },
    EnumValueOf {
        classifier: TypeName,
        arg: ExprId,
    },
    EnumEntries {
        classifier: TypeName,
    },
    /// A reified-type-parameter CLASS placeholder inside an EMITTED `inline fun <reified T>` body:
    /// `Intrinsics.reifiedOperationMarker(4, name)` followed by the ERASED class constant — the
    /// pattern every splicer (kotlinc's and krusty's) patches with the call-site class at inline
    /// time. Value type: `java.lang.Class`.
    ReifiedClassMarker {
        name: String,
        erased: TypeName,
        /// Whether the checked expression produces Kotlin `KClass` rather than the raw platform
        /// class token. The JVM marker pair itself always materializes `java.lang.Class`; this flag
        /// retains the original expression's result representation for the backend wrapper.
        kclass: bool,
    },
    /// A reified `is`/`as` inside an EMITTED `inline fun <reified T>` body:
    /// `reifiedOperationMarker(3|1, name)` then `instanceof`/`checkcast` against the erasure —
    /// kotlinc's placeholder pair, patched at inline time. `negated` inverts the instanceof result.
    ReifiedTypeOp {
        cast: bool,
        negated: bool,
        arg: ExprId,
        name: String,
        erased: TypeName,
    },
    /// A lambda literal — emitted as `invokedynamic` + `LambdaMetafactory`. `impl_fn` is the
    /// synthesized static method holding the body; `captures` are the free-variable values bound into
    /// the call site (empty = non-capturing). `sam` is `None` for a plain Kotlin lambda (target
    /// `kotlin/jvm/functions/Function{arity}.invoke`) or contains the exact checked functional-
    /// interface declaration selected by the frontend. Platform descriptors are derived by the
    /// backend from that semantic declaration shape.
    /// `inline_body` is the lambda's *value-producing* body form (no synthetic `return`), emitted
    /// directly when the lambda is inlined into a stdlib `inline fun` splice — so a user `return` in the
    /// lambda becomes a real return from the *enclosing* method (correct non-local return). `None` for a
    /// callable reference (`::foo`), which has no inlinable body.
    Lambda {
        impl_fn: u32,
        arity: u8,
        captures: Vec<ExprId>,
        sam: Option<IrSamTarget>,
        inline_body: Option<ExprId>,
    },
    /// The `kotlin.Unit` singleton value (`IrGetObjectValue` of `Unit`). On the JVM, `getstatic
    /// kotlin/Unit.INSTANCE:Lkotlin/Unit;` — what a `Unit`-returning lambda body yields so its
    /// `FunctionN.invoke` returns an `Object`. Another backend realizes the unit value differently.
    UnitInstance,
    /// The enclosing suspend function's own `Continuation` — the receiver bound to the lambda parameter
    /// of `suspendCoroutineUninterceptedOrReturn { c -> … }`. A placeholder emitted by `ir_lower` that the
    /// CPS pass (`jvm/suspend.rs`) rewrites to the real continuation value (`GetValue(<cont slot>)`) once
    /// the trailing `Continuation` parameter exists. It must never survive to the emitter.
    CurrentContinuation,
    /// Invoke a function value (`f(args)` where `f: (A,…) -> R`) via the `FunctionN.invoke` interface
    /// method. Arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`.
    /// `params` retains the semantic Kotlin parameter types through backend carrier lowering so an
    /// adapter can distinguish equal carriers with different wrappers (`UInt` versus `Int`) without
    /// rediscovering the signature from the expression that produced `func`.
    InvokeFunction {
        func: ExprId,
        args: Vec<ExprId>,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// The not-null assertion `operand!!` — yields `operand`, throwing if it is null. On the JVM this
    /// is `kotlin/jvm/internal/Intrinsics.checkNotNull` applied to a duplicate of the value.
    ///
    /// `message` is set instead for the assertion a PLATFORM value (`T!`) gets when it is committed
    /// to a declared non-null type: the same yields-or-throws semantics, but with the checked
    /// expression's rendering (`getenv(...)`) carried into the failure, so the JVM form is
    /// `Intrinsics.checkNotNullExpressionValue(value, message)`. Every pass treats the two alike —
    /// only the emitted intrinsic and the `-X` option that removes it differ.
    NotNullAssert {
        operand: ExprId,
        message: Option<String>,
    },
    /// A `lateinit` read: yields `operand`, throwing `UninitializedPropertyAccessException(name)` if it
    /// is still null. Emitted as `<operand>; dup; ifnonnull L; ldc name;
    /// invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:` — the same guard the
    /// member-field lateinit read uses, here for a `lateinit var` LOCAL slot read.
    LateinitCheck {
        operand: ExprId,
        name: String,
    },
    /// Read the checker-selected static field holding a singleton:
    /// `getstatic <owner>.<field>:L<ty>;`. The owner and field type are semantic classifier identities;
    /// this works for both module and dependency singletons without reconstructing storage.
    ExternalStaticInstance {
        owner: TypeName,
        ty: TypeName,
        field: String,
    },
    /// A `kotlin/jvm/internal/Ref$XxxRef` holder boxing a mutable local that a closure captures: a
    /// new `Ref$IntRef`/`Ref$ObjectRef`/… whose `element` field is initialized to `init`. `elem` is
    /// the boxed value's type (selects the `Ref` subclass + the `element` field descriptor). Evaluates
    /// to the holder, so it's the initializer of the local that holds the box.
    RefNew {
        elem: Ty,
        init: ExprId,
    },
    /// Read a boxed mutable local: `holder.element` (`getfield Ref$XxxRef.element`).
    RefGet {
        holder: ExprId,
        elem: Ty,
    },
    /// Write a boxed mutable local: `holder.element = value` (`putfield`), evaluating to `value`.
    RefSet {
        holder: ExprId,
        elem: Ty,
        value: ExprId,
    },
    /// `throw operand` — throws the (Throwable) value; control never falls through (`Nothing`).
    Throw {
        operand: ExprId,
    },
    /// A `vararg` argument at a call site (Kotlin IR's `IrVararg`): the spread/listed elements and
    /// their element type. The JVM backend packs them into an array; another backend may differ.
    Vararg {
        /// The whole array type (`kotlin/IntArray`, `kotlin/Array<Int>`, `kotlin/Array<String>`), NOT the
        /// bare element — the JVM emitter derives the element + `newarray`/`anewarray` (and boxing of a
        /// `kotlin/Array<Int>` = `Integer[]`) from it via `ir_ty_to_jvm`. The element alone is ambiguous
        /// (`Obj("kotlin/Int")` is both a primitive `IntArray` element and a boxed `Array<Int>` element).
        array_type: Ty,
        /// Parallel to `elements`; a set entry contributes an array rather than one scalar element.
        spreads: Vec<bool>,
        elements: Vec<ExprId>,
    },
    /// Allocate an uninitialized array of `size` elements (`anewarray` for a reference element,
    /// `newarray` for a primitive) — the sized constructor `Array<T>(n) { … }` / `arrayOfNulls<T>(n)`
    /// fills it afterwards. (`Vararg` is the *literal* form with a statically-known element list.)
    NewArray {
        /// The whole array type — see [`IrExpr::Vararg::array_type`].
        array_type: Ty,
        size: ExprId,
    },
    /// `try { body } catch (e: E) { … } … [finally { f }]`. `result` is the value type (`Unit` when
    /// used as a statement). Each catch binds the exception to a value index and runs its body. A
    /// `finally` block runs on every exit (normal, each catch, and an uncaught exception via a
    /// catch-all that re-throws); it is emitted (inlined) at each.
    Try {
        body: ExprId,
        catches: Vec<IrCatch>,
        finally: Option<ExprId>,
        result: Ty,
    },
}

/// Checked functional-interface target attached to a lambda after SAM conversion.
///
/// Both the call-site-specialized shape and the declaration shape are retained: the former types
/// the implementation while the latter determines the platform method that the closure implements.
/// This is frontend semantic data; platform owner spellings and descriptors do not belong here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSamTarget {
    pub classifier: TypeName,
    pub method: String,
    pub parameters: Vec<Ty>,
    pub result: Ty,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// A fun-interface conversion of a callable reference delegates equality/hashCode through
    /// Kotlin's `FunctionAdapter` contract. Ordinary lambdas remain identity objects.
    pub function_adapter: bool,
}

/// One `catch (var: exc_internal) { body }` clause of an [`IrExpr::Try`].
#[derive(Clone, Debug)]
pub struct IrCatch {
    /// Value index the caught exception is bound to.
    pub var: u32,
    /// Source parameter name, absent for compiler-generated handlers.
    pub name: Option<String>,
    /// JVM internal name of the caught exception type.
    pub exc_internal: TypeName,
    pub body: ExprId,
}

/// Built-in binary operators carried by `IrExpr::PrimitiveBinOp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    /// Referential identity (`===`/`!==`): a JVM `if_acmp*` on two reference operands, never the
    /// structural `Intrinsics.areEqual` that `==`/`!=` (`Eq`/`Ne`) uses for references.
    RefEq,
    RefNe,
    And,
    Or,
    /// Bitwise/shift on `Int`/`Long` (Kotlin's `and`/`or`/`xor`/`shl`/`shr`/`ushr` infix functions).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
}

/// The `IrTypeOperatorCall` operators (Kotlin IR's `IrTypeOperator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrTypeOp {
    InstanceOf,    // `is T`
    NotInstanceOf, // `!is T`
    Cast,          // `as T?` (or `as <primitive>`): a plain `checkcast` — `null` passes
    /// `as T` to a non-null reference type: null-check (`Intrinsics.checkNotNull`) then `checkcast`,
    /// so casting `null` throws — matching kotlinc.
    CastNonNull,
    SafeCast, // `as? T`
    /// Representation coercion the backend inserts (e.g. JVM box/unbox) — explicit in the IR so it
    /// is visible and testable, not hidden in codegen.
    ImplicitCoercion,
}

/// A function/method declaration (`IrFunction`).
#[derive(Clone, Debug)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// The body expression (typically an `IrBlock`), or `None` for abstract/external.
    pub body: Option<ExprId>,
    pub is_static: bool,
    /// `Some(class internal)` for an instance method — `this` is value index 0, params follow.
    pub dispatch_receiver: Option<TypeName>,
    /// Per-parameter `Some(name)` when the backend should guard it with a non-null assertion at method
    /// entry (`Intrinsics.checkNotNullParameter` on the JVM) — non-null reference parameters of a
    /// visible (non-private) function. Empty for synthesized methods (no guards). Parallel to `params`.
    pub param_checks: Vec<Option<String>>,
}

/// One entry of an `enum class` in [`IrClass`]. Groups what were parallel `Vec`s keyed by entry index
/// (the `(name, args)` tuple plus the separate `subclass` vec), so an entry's name / constructor args /
/// synthesized-subclass marker can't desync.
#[derive(Clone, Debug)]
pub struct IrEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Checked source-order evaluation that must run before the physical constructor is entered.
    /// Argument mapping spills operands here so named/reordered arguments preserve Kotlin evaluation
    /// order without asking a backend to reconstruct the source call.
    pub argument_prelude: Vec<ExprId>,
    /// Lowered constructor-argument value ids (`RED(0xFF0000)`); empty for an arg-less entry. Filled in a
    /// later lowering pass — built empty when the entry list is first created.
    pub args: Vec<ExprId>,
    /// Selected constructor parameters whose values come from declaration defaults. This is the
    /// frontend's final argument-mapping decision; a backend only realizes its default-argument ABI.
    pub default_parameters: Vec<u32>,
    /// `Some(subclass_internal)` when the entry has a body and is constructed as an instance of a synthesized
    /// anonymous subclass (`new Enum$ENTRY(name, ordinal, args)`); `None` when constructed as the enum
    /// itself.
    /// 1-based source line of the entry's declaration, for the `<clinit>` `LineNumberTable`.
    pub decl_line: u32,
    pub subclass: Option<TypeName>,
}

/// One instance field of an [`IrClass`]. Groups what were parallel `Vec`s keyed by field index, so a
/// field's type / generic-param name / constant default / finality / visibility can't desync.
/// Bit-packed boolean flags for an [`IrField`], collapsing `has_default`/`is_final`/`is_private`/
/// `is_lateinit` into one byte. Read through the `IrField` accessors of the same names; built with the
/// `with_*` chain. Headroom for four more flags before the byte fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrfFlags(u8);

impl IrfFlags {
    const HAS_DEFAULT: u8 = 1 << 0;
    const IS_FINAL: u8 = 1 << 1;
    const IS_PRIVATE: u8 = 1 << 2;
    const IS_LATEINIT: u8 = 1 << 3;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_has_default(self, on: bool) -> Self {
        self.with(Self::HAS_DEFAULT, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_private(self, on: bool) -> Self {
        self.with(Self::IS_PRIVATE, on)
    }
    #[inline]
    pub const fn with_is_lateinit(self, on: bool) -> Self {
        self.with(Self::IS_LATEINIT, on)
    }
}

#[derive(Clone, Debug)]
pub struct IrField {
    pub name: String,
    pub ty: Ty,
    /// The source type-parameter NAME the field was declared with (`val x: T` → `Some("T")`), else
    /// `None`. Platform-neutral; lets the value-class pass pick the CORRECT bound for a generic
    /// underlying (vs guessing), independent of erasure dropping the name.
    pub type_param: Option<String>,
    /// The CONSTANT default from a primary-constructor default (`val b: Int = 5` → `Some(Int(5))`,
    /// `val t: T? = null` → `Some(Null)`), else `None` (no default, or a non-constant one). Later
    /// compiler passes may use it; the core backend ignores it.
    pub default: Option<IrConst>,
    /// Bit-packed `has_default`/`is_final`/`is_private`/`is_lateinit` (read via the accessors below).
    /// `has_default` — the primary-constructor parameter declared ANY default (constant or not, e.g.
    /// `routes: List<String> = emptyList()`); distinct from `default` (constant-only), needed so the
    /// `@Metadata` emitter sets the `DECLARES_DEFAULT_VALUE` value-parameter flag as kotlinc does.
    /// `is_final` — the backing field is immutable (`val`), emitted `final`. `is_private` — private
    /// backing field (the Kotlin default, reached via accessors); `false` for a field read/written
    /// cross-class (a coroutine continuation's `result`/`label`). `is_lateinit` — backs a `lateinit
    /// var`; every backend read null-checks it and throws when still unset, matching kotlinc.
    pub flags: IrfFlags,
}

impl IrField {
    /// A plain backing field with Kotlin defaults: mutable-unknown (`is_final = false`), `private`, no
    /// generic-param name, no constant default. Synthesized classes build fields from this.
    pub fn new(name: String, ty: Ty) -> IrField {
        IrField {
            name,
            ty,
            type_param: None,
            default: None,
            flags: IrfFlags::default().with_is_private(true),
        }
    }

    #[inline]
    pub fn has_default(&self) -> bool {
        self.flags.has(IrfFlags::HAS_DEFAULT)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(IrfFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_private(&self) -> bool {
        self.flags.has(IrfFlags::IS_PRIVATE)
    }
    #[inline]
    pub fn is_lateinit(&self) -> bool {
        self.flags.has(IrfFlags::IS_LATEINIT)
    }

    /// Chainable override of `is_final` on top of [`IrField::new`] (replaces a `..IrField::new` spread).
    #[inline]
    pub fn with_is_final(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_final(on);
        self
    }
    /// Chainable override of `is_private` on top of [`IrField::new`].
    #[inline]
    pub fn with_is_private(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_private(on);
        self
    }
}

/// One primary-constructor parameter of an [`IrClass`], in declaration order. Folds what were the
/// index-parallel `ctor_args` tuple and `ctor_param_checks` vec, so a parameter's type / `is_field`
/// flag / null-check name can't desync.
#[derive(Clone, Debug)]
pub struct IrCtorArg {
    /// Source parameter name. Synthetic constructor parameters have no name.
    pub name: Option<String>,
    /// The parameter type (carries declared nullability — a nullable value-class param erases like its
    /// field).
    pub ty: Ty,
    /// Declared source-level semantic type before storage/JVM erasure. `None` for synthetic
    /// parameters. Metadata consumes this shape so nested type-parameter uses such as `KClass<T>`
    /// do not collapse to `KClass<Any>` merely because the constructor stores an erased value.
    pub declared_ty: Option<Ty>,
    /// `true` ⇒ a `val`/`var` property whose arg is stored to a field (the property fields are
    /// represented by `field_index`; `false` ⇒ a plain parameter, an argument only, available as a
    /// local in `<init>` for property initializers / `init` blocks.
    pub is_field: bool,
    /// Exact backing-field index for a property/capture constructor parameter. This cannot be derived
    /// from parameter or field order: interface-delegation storage may precede a source property, and
    /// lexical captures form a distinct constructor prefix. `None` for a plain parameter and for older
    /// synthesized layouts that deliberately use their complete leading field list.
    pub field_index: Option<u32>,
    pub has_default: bool,
    /// A `vararg` primary-ctor parameter — class `@Metadata` emits `ValueParameter.vararg_element_type`
    /// (f4) so a consumer admits element-form/omitted arguments instead of demanding a literal array.
    pub is_vararg: bool,
    pub type_param: Option<u32>,
    /// `Some(name)` when the backend should guard this parameter with a non-null assertion
    /// (`Intrinsics.checkNotNullParameter`) at `<init>` entry — a non-null reference param. `None` for a
    /// primitive, nullable, or class-type-parameter param, and for the synthetic inner `this$0`.
    pub check: Option<String>,
}

/// A property a class DECLARES. A property is a declaration, not a pair of methods: `val a: Int` is one
/// thing, and the `getA()` a target may emit for it is a realization of it. The front end lowers only
/// what is genuinely Kotlin — a source-written accessor's BODY — and leaves naming, descriptors and
/// dispatch to the backend.
/// One member EXTENSION property declaration ([`IrFile::member_ext_props`]): the SEMANTIC types
/// (checker-resolved, pre-erasure) plus the accessor realization, everything a class `Property`
/// metadata record needs.
#[derive(Clone, Debug)]
pub struct MemberExtProp {
    pub name: String,
    /// Declared extension receiver (`Int` in `val Int.doubled`).
    pub receiver: crate::types::Ty,
    /// Declared property type.
    pub ty: crate::types::Ty,
    pub is_var: bool,
    /// Whether this declaration has no accessor implementation and must be realized abstractly.
    pub is_abstract: bool,
    /// Getter function id. Abstract properties point at a bodyless common-IR function.
    pub getter: u32,
    /// Setter function id, for a `var`.
    pub setter: Option<u32>,
    pub visibility: crate::types::Visibility,
    /// Declaration-owned generic parameters. Source names are metadata payload; semantic names are
    /// the stable identities used by `receiver` and `ty`.
    pub type_params: Vec<IrTypeParameter>,
}

#[derive(Clone, Debug)]
pub struct IrProperty {
    pub name: String,
    /// Named context parameters in source order. Metadata records these separately from ordinary
    /// value parameters, and checked call sites supply their operands implicitly.
    pub context_params: Vec<(String, Ty)>,
    /// Source byte offset and 1-based declaration line. These remain attached to the declaration so
    /// a backend can order/debug synthesized accessors without rebinding the property by spelling.
    pub source_order: u32,
    pub decl_line: u32,
    /// The property's language-level type. This is also the type exposed by its accessors.
    pub ty: Ty,
    /// Kotlin declaration visibility. Backends consume this semantic fact when choosing the
    /// visibility of an accessor or a target-specific storage realization; they must not recover it
    /// from a rendered owner/property-name key.
    pub visibility: crate::types::Visibility,
    /// Resolved Kotlin annotation identities. Backends interpret annotations in their own namespace;
    /// common lowering does not turn them into storage or calling-convention choices.
    pub annotations: Box<[TypeName]>,
    /// The declaration initializer after common lowering, before any backend chooses storage.
    /// `None` means the declaration has no initializer (or its source shape is not represented),
    /// which is distinct from an explicit nullable initializer lowered to `IrConst::Null`.
    /// Keeping this on the declaration lets a backend relocate storage without re-reading the AST
    /// or mistaking a later assignment in an `init` block for the declaration initializer.
    pub initializer: Option<ExprId>,
    /// The declared type of an explicit backing field when it differs from the property's public type.
    /// The JVM value-class pass may erase the physical [`IrField`] to its carrier, so retaining this
    /// semantic storage boundary lets the backend box/unbox at the accessor without resolving anything.
    pub storage_ty: Option<Ty>,
    /// Index into [`IrClass::fields`] for the backing field, `None` for a computed/delegated property
    /// (which stores nothing).
    pub backing_field: Option<u32>,
    pub is_var: bool,
    /// Non-final: the accessor a backend emits for it must be overridable.
    pub is_open: bool,
    /// A `private` property. kotlinc emits NO accessor for one — in-class reads go straight to the
    /// backing field — so a use from outside the declaring class has nothing to call, and whichever
    /// path is lowering it does not own the access.
    pub is_private: bool,
    /// `true` for a `var` whose setter alone is declared `private`. This is declaration visibility, not
    /// a JVM flag: every backend must preserve it when realizing a default setter.
    pub setter_is_private: bool,
    /// The lowered body of a source-written getter/setter (a computed, `field`-using, or delegated
    /// property). `None` for a plain backing-field property, whose accessor has no source body at all.
    pub getter: Option<FunId>,
    pub setter: Option<FunId>,
    /// The JVM name a backend must use for the synthesized accessor when the plain spelling is wrong —
    /// a value-class-typed property's accessor is `@JvmName`-mangled. Stamped by the pass that knows the
    /// value classes; `None` means the ordinary spelling applies.
    pub getter_jvm_name: Option<String>,
    pub setter_jvm_name: Option<String>,
    /// Some use of this PRIVATE property reaches it from outside the declaring class — an `inline`
    /// function's body, spliced into its caller. The declaring class must then expose a synthetic
    /// accessor for it (`access$get<X>$p` on the JVM); without one the splice would be illegal, and
    /// silently degrading the `inline` call instead would change what the program does.
    pub needs_access_bridge: bool,
}

/// A class/interface/object declaration (`IrClass`). Instance fields come from the primary
/// constructor's `val`/`var` parameters (in order); the constructor stores each.
#[derive(Clone, Debug)]
pub struct IrClass {
    pub fq_name: TypeName,
    /// `true` when this classifier comes from a source declaration. Synthesized implementation
    /// classes must not be published as declared nested classifiers in language metadata, even when
    /// their backend name happens to look nested.
    pub is_source_declared: bool,
    /// A source anonymous-object declaration. Its lexical function is recorded separately as an
    /// exact [`FunId`], so a backend can realize enclosure metadata without parsing generated names.
    pub is_anonymous_object: bool,
    pub enclosing_function: Option<FunId>,
    /// A language-level non-static nested class. Backends consume this declaration property directly;
    /// a synthetic receiver field or its physical name does not imply inner-class semantics.
    pub is_inner_class: bool,
    /// A classifier declared in STATEMENT position. Its name is qualified by the declaration it was
    /// written in, so it contains a `$` that names no class — a local class is not a member of
    /// anything, and the JVM says so with `outer_class_info_index = 0` in its `InnerClasses` entry.
    pub is_local_class: bool,
    /// `@JvmInline value class` — a single-field class represented unboxed (as its one field's type) by
    /// the JVM `jvm::value_classes` IR pass. The IR otherwise treats it as a plain class.
    pub is_value: bool,
    /// `data class` — carried so the metadata emitter can reproduce kotlinc's `IS_DATA` class flag and
    /// its synthesized `componentN`/`copy`/`equals`/`hashCode`/`toString` function metadata.
    pub is_data: bool,
    /// 1-based source line of the class declaration (0 = unknown). The emitter maps the
    /// `LineNumberTable` of synthesized members (ctor/accessors) to this line, as kotlinc does.
    pub decl_line: u32,
    /// Declared non-`Any` generic upper bounds (`<T: String>` → `("T", String)`), carried verbatim from
    /// the source. Platform-neutral metadata; the JVM value-class pass uses it to erase a value class's
    /// underlying type parameter to its bound (`value class S<T: String>` → `String`).
    pub type_param_bounds: Vec<(String, Ty)>,
    /// ALL declared generic type-parameter names in order (`class C<A, B>` → `["A","B"]`), including
    /// those with only the implicit `Any` bound (unlike [`type_param_bounds`], which lists only non-`Any`
    /// bounds). Empty for a non-generic class.
    pub type_params: Vec<String>,
    /// Semantic identities captured from enclosing generic declarations. These are available to
    /// member metadata but are not declarations of this class. Kotlin metadata assigns them IDs
    /// before this class's own parameters (an inner `U` is id 1 when outer `T` is id 0).
    pub captured_type_params: Vec<String>,
    pub supertypes: Vec<Ty>,
    /// Instance fields. The first `ctor_param_count` are the primary-constructor parameters (stored
    /// directly from args, in order); any after them are class-body properties initialized by `init_body`.
    /// The properties this class declares — the DECLARATION, alongside the backing `fields` that store
    /// them and (for now) the accessor methods the front end still synthesizes into `methods`.
    pub properties: Vec<IrProperty>,
    pub fields: Vec<IrField>,
    /// How many leading `fields` are property constructor parameters (`val`/`var`) — the rest are body
    /// properties. NOTE: this is the count of constructor params that BACK A FIELD, not the total
    /// constructor arity (a non-`val`/`var` parameter is an argument only, no field) — see `ctor_args`.
    pub ctor_param_count: u32,
    /// Compiler-supplied parameters leading every constructor body: enclosing instances and lexical
    /// captures. They are a language-level closure layout, not source value parameters, so they stay
    /// out of constructor metadata and default-mask ordinals. The first `constructor_prefix_count`
    /// entries of `ctor_args` describe their common-IR types and storage.
    pub constructor_prefix_count: u32,
    /// ALL primary-constructor parameters in declaration order (each an [`IrCtorArg`] with type,
    /// `is_field`, and optional null-check name). Empty for synthesized/enum/object classes (then the
    /// constructor arity is `ctor_param_count`).
    pub ctor_args: Vec<IrCtorArg>,
    /// User annotations on each primary-constructor parameter, parallel to `ctor_args`. Empty when no
    /// parameter carries one (every synthesized class). Kept off [`IrCtorArg`] so the many synthesized
    /// constructors that build one stay unchanged. An annotation reaches this list only when Kotlin's
    /// use-site defaulting puts it on the PARAMETER rather than the property or the backing field.
    pub ctor_param_annotations: Vec<DeclarationAnnotations>,
    /// Constructor body run after `super(…)`: an effect `Block` lowered with `this` = value 0 and the
    /// constructor parameters as values `1..=N`. When [`explicit_param_stores`] is set it BEGINS with the
    /// `val`/`var` param→field stores (the desugared primary-constructor sugar); it also carries body-
    /// property initializers (`SetField`) and `init { … }` blocks. `None` when there's nothing to run.
    pub init_body: Option<ExprId>,
    /// Explicit `(constructor parameter index, field index)` stores that must run before the superclass
    /// constructor. This is semantic constructor-order metadata: the JVM backend must not infer it from
    /// a synthetic field spelling or assume the target is a leading property field. Language-level inner
    /// classes and generated state machines can both require such a store for different reasons; ordinary
    /// lexical/enclosing captures remain post-`super` stores.
    pub pre_super_param_fields: Vec<(u32, u32)>,
    /// `true` when `init_body` already stores the primary-constructor `val`/`var` params (and inner
    /// `this$0`) to their fields — the desugared form. The JVM backend then must NOT auto-store them (it
    /// would double-store). `false` for synthesized classes that still rely on the backend's implicit
    /// param→field store.
    pub explicit_param_stores: bool,
    /// Instance methods — `FunId`s into `IrFile.functions` (each with `dispatch_receiver = Some`).
    pub methods: Vec<FunId>,
    pub is_interface: bool,
    /// `true` for a source `fun interface`. This is a language-level classifier fact carried through
    /// IR so emitted Kotlin metadata preserves SAM eligibility for dependent modules.
    pub is_fun_interface: bool,
    /// `true` for a Kotlin `annotation class`. Emitted as a JVM annotation INTERFACE (`ACC_ANNOTATION|
    /// ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/annotation/Annotation`, one abstract accessor per
    /// member named after the property — from `fields`). NOT a plain class.
    pub is_annotation: bool,
    /// `Some(annotation_interface_internal)` when this class is the synthetic IMPLEMENTATION of an
    /// annotation (kotlinc's `…$annotationImpl$A$0`): it implements the annotation interface and the JVM
    /// `java.lang.annotation.Annotation` contract (per-member accessors + content `equals`/`hashCode`/
    /// `toString`/`annotationType`), so `A(args)` can construct an annotation instance. `fields` are the
    /// members in order. The backend emits the whole contract from `fields`.
    pub annotation_impl_of: Option<TypeName>,
    /// `true` for a `sealed class`/`sealed interface`.
    pub is_sealed: bool,
    /// Direct subclasses known from the whole source module.
    pub sealed_subclasses: TypeNameList,
    /// `true` for an `abstract class` (not `sealed`).
    pub is_abstract: bool,
    /// `true` for a source `open`/`sealed` class. Needed by backends because a subclass may be emitted
    /// from a different `IrFile`, so same-file subclass scans are not enough to decide JVM finality.
    pub is_open: bool,
    /// Semantic superclass internal name (`kotlin/Any` normally, or a user base class for
    /// `class B : A(args)`). Target-specific representation classes such as JVM enum bases are chosen by
    /// the backend.
    pub superclass: TypeName,
    /// Arguments to the base-class constructor (`: A(args)`) — lowered IR value ids, evaluated with
    /// `this`=value 0 and the primary-constructor params as values `1..=ctor_param_count`. Empty
    /// unless `superclass` is a user base class.
    pub super_arg_prelude: Vec<ExprId>,
    pub super_args: Vec<ExprId>,
    /// Checker-selected semantic parameter types parallel to `super_args`. A backend couples these to
    /// its physical superclass-constructor ABI without resolving the constructor again.
    pub super_ctor_params: Vec<Ty>,
    /// Enum entries in declaration order. Non-empty only for an `enum class`; the backend emits a static
    /// field per entry, a `$VALUES` array, a `<clinit>` that constructs them, and `values()`/
    /// `valueOf(String)`. Each [`IrEnumEntry`] carries its name, lowered constructor args, and optional
    /// synthesized-subclass fq name.
    pub enum_entries: Vec<IrEnumEntry>,
    /// `Some(user_field_types)` marks this class as a synthesized enum-entry subclass: it extends the
    /// enum (`superclass`), has no own fields, and its constructor is `(String name, int ordinal,
    /// <user_field_types>)V` delegating to the enum's `(String,int,<user>)V` constructor.
    pub enum_entry_of: Option<Vec<Ty>>,
    /// `Some(..)` marks this class as a synthesized property-reference singleton: a `final class
    /// extends kotlin/jvm/internal/PropertyReference1Impl` (the `superclass`) with a `public static
    /// final INSTANCE`, a constructor `super(owner.class, name, signature, 0)`, and a `get(Object)
    /// Object` override that reads the referenced property via its getter.
    pub prop_ref: Option<PropRef>,
    /// When `Some`, this class is a synthesized function-reference subclass (`<Owner>$ref$N extends
    /// kotlin/jvm/internal/FunctionReferenceImpl implements Function<arity>`), emitted by
    /// `emit_func_ref_class`. Gives callable references real Kotlin reference EQUALITY (the base class
    /// compares owner/name/signature/boundReceiver) — `::f == ::f`, `a::m != b::m`.
    pub func_ref: Option<FuncRef>,
    /// JVM declaration adapters. Most are synthetic bridges for generic/covariant overrides; a boxed
    /// value class also needs ordinary instance entries for interface methods whose implementation is
    /// realized as a static carrier function.
    pub bridges: Vec<Bridge>,
    /// Implemented interface internal names (`class C : I, J`). The class file lists them as
    /// `implements`; an interface declaration lists its super-interfaces here.
    pub interfaces: TypeNameList,
    /// `object Foo` — a singleton: a `public static final Foo INSTANCE` field, a private no-arg
    /// constructor, and a `<clinit>` that constructs the instance.
    pub is_object: bool,
    /// `true` for a synthesized `C$Companion` class: a private no-arg constructor and no own singleton
    /// field (the `Companion` instance is held by the outer class).
    pub is_companion: bool,
    /// `Some(companion_fq)` on a class with a `companion object`: emit a `public static final
    /// <companion> Companion` field, initialized in this class's `<clinit>`.
    pub companion_class: Option<TypeName>,
    /// Secondary constructors — each an extra `<init>(params)` that delegates to the primary
    /// constructor (`constructor(…) : this(args)`) then runs its body. Empty for most classes.
    pub secondary_ctors: Vec<IrSecondaryCtor>,
    /// `false` for a class with NO primary constructor: the backend emits no primary `<init>`; every
    /// `<init>` comes from `secondary_ctors` (a `Super`-delegating one carries the init body). `true`
    /// for every other class (including synthesized/enum/object).
    pub has_primary_ctor: bool,
    /// Resolved annotations applied to this class (`@Anno(...) class TTT`) with their semantic
    /// retention. A backend decides how each retained annotation is represented; SOURCE-retained
    /// annotations are absent. Empty for a class with none.
    pub applied_annotations: DeclarationAnnotations,
    /// User annotations applied to this class's fields (property backing fields and enum-constant
    /// fields), by field name — emitted into each field's `Runtime[In]VisibleAnnotations`. Empty for a
    /// class whose fields carry none.
    pub field_annotations: Vec<FieldAnnotations>,
    /// User annotations that landed on the PROPERTY itself (`@Anno val v`, where the annotation's
    /// `@Target` admits `PROPERTY`), by property name. Kotlin properties have no class-file
    /// declaration, so these are emitted onto a synthetic `get<Name>$annotations()` marker method
    /// that the property's `JvmPropertySignature` names. Empty for a class whose properties carry
    /// none.
    pub property_annotations: Vec<PropertyAnnotations>,
    /// User annotations declared on the PRIMARY constructor (`class C @Mark constructor(…)`) — the
    /// primary-`<init>` analogue of [`IrSecondaryCtor::annotations`], carrying retention per
    /// application like every other declaration's. Empty for a class with no primary constructor, or
    /// one that carries none.
    pub primary_ctor_annotations: DeclarationAnnotations,
    /// For an `annotation class`: its declared Kotlin retention. `None` for every other class. Drives the
    /// meta-annotations the emitter stamps on the annotation interface — kotlinc writes
    /// `@kotlin.annotation.Retention(<declared>)` for an EXPLICIT `@Retention(…)` plus
    /// `@java.lang.annotation.Retention(RUNTIME|CLASS|SOURCE)` always (RUNTIME when defaulted) — so
    /// consumers can read the retention back from the compiled class.
    pub annotation_retention: Option<AnnoRetention>,
}

/// Backend-agnostic retention fact resolved by the frontend.
pub type AnnoRetention = crate::types::AnnotationRetention;

/// A resolved JVM annotation value (`element_value`, JVMS §4.7.16.1) — an annotation argument folded to
/// the constant the class file encodes.
#[derive(Clone, Debug)]
pub enum AnnoValue {
    /// A primitive/`String` constant (encoded by tag `B`/`C`/`D`/`F`/`I`/`J`/`S`/`Z`/`s`).
    Const(IrConst),
    /// An enum constant `(enum_type_internal, const_name)` — tag `e`.
    Enum(TypeName, String),
    /// A class literal `T::class` `(type_internal)` — tag `c` (its type descriptor).
    Class(TypeName),
    /// A nested annotation instance `A(...)` — tag `@`.
    Annotation(AppliedAnnotation),
    /// An array `[…]` — tag `[`.
    Array(Vec<AnnoValue>),
}

/// User annotations on one field. Retention remains a semantic fact on each annotation; the backend
/// chooses its physical representation.
#[derive(Clone, Debug)]
pub struct FieldAnnotations {
    pub field: String,
    pub annotations: DeclarationAnnotations,
}

/// One retained annotation application on a declaration. The application payload stays independent
/// of retention so nested annotation values can reuse [`AppliedAnnotation`] without inventing a
/// declaration-retention fact for the nested value.
#[derive(Clone, Debug)]
pub struct RetainedAnnotation {
    pub retention: AnnoRetention,
    pub annotation: AppliedAnnotation,
}

/// Backend-agnostic annotations on any declaration kind. A HIDDEN-deprecated declaration is
/// identified from these records rather than a separate flag: the annotation IS the fact.
/// User annotations that landed on one PROPERTY. Retention remains semantic until a backend maps
/// the declaration onto its physical representation (a JVM marker method, for example).
#[derive(Clone, Debug)]
pub struct PropertyAnnotations {
    pub property: String,
    pub annotations: DeclarationAnnotations,
}

#[derive(Clone, Debug, Default)]
pub struct DeclarationAnnotations(Vec<RetainedAnnotation>);

impl DeclarationAnnotations {
    pub fn new(annotations: Vec<RetainedAnnotation>) -> Self {
        Self(annotations)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, RetainedAnnotation> {
        self.0.iter()
    }

    /// Applied annotation payloads in declaration order, independent of the physical retention
    /// partition a backend may later require.
    pub fn applications(&self) -> impl Iterator<Item = &AppliedAnnotation> {
        self.0.iter().map(|retained| &retained.annotation)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether these annotations include `@kotlin.Deprecated` at any level. kotlinc additionally
    /// stamps the classic `Deprecated` class-file attribute on such a declaration, beside the
    /// annotation itself.
    pub fn deprecated(&self) -> bool {
        self.iter()
            .any(|retained| retained.annotation.internal.matches("kotlin/Deprecated"))
    }

    /// Whether these annotations include `@kotlin.Deprecated(level = DeprecationLevel.HIDDEN)`.
    /// kotlinc removes such a declaration from resolution and emits its realization
    /// `ACC_SYNTHETIC`; both facts follow from this one annotation.
    pub fn deprecated_hidden(&self) -> bool {
        self.iter().any(|retained| {
            let annotation = &retained.annotation;
            annotation.internal.matches("kotlin/Deprecated")
                && annotation.values.iter().any(|(name, value)| {
                    name == "level"
                        && matches!(value, AnnoValue::Enum(ty, constant)
                            if ty.matches("kotlin/DeprecationLevel") && constant == "HIDDEN")
                })
        })
    }
}

/// An applied annotation (`@Anno(...)`) to encode into a `RuntimeVisibleAnnotations` attribute.
#[derive(Clone, Debug)]
pub struct AppliedAnnotation {
    /// The annotation type's internal name (`Anno`).
    pub internal: TypeName,
    /// `element_value_pairs`: `(element_name, value)` in declaration order.
    pub values: Vec<(String, AnnoValue)>,
}

/// How a function-reference subclass's `invoke` dispatches to its target.
#[derive(Clone, Debug)]
pub enum FrDispatch {
    /// Top-level / static target: `invokestatic call_owner.call_name(call_desc)`. All invoke params are
    /// the call arguments.
    Static,
    /// Unbound member `Type::m`: the FIRST invoke param is the receiver; `invokevirtual` on it.
    VirtualUnbound,
    /// Bound member `obj::m`: the receiver is captured (`this.receiver`); `invokevirtual` on it. All
    /// invoke params are the call arguments.
    VirtualBound,
    /// Bound extension `obj::ext`: the receiver is captured (`this.receiver`) and passed as the FIRST
    /// argument of `invokestatic call_owner.call_name(receiver, args…)`. `target_param_tys` leads with
    /// the receiver type; `param_tys` (the invoke args) map to `target_param_tys[1..]`.
    StaticBound,
    /// Suspend conversion: a NON-suspend function VALUE captured as the receiver, adapted to a
    /// `suspend` function type. `invoke((args…), continuation)` delegates
    /// `invokeinterface call_owner(=Function{n}).invoke(args…)` with the trailing continuation
    /// DROPPED (a plain function never suspends; its erased result is returned as the completion
    /// value). The class also implements the `kotlin/coroutines/jvm/internal/SuspendFunction` marker.
    SuspendConvert,
}

/// A synthesized function-reference subclass of `kotlin/jvm/internal/FunctionReferenceImpl`. See
/// `emit_func_ref_class`. `param_tys`/`ret_ty` are the LOGICAL `invoke` signature (for `VirtualUnbound`,
/// `param_tys[0]` is the receiver); the SAM interface erases them to `Object`, so `invoke` casts.
#[derive(Clone, Debug)]
pub struct FuncRef {
    /// Adapted callable references use Kotlin's `AdaptedFunctionReference` carrier so equality and
    /// hashing include the checked adaptation arity/flags instead of lambda identity.
    pub adapted: bool,
    pub bound: bool,
    /// Kotlin source-level function arity. Backends add any representation parameters, such as a
    /// suspend continuation, when selecting their callable carrier.
    pub arity: u8,
    /// The referenced declaration is suspend. The generated reference carries Kotlin's semantic
    /// suspend-function identity; each backend owns its physical calling convention.
    pub is_suspend: bool,
    /// Exact current-module declaration selected by the frontend when this carrier invokes a
    /// source callable directly. A backend uses the stable identity only to realize physical
    /// naming/layout; reflection continues to expose `fn_name`, the Kotlin declaration name.
    pub module_target: Option<crate::fir::CallableId>,
    /// Exact common-IR helper invoked by this carrier when callable-reference adaptation generated
    /// a local wrapper. This is a stable IR identity, so backend liveness and access-bridge planning
    /// never rediscover the helper from its synthesized name and arity.
    pub local_target: Option<FunId>,
    /// Class passed to `super(...)` (the reference's declaring class); `None` = the file facade.
    pub owner_class: Option<TypeName>,
    pub fn_name: String,
    pub flags: i32,
    pub dispatch: FrDispatch,
    /// Class the target method is invoked on; `None` = the file facade.
    pub call_owner: Option<TypeName>,
    pub call_name: String,
    pub reflection_name: Option<String>,
    /// The physical static bridge takes the original dispatch receiver as parameter zero, while
    /// reflection still describes the referenced instance declaration without that receiver.
    pub reflection_receiver_parameter: bool,
    /// Reflection declaration return when the invoked helper has a different ABI. Constructor
    /// adapters return the constructed value, while their reflected declaration returns JVM void.
    pub reflection_target_ret_ty: Option<Ty>,
    /// Declaration parameters used only for callable-reference identity. An adapted reference
    /// invokes a generated wrapper whose ABI is in `target_param_tys`, while equality/reflection
    /// must retain the original declaration descriptor.
    pub reflection_target_param_tys: Option<Vec<Ty>>,
    /// The target method is declared on an INTERFACE (`invokeinterface`, not `invokevirtual`).
    pub call_interface: bool,
    /// The LOGICAL `invoke` parameter types. For `VirtualUnbound`, `param_tys[0]` is the receiver
    /// (excluded from the method descriptor / signature). The emitter derives the JVM signature and
    /// reference metadata signature from these + `ret_ty`.
    pub param_tys: Vec<Ty>,
    pub ret_ty: Ty,
    /// The PHYSICAL target-call parameter/return types after backend lowerings such as JVM value-class
    /// erasure. Same shape as `param_tys` (including the unbound receiver slot when present).
    pub target_param_tys: Vec<Ty>,
    pub target_ret_ty: Ty,
    /// Per logical invoke parameter: `Some(value_class_internal)` means the erased Object argument is a
    /// boxed value-class instance and must be unboxed before the physical target call.
    pub unbox_params: Vec<Option<TypeName>>,
    /// Parallel to `unbox_params`: nullable value-class parameters unbox `null` to a null underlying.
    pub unbox_param_nullable: Vec<bool>,
    /// `Some(value_class_internal)` means the physical target returns the value-class underlying and the
    /// function-reference `invoke` must box it back before returning Object.
    pub box_ret: Option<TypeName>,
    /// `StaticBound` only: `Some(value_class_internal)` when the CAPTURED receiver is a value class
    /// (`Z(42)::ext`). The receiver is stored boxed as `Object`; the emitter `checkcast`s it to the box
    /// class then `unbox-impl`s it to the underlying before the mangled `invokestatic ext-<hash>(under)`.
    pub staticbound_recv_unbox: Option<TypeName>,
}

/// A synthesized property-reference class's metadata (`Type::prop` → `Type$prop$N`): the referenced
/// property's owner, name, getter, and value type. The backend emits the `PropertyReference1Impl`
/// subclass from this.
#[derive(Clone, Debug)]
pub struct PropRef {
    /// Referenced property's owner class; `None` = the file facade.
    pub owner_internal: Option<TypeName>,
    /// Physical owner of a member accessor. This differs from `owner_internal` for an inherited
    /// property reference (`Derived::p` reflects on `Derived` but may invoke `Base.getP`).
    pub call_owner_internal: Option<TypeName>,
    pub prop_name: String,
    pub getter_name: String,
    pub getter_descriptor: Option<String>,
    pub setter_name: Option<String>,
    pub setter_descriptor: Option<String>,
    /// JVM-only property-reference boundary: the accessor uses this value class's erased carrier,
    /// while `KProperty.get`/`set` exchange the boxed value-class object through `Object`.
    pub boxed_value_class: Option<TypeName>,
    /// The selected member accessor is declared by an interface. Static extension/top-level
    /// accessors ignore this bit; instance references use it to choose `invokeinterface` without
    /// querying a class model again during emission.
    pub owner_is_interface: bool,
    pub prop_ty: Ty,
    /// `false` = an unbound `Type::prop` (a `PropertyReference1Impl` singleton with `get(Object)`);
    /// `true` = a bound `obj::prop` (a `PropertyReference0Impl` constructed with the captured receiver,
    /// whose `get()` reads `this.receiver`).
    pub bound: bool,
    /// A top-level property reference `::foo` (a `(Mutable)PropertyReference0Impl` singleton): the
    /// getter/setter are STATIC on the file facade, so `get`/`set` dispatch via `invokestatic`
    /// (`owner_internal = None` is resolved at emit). No receiver is captured.
    pub static_dispatch: bool,
    /// The referenced property is a `var` — emit a `set(Object)` override (calls `setName`). Only
    /// meaningful with `static_dispatch` (a `MutablePropertyReference0Impl`).
    pub mutable: bool,
    /// An EXTENSION property reference (`obj::ext`, `Type::ext` where `val Recv.ext`): the getter/setter
    /// are STATIC methods on this facade taking the receiver as the first argument (`getExt(Recv)` /
    /// `setExt(Recv, v)`), unlike a member reference's instance `getExt()`. `None` for member/top-level
    /// references. The reference's receiver-class metadata still lives in `owner_internal`.
    pub ext_facade: Option<Option<TypeName>>,
}

impl FuncRef {
    pub fn owner_class_or_facade(&self, facade: &str) -> String {
        self.owner_class
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn call_owner_or_facade(&self, facade: &str) -> String {
        self.call_owner
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn call_owner_key(&self) -> String {
        self.call_owner.map(TypeName::render).unwrap_or_default()
    }

    pub fn call_owner_is_facade(&self) -> bool {
        self.call_owner.is_none()
    }
}

impl PropRef {
    pub fn owner_or_facade(&self, facade: &str) -> String {
        self.owner_internal
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn owner(&self) -> Option<String> {
        self.owner_internal.map(TypeName::render)
    }

    pub fn call_owner(&self) -> Option<String> {
        self.call_owner_internal.map(TypeName::render)
    }

    pub fn ext_facade_or_facade(&self, facade: &str) -> Option<String> {
        self.ext_facade.as_ref().map(|f| {
            f.as_ref()
                .map(|facade| facade.render())
                .unwrap_or_else(|| facade.to_string())
        })
    }
}

impl IrClass {
    /// Minimal backend-neutral shape for a compiler-generated class. The producer sets only the
    /// semantic payload it owns (for example `prop_ref`); target passes choose representation.
    pub fn synthetic(fq_name: TypeName) -> Self {
        Self {
            fq_name,
            is_source_declared: false,
            is_anonymous_object: false,
            enclosing_function: None,
            is_inner_class: false,
            is_local_class: false,
            is_value: false,
            is_data: false,
            decl_line: 0,
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            captured_type_params: Vec::new(),
            supertypes: Vec::new(),
            properties: Vec::new(),
            fields: Vec::new(),
            ctor_param_count: 0,
            constructor_prefix_count: 0,
            ctor_args: Vec::new(),
            ctor_param_annotations: Vec::new(),
            init_body: None,
            pre_super_param_fields: Vec::new(),
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: false,
            is_fun_interface: false,
            is_annotation: false,
            annotation_impl_of: None,
            is_sealed: false,
            sealed_subclasses: Default::default(),
            is_abstract: false,
            is_open: false,
            superclass: crate::types::wk::any(),
            super_arg_prelude: Vec::new(),
            super_args: Vec::new(),
            super_ctor_params: Vec::new(),
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            bridges: Vec::new(),
            interfaces: Default::default(),
            is_object: false,
            is_companion: false,
            companion_class: None,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: DeclarationAnnotations::default(),
            field_annotations: Vec::new(),
            property_annotations: Vec::new(),
            primary_ctor_annotations: DeclarationAnnotations::default(),
            annotation_retention: None,
        }
    }

    /// Build the declaration-only portion available from the pending-free module index. Other
    /// declaration families enrich this class through their own stable identities; this constructor
    /// performs no syntax lookup.
    pub fn source_skeleton(
        header: &crate::fir::ResolvedClassifierHeader,
        flags: crate::fir::DeclarationFlags,
    ) -> Self {
        let superclass = header
            .superclass
            .and_then(|ty| ty.get().non_null().obj_internal())
            .unwrap_or_else(crate::types::wk::any);
        let interfaces = header
            .interfaces
            .iter()
            .filter_map(|ty| ty.get().non_null().obj_internal())
            .collect::<Vec<_>>()
            .into();
        let mut supertypes = Vec::with_capacity(1 + header.interfaces.len());
        if let Some(superclass) = header.superclass {
            supertypes.push(superclass.get());
        }
        supertypes.extend(header.interfaces.iter().map(|interface| interface.get()));
        let context_fields = header
            .context_parameters
            .iter()
            .enumerate()
            .map(|(ordinal, parameter)| {
                IrField::new(
                    format!("$context_receiver_{ordinal}"),
                    crate::types::stored_value_ty(parameter.ty.get()),
                )
                .with_is_final(true)
            })
            .collect::<Vec<_>>();
        let context_arguments = header
            .context_parameters
            .iter()
            .enumerate()
            .map(|(field, parameter)| IrCtorArg {
                name: None,
                ty: crate::types::stored_value_ty(parameter.ty.get()),
                declared_ty: Some(parameter.ty.get()),
                is_field: true,
                field_index: Some(u32::try_from(field).expect("too many context fields")),
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
            })
            .collect::<Vec<_>>();
        let context_count =
            u32::try_from(context_arguments.len()).expect("too many classifier context parameters");
        Self {
            fq_name: header.classifier,
            is_source_declared: true,
            is_anonymous_object: flags.has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT),
            enclosing_function: None,
            is_inner_class: flags.has(crate::fir::DeclarationFlags::INNER),
            is_local_class: flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS),
            is_value: flags.has(crate::fir::DeclarationFlags::VALUE),
            is_data: flags.has(crate::fir::DeclarationFlags::DATA),
            decl_line: 0,
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            captured_type_params: Vec::new(),
            supertypes,
            properties: Vec::new(),
            fields: context_fields,
            ctor_param_count: context_count,
            constructor_prefix_count: context_count,
            ctor_args: context_arguments,
            ctor_param_annotations: Vec::new(),
            init_body: None,
            pre_super_param_fields: Vec::new(),
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: flags.has(crate::fir::DeclarationFlags::INTERFACE),
            is_fun_interface: flags.has(crate::fir::DeclarationFlags::FUN_INTERFACE),
            is_annotation: flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS),
            annotation_impl_of: None,
            is_sealed: flags.has(crate::fir::DeclarationFlags::SEALED),
            sealed_subclasses: header.sealed_subclasses.to_vec().into(),
            is_abstract: flags.has(crate::fir::DeclarationFlags::ABSTRACT),
            is_open: flags.has(crate::fir::DeclarationFlags::OPEN),
            superclass,
            super_arg_prelude: Vec::new(),
            super_args: Vec::new(),
            super_ctor_params: Vec::new(),
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            bridges: Vec::new(),
            interfaces,
            is_object: flags.has(crate::fir::DeclarationFlags::SINGLETON),
            is_companion: flags.has(crate::fir::DeclarationFlags::COMPANION),
            companion_class: None,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: DeclarationAnnotations::default(),
            field_annotations: Vec::new(),
            property_annotations: Vec::new(),
            primary_ctor_annotations: DeclarationAnnotations::default(),
            annotation_retention: None,
        }
    }

    pub fn fq_name_id(&self) -> TypeName {
        self.fq_name
    }

    /// Whether the property named `name` carries `@JvmField`. Production common lowering retains the
    /// resolved annotation identity on [`IrProperty`]; the richer field-annotation record remains the
    /// legacy metadata path's equivalent source of the same semantic fact.
    pub fn property_has_jvm_field(&self, name: &str) -> bool {
        self.properties.iter().any(|property| {
            property.name == name
                && property
                    .annotations
                    .iter()
                    .any(|annotation| annotation.matches("kotlin/jvm/JvmField"))
        }) || self.field_annotations.iter().any(|fa| {
            fa.field == name
                && fa
                    .annotations
                    .applications()
                    .any(|a| a.internal.matches("kotlin/jvm/JvmField"))
        })
    }

    pub fn fq_name(&self) -> String {
        self.fq_name.render()
    }

    pub fn fq_name_matches(&self, internal: &str) -> bool {
        self.fq_name.matches(internal)
    }

    pub fn superclass(&self) -> String {
        self.superclass.render()
    }

    pub fn superclass_matches(&self, internal: &str) -> bool {
        self.superclass.matches(internal)
    }

    pub fn has_non_top_superclass(&self) -> bool {
        !self.superclass.matches("")
            && !self.superclass.matches("java/lang/Object")
            && !self.superclass.matches("kotlin/Any")
    }

    pub fn annotation_impl_of(&self) -> Option<String> {
        self.annotation_impl_of.map(TypeName::render)
    }

    pub fn companion_class(&self) -> Option<String> {
        self.companion_class.map(TypeName::render)
    }

    pub fn companion_class_matches(&self, internal: &str) -> bool {
        self.companion_class
            .is_some_and(|name| name.matches(internal))
    }

    pub fn is_singleton(&self) -> bool {
        self.is_object || self.is_companion
    }
}

/// A secondary constructor: `<init>(params)` runs `delegate_prelude`, loads `delegate_args`, calls the
/// delegate target, then runs `body`. `this` is value 0 and parameters are values `1..=params.len()`.
#[derive(Clone, Debug)]
pub struct IrSecondaryCtor {
    /// User annotations declared on this constructor, split by JVM retention — the constructor
    /// analogue of [`IrFile::function_annotations`] (a secondary constructor is not an
    /// [`IrFunction`], so it carries them directly).
    pub annotations: DeclarationAnnotations,
    /// Compiler-supplied leading parameters shared by every constructor of the class. These occupy
    /// body value slots before `params`, but are absent from Kotlin source metadata and default masks.
    pub prefix_params: Vec<Ty>,
    pub params: Vec<Ty>,
    /// SOURCE parameter names paired with SEMANTIC (checker-resolved) types — what the class
    /// `@Metadata` `Constructor` record describes (`params` above are the erased IR realization,
    /// which loses fun-type shapes and generic arguments). Empty for a synthesized constructor.
    pub named_params: Vec<(String, Ty)>,
    /// Index into `named_params` of a `vararg` parameter, for the `Constructor` metadata record.
    pub vararg_index: Option<usize>,
    pub defaults: Vec<Option<ExprId>>,
    /// Source-ordered temp declarations for delegation arguments.
    pub delegate_prelude: Vec<ExprId>,
    pub delegate_args: Vec<ExprId>,
    /// Semantic target-parameter ordinals omitted at this delegation site. A backend derives its
    /// own default-constructor ABI (for example JVM masks and marker) from these checked ordinals.
    pub default_parameters: Vec<u32>,
    pub body: Option<ExprId>,
    /// Which `<init>` this constructor delegates to, and whether it runs the class init body.
    pub delegate: CtorDelegateTarget,
    /// kotlinc marks this ctor `ACC_SYNTHETIC` (0x1000) — e.g. a `@Serializable` deserialization ctor.
    pub synthetic: bool,
    /// A DECLARED parameter was value-class-typed (recorded by the value-class pass before erasure):
    /// the ctor gets kotlinc's PRIVATE + public synthetic `(…, DefaultConstructorMarker)` ABI, and
    /// its metadata record names the marker form.
    pub vc_params: bool,
}

/// Semantic declaration metadata retained when the JVM value-class pass replaces a secondary
/// constructor with a static `constructor-impl` realization. The backend owns the physical handle;
/// Kotlin metadata must still describe the original source parameters/defaults and link them to that
/// exact handle for downstream frontend resolution.
#[derive(Clone, Debug)]
pub struct IrJvmValueClassSecondaryCtor {
    pub params: Vec<(String, Ty)>,
    pub param_defaults: Vec<bool>,
    pub vararg_index: Option<usize>,
    pub annotations: DeclarationAnnotations,
    pub descriptor: String,
}

/// The delegation target of a secondary constructor.
#[derive(Clone, Debug)]
pub enum CtorDelegateTarget {
    /// `this(args)` → `invokespecial` an own `<init>(target_params)` (the primary, or a sibling
    /// secondary). The class init body runs in the reached constructor, not here.
    This {
        target_params: Vec<Ty>,
        to_primary: bool,
        default_masks: Vec<i32>,
    },
    /// `super(args)` (or implicit) → the exact checker-selected superclass constructor.
    Super {
        owner: TypeName,
        target_params: Vec<Ty>,
        default_masks: Vec<i32>,
    },
}

/// A JVM declaration adapter (`name(erased_params)erased_ret` → a selected concrete target).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeKind {
    Function,
    PropertyGetter,
    PropertySetter,
    /// The ordinary public instance entry through which a boxed value class implements an interface;
    /// it delegates to the selected static carrier implementation but is not `ACC_BRIDGE|SYNTHETIC`.
    ValueClassInterfaceEntry,
}

#[derive(Clone, Debug)]
pub struct Bridge {
    pub kind: BridgeKind,
    /// Exact same-module function this bridge delegates to. Backend realization uses this stable
    /// identity for representation decisions; `target_name` is emitted spelling, never lookup input.
    pub target_function: Option<u32>,
    pub name: String,
    pub erased_params: Vec<Ty>,
    pub erased_ret: Ty,
    pub concrete_params: Vec<Ty>,
    pub concrete_ret: Ty,
    /// Physical return in the delegated target's descriptor when it differs from the value the bridge
    /// adapts. A suspend target always returns `Object`; a generic bridge may still need to cast that
    /// object to a reference carrier and box it as a value class for the erased supertype boundary.
    pub target_ret: Option<Ty>,
    /// Whether incompatible erased arguments return the collection operation's neutral result.
    pub type_safe_barrier: bool,
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
    /// When set, the bridge boxes its (unboxed value-class) result with `<owner>.box-impl` before
    /// returning — a value-class-returning override seen through a supertype hands back a boxed `X`.
    pub box_ret: Option<TypeName>,
    /// Per concrete parameter, the boxed value class to `checkcast` + `unbox-impl` before the target
    /// call — a generic supertype method (`B.f(T,U)` → erased `f(Object,Object)`) delegates to a
    /// mangled concrete override taking the value class's UNDERLYING, while the incoming arg is a
    /// boxed `X`. Empty (or all-`None`) ⇒ plain checkcast/convert (the common case). JVM/value-class
    /// concern, populated by the value-class pass; the front end leaves it empty.
    pub unbox_params: Vec<Option<TypeName>>,
}

/// A top-level (module) property: a static field on the file facade, initialized in `<clinit>`.
#[derive(Clone, Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: Ty,
    /// The initializer expression (run in `<clinit>` in declaration order).
    pub init: ExprId,
    /// `var` (mutable) ⇒ a setter is emitted and the backing field is non-`final`.
    pub is_var: bool,
    /// `const val` ⇒ kotlinc keeps the field `public static final` (inlined at use) with no accessor;
    /// a plain top-level `val`/`var` is `private static [final]` + a `public static` getter/setter.
    pub is_const: bool,
    /// The class this static field belongs to. `None` = the file facade (a top-level property). `Some`
    /// = a specific class — a `companion object`'s `const val` lives on the OUTER class (kotlinc emits
    /// `public static final` + `ConstantValue` there), not the facade.
    pub owner: Option<TypeName>,
    /// Declaration visibility (`public` by default). A PRIVATE top-level property gets NO public
    /// accessors; cross-class reads inside the file go through a synthesized `access$get<X>$p` bridge
    /// (kotlinc's shape).
    pub visibility: crate::types::Visibility,
    /// `true` when this backing field has a CUSTOM accessor (`val x = init get() = field…`): the field
    /// is still emitted + initialized in `<clinit>`, but the trivial `getX`/`setX` accessors are NOT
    /// auto-generated here — the custom `getX`/`setX` are emitted as ordinary facade methods (their
    /// bodies lowered with `field` bound to this static). Prevents a duplicate-accessor collision.
    pub custom_accessor: bool,
    /// 1-based source line of the property declaration (0 = unknown). kotlinc maps the accessors'
    /// LineNumberTables and the `<clinit>` initializer store to this line.
    pub line: u32,
    /// Source byte offset of the declaration (`u32::MAX` for a target/plugin synthetic). This is the
    /// exact ordering key when class metadata interleaves static and instance properties.
    pub source_order: u32,
}

impl IrStatic {
    pub fn is_facade_owned(&self) -> bool {
        self.owner.is_none()
    }

    pub fn owner_matches(&self, internal: &str) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.matches(internal))
    }
}

#[derive(Clone, Default, Debug)]
pub struct FnParamInfo {
    pub names: Vec<String>,
    pub defaults: Option<Vec<Option<ExprId>>>,
    /// The registered `defaults` serve only the `$default` STUB (which re-emits them inside the
    /// stub's own frame) — a CALL SITE must not reuse them to fill an omitted argument. Set for an
    /// EXTENSION whose defaults are not all constant: kotlinc still emits `name$default` for it (the
    /// cross-module ABI), while krusty's same-module omitted-arg lowering — which inlines only
    /// checker-recorded constant defaults — keeps bailing exactly as before the defaults were
    /// registered (skip, never miscompile).
    pub stub_only: bool,
}

impl FnParamInfo {
    pub fn names(names: Vec<String>) -> Self {
        Self {
            names,
            defaults: None,
            stub_only: false,
        }
    }

    pub fn defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
            stub_only: false,
        }
    }

    /// [`Self::defaults`] with the stub-only marker set — see [`Self::stub_only`].
    pub fn stub_only_defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
            stub_only: true,
        }
    }
}

/// Stable source identity and lexical naming context for one lowered lambda implementation. A
/// source lambda can be lowered more than once (for example into multiple constructors); every such
/// implementation carries the same origin so a backend can realize one closure artifact without
/// recovering identity from generated method names or scanning unrelated expression/value tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrLambdaOrigin {
    /// File-local semantic identity assigned while a checked source lambda is consumed. It is not
    /// an AST id, text offset, or body locator.
    pub identity: u32,
    /// Semantic classifier whose lexical code container owns the implementation, or `None` for the
    /// package facade. Physical method placement may still be changed by a backend pass.
    pub lexical_owner: Option<TypeName>,
    pub enclosing_name: String,
    pub binding_name: Option<String>,
    /// Source-lambda ordinal for class-mode naming within the rendered lexical context.
    pub ordinal: u32,
    /// Backend-neutral source naming stem of the containing executable declaration. A target owns
    /// the separators and complete physical implementation spelling.
    pub implementation_name: String,
    /// Source-lambda implementation ordinal within the enclosing callable name.
    pub implementation_ordinal: u32,
}

/// One lowered source file (`IrFile`) — its arenas. Index-based, bulk-freeable.
#[derive(Default)]
pub struct IrFile {
    pub package: Option<String>,
    pub source_line_count: u32,
    /// Checked file-level annotation applications. These are declaration metadata, not syntax;
    /// backend plugins consume their folded values without retaining or reopening the source AST.
    pub file_annotations: DeclarationAnnotations,
    /// Guards the active-unit metadata handoff when a source is checked in several body groups.
    pub(crate) file_annotations_attached: bool,
    pub functions: Vec<IrFunction>,
    /// Stable checked-FIR callable identity to its realization in this file's function arena.
    /// Common lowering publishes the edge once; checked-operation realization consumes it without
    /// name lookup or overload reconstruction.
    pub checked_callable_functions: std::collections::HashMap<crate::fir::CallableId, FunId>,
    /// Frontend-plugin declaration identity to its predeclared common-IR callable. Backend plugin
    /// realization fills this exact declaration instead of rediscovering a generated member by
    /// owner/name/descriptor or emitting a duplicate alongside it.
    pub plugin_declaration_functions:
        std::collections::HashMap<crate::libraries::PluginExpressionDeclaration, Vec<FunId>>,
    /// Referenced current-module callables copied from finalized declaration headers. The stable
    /// identity remains only as an exact edge; the backend consumes this semantic container record
    /// instead of reopening the frontend module index.
    pub referenced_module_callables:
        std::collections::HashMap<crate::fir::CallableId, IrModuleCallable>,
    /// Referenced current-module properties with their complete checked declaration shape.
    pub referenced_module_properties:
        std::collections::HashMap<crate::fir::PropertyId, IrModuleProperty>,
    /// Referenced current-module singleton classifiers. Backends choose their storage field from
    /// this semantic singleton/companion shape without querying frontend declarations.
    pub referenced_module_classifiers: std::collections::HashMap<TypeName, IrModuleClassifier>,
    /// Complete checked applied hierarchy for every source classifier realized in this file.
    /// Common lowering copies it from the stable FIR index; target passes may inspect target-specific
    /// representation rules but must not reconstruct semantic inheritance through frontend lookup.
    pub classifier_hierarchies: std::collections::HashMap<TypeName, Vec<IrAppliedClassifier>>,
    /// Exact semantic property-override edges copied from stable FIR. A target backend may erase
    /// these types and materialize representation bridges, but it must not search declarations.
    pub property_overrides: std::collections::HashMap<TypeName, Vec<IrPropertyOverride>>,
    pub function_overrides: std::collections::HashMap<TypeName, Vec<IrFunctionOverride>>,
    /// Stable checked-FIR classifier declaration to its common-IR class realization. Member bodies
    /// attach through this edge; neither the sink nor a backend searches by rendered class name.
    pub checked_classifier_classes: std::collections::HashMap<crate::fir::DeclarationId, ClassId>,
    /// Stable `(classifier declaration, interface-delegation ordinal)` to its generated storage
    /// field. Common lowering predeclares this source-ordered layout once and both checked
    /// constructor initializers and forwarding-plan materialization consume the exact coordinate.
    pub checked_interface_delegation_fields:
        std::collections::HashMap<(crate::fir::DeclarationId, u32), u32>,
    /// Interface-delegation ordinals whose checked constructor-body initializer has been consumed.
    /// Final forwarding materialization uses this identity edge as a completeness invariant; it
    /// never searches the constructor IR for a matching field assignment.
    pub checked_interface_delegation_initializers:
        std::collections::HashSet<(crate::fir::DeclarationId, u32)>,
    /// Stable enum-entry declaration to its compiler-generated common-IR subclass. The entry keeps
    /// the enum classifier as its semantic receiver in FIR; this transient edge only owns where its
    /// declared methods/properties are physically attached in the active file.
    pub checked_enum_entry_classes: std::collections::HashMap<crate::fir::DeclarationId, ClassId>,
    /// Source constructors consumed from checked FIR, keyed by stable declaration identity. This is
    /// common-IR state, not retained frontend state; every operand belongs to this file's IR arena.
    pub checked_constructor_bodies:
        std::collections::HashMap<crate::fir::DeclarationId, IrCheckedConstructorBody>,
    /// Stable property identity to its source-oriented common-IR declaration.
    pub checked_properties: std::collections::HashMap<crate::fir::PropertyId, IrCheckedProperty>,
    /// Stable property identity to the storage/accessor declarations materialized in this file.
    /// Ordinary reads and writes remain checked semantic operations until a target consumes this
    /// table and chooses its representation.
    pub local_property_layouts:
        std::collections::HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    /// Expression identity → the source classifier whose body owns it. Package-level bodies are
    /// absent. This is a semantic containment edge recorded while consuming FIR; target backends
    /// use it for representation decisions that depend on crossing a class-file boundary without
    /// reconstructing ownership from generated function or class names.
    pub expression_owners: std::collections::HashMap<ExprId, TypeName>,
    /// Class initialization blocks in source order. Member-property initializers remain attached to
    /// their declarations so a backend can merge both without consulting syntax.
    pub checked_class_initializers: Vec<IrCheckedClassInitializer>,
    pub checked_enum_entry_bodies:
        std::collections::HashMap<crate::fir::DeclarationId, IrCheckedEnumEntryBody>,
    pub checked_script_body: Option<ExprId>,
    /// Lifted function parameter that carries a mutable capture's shared holder, keyed by
    /// `(function, parameter)`. `Ty` is the logical captured element type; each backend chooses its
    /// holder representation. Keeping this sparse semantic edge separate prevents JVM `Ref` classes
    /// from leaking into checked FIR or common function signatures.
    pub shared_capture_parameters: std::collections::HashMap<(FunId, u32), Ty>,
    /// A local or anonymous class field that carries a mutable capture's shared holder, keyed by
    /// `(class, field)`. The stored `Ty` is the logical captured element type; the field and its
    /// constructor argument deliberately retain that semantic type in common IR. Backends realize
    /// the holder representation from this exact coordinate without inferring it from a field name,
    /// constructor position, or expression shape.
    pub shared_class_capture_fields: std::collections::HashMap<(ClassId, u32), Ty>,
    /// Body-local static functions physically owned by a class. Their `$default` ABI uses the
    /// ordinary function marker rather than constructor/value-class markers.
    pub class_static_local_functions: std::collections::HashSet<FunId>,
    pub classes: Vec<IrClass>,
    /// Source type aliases declared directly in a classifier, keyed by that classifier's semantic
    /// identity. These are pending-free declaration headers copied by common lowering; they carry
    /// no body, parser identity, source range, or backend representation.
    pub class_type_aliases: std::collections::HashMap<TypeName, Vec<IrTypeAlias>>,
    /// Source functions declared directly in this file's package. These are complete semantic
    /// declaration records copied from finalized Pass-1 headers. A backend may combine them with
    /// the post-pass physical function realization, but must not reopen the frontend index.
    pub package_functions: Vec<IrPackageFunction>,
    /// Source properties declared directly in this file's package. Storage/accessor representation
    /// remains target-owned; this record contains only checked Kotlin declaration semantics.
    pub package_properties: Vec<IrPackageProperty>,
    /// Source type aliases declared directly in this file's package. Like classifier aliases, these
    /// carry a pending-free expansion and metadata spelling without retaining syntax coordinates.
    pub package_type_aliases: Vec<IrTypeAlias>,
    /// JVM value-class identity to secondary-constructor declarations consumed into static
    /// `constructor-impl` methods. Populated only by the JVM representation pass.
    pub(crate) jvm_value_class_secondary_ctors:
        std::collections::HashMap<TypeName, Vec<IrJvmValueClassSecondaryCtor>>,
    /// Top-level properties — static fields on the facade, initialized in `<clinit>` in order.
    pub statics: Vec<IrStatic>,
    /// Static indices whose storage was moved from a companion declaration to its outer class by the
    /// JVM companion-storage pass. Common lowering never populates this physical realization table.
    jvm_companion_hoisted_statics: std::collections::HashSet<u32>,
    /// Statics realized as `@JvmField` public fields (no accessors, no bridges) by JVM property
    /// storage passes. Common lowering never populates this physical realization table.
    jvm_field_statics: std::collections::HashSet<u32>,
    /// Exact companion property declaration → its JVM outer-class static realization. The property
    /// index is stable within its declaring class; backend consumers must not recover this edge by
    /// matching the property's spelling against a static field name.
    jvm_companion_property_statics: std::collections::HashMap<(TypeName, u32), u32>,
    /// User annotations applied to FUNCTIONS (top-level and members share the `functions` arena),
    /// by function id, retaining the resolved semantic retention. A side table rather than a field on [`IrFunction`], for the
    /// same reason [`IrClass::field_annotations`] is one: the overwhelming majority of functions
    /// carry none, and every synthesized function stays constructible without naming them.
    pub function_annotations: std::collections::HashMap<u32, DeclarationAnnotations>,
    /// `(class, property name)` → the synthetic `get<Name>$annotations()` marker method that carries
    /// that property's annotations. The property's `JvmPropertySignature` names the marker, so
    /// emission reads its FINAL name from here (the value-class pass may have mangled it).
    pub property_annotation_markers: std::collections::HashMap<(TypeName, String), u32>,
    pub exprs: Vec<IrExpr>,
    /// Checked source/synthetic origin for every expression produced by FIR lowering. Legacy IR may
    /// leave this sparse during migration; the consuming FIR path records every generated node.
    pub fir_origins: std::collections::HashMap<ExprId, IrNodeOrigin>,
    /// Checked lexical target depth for source returns lowered from FIR. The return node remains an
    /// ordinary backend-neutral `IrExpr::Return`; inline expansion consumes/decrements this fact as
    /// lambda bodies cross lexical boundaries, so no source label or AST identity survives.
    pub checked_return_depths: std::collections::HashMap<ExprId, u32>,
    /// Sparse construction facts keyed by the ordinary [`IrExpr::New`] identity. Common lowering
    /// keeps one generic construction node; a backend consumes this semantic annotation tag when it
    /// must realize annotation instances through a platform-specific implementation class.
    pub annotation_constructions: std::collections::HashMap<ExprId, IrAnnotationConstruction>,
    /// Current class identity → semantic superclass-constructor parameter ordinals omitted by its
    /// primary delegation. Kept separate from `super_args` so common IR does not encode a target's
    /// mask/marker ABI.
    pub super_constructor_default_arguments: std::collections::HashMap<TypeName, Vec<u32>>,
    /// Current class → exact dependency constructor selected for its primary `super(…)`
    /// delegation. The semantic operands/types remain on [`IrClass`]; the backend fills the opaque
    /// physical descriptor through this provider identity before emission.
    pub(crate) external_super_constructors:
        std::collections::HashMap<TypeName, IrExternalConstructorTarget>,
    /// `(current class, secondary-constructor ordinal)` → exact dependency constructor selected for
    /// that constructor's `super(…)` delegation. Kept beside, rather than inside, the constructor so
    /// non-JVM consumers need not carry a physical realization field on every declaration.
    pub(crate) external_secondary_super_constructors:
        std::collections::HashMap<(TypeName, u32), IrExternalConstructorTarget>,
    /// Exact `SetField` expression identities that realize a source property declaration's
    /// initializer. A later assignment can target the same field with the same value, so backend
    /// storage passes must consume this linkage instead of recognizing stores by shape or spelling.
    pub(crate) property_initializer_stores: std::collections::HashSet<ExprId>,
    /// Sparse `ExprId` → 1-based source line for the `LineNumberTable`: statement roots, loop
    /// updates, and the implicit `Unit` return (the block's closing-brace line, kotlinc's mapping).
    /// Absent = no line mark starts at that expression.
    pub expr_lines: std::collections::HashMap<u32, u32>,
    /// Source line for every lowered expression whose AST node has a source location.
    pub expr_source_lines: std::collections::HashMap<u32, u32>,
    /// Source end line for every lowered expression whose AST node has a source location.
    pub expr_end_lines: std::collections::HashMap<u32, u32>,
    /// Source names for `IrExpr::Variable` nodes included in `LocalVariableTable`.
    /// Compiler-generated temporaries are omitted.
    pub value_names: std::collections::HashMap<u32, String>,
    /// Lifted lambda implementation id → stable source origin and lexical binding context.
    pub lambda_origins: std::collections::HashMap<u32, IrLambdaOrigin>,
    /// `ExprId` → the expression's LOGICAL (source) type as the checker inferred it, recorded verbatim by
    /// the lowerer — NOT erased. The value-class pass consults it to recover the representation of a value
    /// whose IR node alone is ambiguous: a library call returns a physical `Object` descriptor, but its
    /// logical type may be a value class (`runCatching{…}: Result`), so the pass knows the result is the
    /// value class's UNBOXED underlying, not an opaque `Object`. Populated for every lowered expression;
    /// consumed by the value-class pass (the sole owner of value-class knowledge) and — for scalar and
    /// `String` types only, where logical = physical representation — by the suspend pass's operand
    /// snapshot typing (`hoisted_value_ty`) for external callees.
    pub logical_types: std::collections::HashMap<u32, Ty>,
    /// Checked exhaustive `when` expressions without a written `else`, keyed by common-IR identity.
    /// The value is their final semantic result type. Backends use this to preserve value flow and
    /// emit the mandatory no-match failure path without re-running exhaustiveness analysis.
    pub exhaustive_whens: std::collections::HashMap<ExprId, Ty>,
    /// Physical type before a semantic read coercion.
    pub physical_types: std::collections::HashMap<u32, Ty>,
    /// `FunId` → source parameter names and, when present, default-value expressions.
    pub fn_params: std::collections::HashMap<u32, FnParamInfo>,
    /// Per declared method/function, whether each SOURCE parameter was declared nullable (`a: String?`).
    /// The checker `Ty` drops nullability and `IrFunction::params` is kept non-null on purpose — it feeds
    /// the value-class name mangle, which must NOT see recovered nullability (a nullable-VC param mangles
    /// like a non-null one; perturbing it renames the method away from its call sites). `@Metadata` and
    /// the `@NotNull`/`@Nullable` parameter annotations, which DO need the declared nullability, consult
    /// this side-table instead. Empty ⇒ treat every parameter as non-null (the prior behavior).
    pub fn_param_declared_nullable: std::collections::HashMap<u32, Vec<bool>>,
    /// How SOURCE spelled a member's declared types, by `FunId` — see [`crate::spelling`].
    ///
    /// Class `@Metadata` is built from the IR alone (no AST, no `FrontendSymbols`), so a declared
    /// type's `typealias` spelling reaches it the same way a parameter's declared `?` does: on a
    /// side table filled at lowering, where the AST member is still in hand. Only members that
    /// actually spell an alias get an entry.
    pub fn_declared_spellings: std::collections::HashMap<u32, crate::spelling::DeclaredSpellings>,
    /// Source declaration name for a function whose target realization renamed it. Common lowering
    /// initially keeps the Kotlin name on [`IrFunction`]; a backend records that name here before
    /// replacing it with a physical spelling such as JVM `@JvmName` or a later value-class mangle.
    /// Metadata/reflection consume this semantic name while calls use the realized function name.
    pub fn_source_names: std::collections::HashMap<u32, String>,
    /// The same, for a CLASS HEADER (supertypes, primary-constructor parameters, type-parameter
    /// bounds), keyed by the class's fully-qualified name.
    pub class_declared_spellings:
        std::collections::HashMap<crate::types::TypeName, crate::spelling::DeclaredSpellings>,
    /// The same, for a class PROPERTY, keyed by `(class fully-qualified name, property name)` —
    /// a property has no `FunId` to hang off.
    pub prop_declared_spellings: std::collections::HashMap<
        (crate::types::TypeName, String),
        crate::spelling::DeclaredSpellings,
    >,
    /// Function ids realizing an extension receiver among their physical parameters. Metadata and JVM
    /// default-argument masks consume this semantic fact directly; neither may infer receiver-ness
    /// from the synthetic `$receiver` parameter spelling. WHERE that receiver sits is
    /// `fn_context_counts`: Kotlin signs a context extension `(contexts…, receiver, values…)`, so the
    /// receiver is `params[0]` only when the function declares no `context(…)` clause.
    pub extension_receiver_fns: std::collections::HashSet<u32>,
    /// Function id → how many of its LEADING physical parameters are context parameters. Class
    /// `@Metadata` is built from the IR alone, so this is the only carrier telling it to record them
    /// as `Function.context_parameter` (field 13) rather than as ordinary value parameters — without
    /// it a consuming compiler demands them positionally. Absent ⇒ none.
    pub fn_context_counts: std::collections::HashMap<u32, usize>,
    /// Member EXTENSION properties per class (`object Tools { val Int.doubled get() = … }`), keyed by
    /// the declaring class's fq name. Lowering realizes each as accessor METHODS (`getDoubled(I)I`),
    /// which erases property-ness from the IR — class `@Metadata` needs the declaration back: a
    /// `Property` record with `Property.receiver_type` (f5) and the accessor `JvmPropertySignature`,
    /// NOT a `Function` record for the accessor (kotlinc emits none), or a consumer cannot resolve
    /// `import Tools.doubled` / `5.doubled` from the classpath.
    pub member_ext_props: std::collections::HashMap<TypeName, Vec<MemberExtProp>>,
    /// Function ids declared `inline`. This is the declaration-semantic set used by metadata;
    /// visibility-specific inline handling remains in [`Self::public_inline_functions`].
    pub inline_fns: std::collections::HashSet<u32>,
    /// Call expressions whose stable current-module target is semantically `inline`. Target
    /// realization may replace the stable [`Callee::Module`] edge with a physical call, so this
    /// expression-owned fact preserves inline-lambda ownership without asking the backend to recover
    /// declaration semantics from a facade/name pair.
    pub module_inline_calls: std::collections::HashSet<ExprId>,
    /// Ordinary checked call sites whose selected declaration is semantically inline. Unlike a
    /// target callee handle, this fact survives provider realization and is available even when a
    /// public inline call legally remains as a non-inlined fallback.
    pub inline_call_sites: std::collections::HashSet<ExprId>,
    /// Complete evaluation regions for semantically inline calls, including any source-order
    /// operand prelude and any consumed inline-body template. This target-neutral fact survives
    /// provider realization and structural expansion, so backends need not reconstruct an inline
    /// call from the resulting block/loop shape.
    pub inline_regions: std::collections::HashSet<ExprId>,
    /// Function ids declared `operator` — `@Metadata` marks `Function.flags` bit 8 (`isOperator`)
    /// so a consumer admits the conventional call form (`recv(args)` for `invoke`, `a[i]` for
    /// `get`, …); the JVM method itself carries no such bit.
    pub operator_fns: std::collections::HashSet<u32>,
    /// Function ids declared `infix` — `@Metadata` marks `Function.flags` bit 9 (`isInfix`) so a
    /// consumer admits the `a f b` call form; like `operator`, only metadata carries it.
    pub infix_fns: std::collections::HashSet<u32>,
    /// Per declared method/function, the user annotations on each SOURCE parameter, parallel to
    /// [`IrFunction::params`] (so an extension's leading receiver slot is present and empty). Absent ⇒
    /// no parameter of that function carries one, the overwhelmingly common case; the JVM emitter and
    /// `@Metadata` both read it, so it must not be folded into either representation. Retention stays
    /// SEMANTIC here — the JVM split into visible/invisible attributes belongs to the emitter.
    pub fn_param_annotations: std::collections::HashMap<u32, Vec<DeclarationAnnotations>>,
    /// Per declared function, whether each PHYSICAL source parameter carries Kotlin's semantic
    /// `@NoInfer` type-use marker. Extension receivers occupy their physical slot with `false`;
    /// metadata projection removes that slot again. This is inference policy, not a JVM fact.
    pub fn_param_no_infer: std::collections::HashMap<u32, Vec<bool>>,
    /// Function id → semantic strict-equality bound declared on `equals`' value parameter.
    /// Resolution owns the annotation lookup and class-literal checking; common lowering only
    /// preserves the resulting type so Kotlin metadata can publish it to dependent modules.
    pub fn_equality_bounds: std::collections::HashMap<u32, Ty>,
    /// Class identity → per-primary-constructor-parameter checked default expression (`None` =
    /// required). This is the target-neutral constructor contract. A target backend consumes it when
    /// choosing that class's physical default-argument ABI; common lowering does not distinguish value
    /// classes from ordinary classes here. Expressions use the ordinary constructor frame (`this` =
    /// value 0, parameters = 1..=n) until a backend deliberately reframes them.
    class_ctor_defaults: std::collections::HashMap<TypeName, Vec<Option<u32>>>,
    /// Instance methods kotlinc leaves NON-`final` even in a final class — currently the data-class
    /// `Object`-overrides (`toString`/`hashCode`/`equals`), which kotlinc emits `public` (open) rather
    /// than `public final`. The JVM backend omits `ACC_FINAL` for a `FunId` in this set.
    pub open_methods: std::collections::HashSet<u32>,
    /// Instance methods kotlinc emits `private` — currently a property's `private set` setter. The JVM
    /// backend uses `ACC_PRIVATE` instead of `ACC_PUBLIC` for a `FunId` in this set.
    pub private_methods: std::collections::HashSet<u32>,
    /// Private instance methods referenced from synthesized callable-reference classes. The JVM
    /// backend emits one declaration-owned static access bridge for each exact method identity.
    pub function_reference_access_bridges: std::collections::HashSet<u32>,
    /// Lambda impls pre-marked `inline_only` by `mark_must_inline_lambdas` (a must-inline callee's
    /// message lambda, assumed spliced). If emission nonetheless records an `invokedynamic` for one,
    /// the two-pass driver RESCUES it — emits the method after all — so the reference never dangles.
    pub must_inline_lambdas: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_SYNTHETIC` — currently a value class's `box-impl`/`unbox-impl` (the
    /// compiler-manufactured box adapters). The JVM backend ORs `0x1000` for a `FunId` in this set.
    pub synthetic_methods: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_BRIDGE` (0x40) — e.g. a `@Serializable` serializer's
    /// `typeParametersSerializers`. The JVM backend ORs `0x40` for a `FunId` in this set.
    pub bridge_methods: std::collections::HashSet<u32>,
    /// Per-method (`FunId`) `(param index, boxed value-class type)` for params whose value class has a
    /// NULLABLE underlying: the base (mangled) method unboxes them, but its `<name>$default` synthetic
    /// keeps them BOXED (kotlinc — a `$default` can't disambiguate the unboxed signature without the
    /// `-<hash>` mangling). Recorded by the value-class pass BEFORE erasure; read by `emit_default_stub`
    /// (signature + box-on-fill + unbox-on-delegate) AND the `$default` CALL site (boxed arg + descriptor).
    pub default_stub_boxed_params: std::collections::HashMap<u32, Vec<(usize, crate::types::Ty)>>,
    /// The subset declared in this MODULE's SOURCE (this file or a sibling). Whether such a class ends up
    /// carrying an `@Metadata` record is decided by its own emit, so a record here cannot assume it does —
    /// unlike a CLASSPATH value class, whose value-class-ness is itself decoded from that record.
    pub module_source_value_classes: std::collections::HashSet<TypeName>,
    /// Current-module source value classes whose stable declaration shape is supported by the class
    /// metadata writer. This is frozen from Pass-1 headers before bodies stream, allowing one source
    /// file to publish a signature mentioning a sibling value class without retaining or reopening
    /// that sibling's body.
    pub module_readable_value_classes: std::collections::HashSet<TypeName>,
    /// Internal names of classes kotlinc marks `ACC_SYNTHETIC` (0x1000) on the class itself — e.g. a
    /// `@Serializable` class's generated `$$serializer` object.
    synthetic_classes: std::collections::HashSet<TypeName>,
    /// `FunId`s of methods carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's `get<Prop>$annotations()` markers, which kotlinc deprecates HIDDEN. ASM
    /// surfaces the attribute as `ACC_DEPRECATED` (0x20000) in the access int, so the ABI gate compares it.
    pub deprecated_methods: std::collections::HashSet<u32>,
    /// Internal names of classes carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's generated `$$serializer` object, which kotlinc deprecates HIDDEN.
    deprecated_classes: std::collections::HashSet<TypeName>,
    /// Internal names of classes whose primary constructor has a value-class-typed parameter (a
    /// `data class Rec(val id: ItemId, …)`). kotlinc makes such a primary `<init>` PRIVATE and adds a
    /// PUBLIC|SYNTHETIC accessor `<init>(…args, DefaultConstructorMarker)` that delegates to it — its ABI
    /// for a constructor mentioning an inline class. Recorded by the value-class pass BEFORE it erases the
    /// parameter types (which lose the value-class identity).
    value_param_ctors: std::collections::HashSet<TypeName>,
    /// For a class in `value_param_ctors`: its primary-ctor parameter types AS DECLARED (recorded
    /// before the value-class pass erased them), positionally parallel to `IrClass::ctor_args`. The
    /// class `@Metadata` constructor record names these (`id: ItemId`), while the physical
    /// descriptor spells the erased marker form.
    vc_ctor_declared_params: std::collections::HashMap<TypeName, Vec<Ty>>,
    /// Lambda impl functions that are INLINE-ONLY — their body has a non-local `return` (returning from
    /// the enclosing function), which is valid only when the lambda is spliced at the call site, never as
    /// a standalone closure method (a non-local return can't compile to a separate method — its `areturn`
    /// would carry the enclosing fn's return type, mismatching the lambda's). The splice reads the
    /// lambda's `inline_body`, not this method, so the backend must NOT emit a `FunId` in this set.
    pub inline_only_fns: std::collections::HashSet<u32>,
    /// Retained inline declarations from another source unit, materialized only as call-site
    /// cloning templates in the active common-IR arena. They are never emitted as declarations in
    /// this file, and a non-inlined fallback keeps its stable module callable edge.
    pub foreign_inline_templates: std::collections::HashSet<u32>,
    /// Top-level functions declared `inline`. This is a source-semantic fact; each backend decides
    /// how an inline declaration is represented (the JVM emitter, for example, adds kotlinc's
    /// `$i$f$<name>` local marker to emitted non-suspend bodies).
    pub top_level_inline_functions: std::collections::HashSet<u32>,
    /// `FunId`s of `suspend fun`s, tagged by ir_lower. The coroutine pass (`jvm::suspend`) owns the
    /// whole transform: it rewrites each to the continuation-passing-style ABI (an extra
    /// `kotlin.coroutines.Continuation` parameter, return type erased to `Object`) and, for a function
    /// with suspension points, builds the state machine + continuation class. ir_lower itself lowers a
    /// `suspend fun` as a plain function (mirroring how value classes stay plain until their pass).
    pub suspend_funs: Vec<u32>,
    /// Methods the source declares WITHOUT `override` — a fresh declaration rather than an override of a
    /// supertype member. A language fact nothing else in the IR records: `IrFunction` carries a signature,
    /// not the modifier, and a SYNTHESIZED method (absent here) is deliberately indistinguishable from an
    /// override, since both can legitimately be reached through a supertype's descriptor. The JVM bridge
    /// pass needs it to refuse to point a supertype's descriptor at a same-named fresh declaration.
    pub fresh_method_decls: Vec<u32>,
    /// Each rewritten `suspend fun`'s DECLARED signature `(params, ret)`, captured by the coroutine
    /// pass just before it appends the `Continuation` and erases the return. `@Metadata` and the JVM
    /// generic `Signature` both describe the function in Kotlin terms — `suspend fun f(a: String)`,
    /// not `f(String, Continuation): Object` — so both need the signature the CPS rewrite consumed.
    pub suspend_declared_sigs: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
    /// Each function the value-class pass rewrote, keyed by `FunId` → its DECLARED `(name, params,
    /// ret)` before the pass mangled the name and erased the value classes to their underlying types.
    /// `@Metadata` names the Kotlin function and its declared parameter types (`ItemId`, not
    /// `String`); the mangled name and erased descriptor ride along as a `JvmMethodSignature`. Captured
    /// before the CPS rewrite, so a function that is both `suspend` and value-class-typed reports the
    /// fully declared signature here rather than the half-lowered one.
    pub vc_declared_sigs: std::collections::HashMap<u32, (String, Vec<Ty>, Ty)>,
    /// Top-level source declaration id → its exact IR function id. Metadata emission uses this
    /// checked declaration handoff to find a value-class-rewritten physical realization; it must
    /// never reconstruct an overload by matching source names and arity.
    pub top_level_function_fids: std::collections::HashMap<u32, u32>,
    /// Exact IR function ids of public inline declarations. A synthesized class referenced from one
    /// of these bodies must be public because the body can be spliced into another package/module.
    /// Lowering carries the current [`IrFunctionScope`] rather than recovering this fact from a name.
    pub public_inline_functions: std::collections::HashSet<u32>,
    /// Each `suspend fun` whose logical return is a non-null value class, mapped to the exact
    /// representation selected by the target value-class pass for the CPS boundary.
    pub value_class_suspend_returns: std::collections::HashMap<u32, IrValueClassSuspendResult>,
    /// Cross-unit counterpart of [`Self::value_class_suspend_returns`], keyed by the exact checked
    /// suspension call expression.
    pub value_class_suspend_calls: std::collections::HashMap<ExprId, IrValueClassSuspendResult>,
    /// `ExprId` of each direct call to a `suspend fun` → the callee's LOGICAL return type (the source
    /// return, before CPS erasure to `Object`). Recorded by ir_lower from the resolver
    /// (`flags.suspend`), so the coroutine pass recognizes a suspend call to ANOTHER file or a classpath
    /// dependency — whose `FunId` is absent from this file's `suspend_funs`. Same-file/member suspend
    /// calls are caught by `suspend_funs`; this is the cross-unit complement.
    pub suspend_calls: std::collections::HashMap<u32, Ty>,
    /// Non-call expressions whose evaluation is a coroutine SUSPENSION POINT → their logical result
    /// type. Unlike [`Self::suspend_calls`], these nodes do not name a callee and must not have a
    /// continuation argument appended by the coroutine pass. The motivating intrinsic is an inlined
    /// `suspendCoroutineUninterceptedOrReturn` block: lowering has already materialized its continuation
    /// use inside the block, while the coroutine pass still needs to split and resume around the block as
    /// one atomic point. Keeping this semantic category separate prevents a structural block from being
    /// mistaken for a cross-unit call merely because both can suspend.
    pub intrinsic_suspension_points: std::collections::HashMap<u32, IrIntrinsicSuspensionPoint>,
    /// A `suspend` LAMBDA's `invokeSuspend` that contains MULTIPLE suspensions / control flow and needs
    /// a state machine with the lambda instance itself as the continuation — `(invokeSuspend FunId,
    /// lambda ClassId, field_base)`. `field_base` is the first free field index on the lambda class
    /// (after its captures/parameters), where the coroutine pass appends the `result`/`label`/spilled
    /// fields. ir_lower builds `invokeSuspend` with the plain body (suspend calls un-threaded); the pass
    /// flattens it. (Single-suspension lambdas are handled inline by ir_lower instead.) `field_base` is
    /// the number of leading capture/parameter fields — the pass reloads them into locals `2..` at each
    /// `invokeSuspend` entry (so a captured/parameter value survives a re-entry), excludes them from
    /// spilling, and places the result/label/spilled fields after them.
    pub suspend_lambda_sm: Vec<(u32, u32, u32)>,
    /// `FunId` → the backend-agnostic generic-signature SHAPE of a type-parameterized function. The JVM
    /// backend formats this into a `Signature` attribute; the IR itself holds no target descriptors.
    pub signatures: std::collections::HashMap<u32, IrGenericSig>,
    /// Class MEMBERS whose semantic parameter/return types mention an ENCLOSING-CLASS type parameter
    /// (`open class Base<T> { open fun choose(value: T): T }`): fid → (semantic params, semantic ret).
    /// The erased [`IrFunction`] carries `Any`, and `signatures` only describes function-OWNED type
    /// parameters — without this record the class `@Metadata` publishes `choose(Any): Any` and a
    /// consumer rejects a `Base<String>` override with "return type mismatch".
    pub member_semantic_sigs: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
    /// Kotlin declaration visibility by `(class internal name, property name)`. This is distinct from
    /// the backing field's JVM visibility.
    pub prop_visibilities: std::collections::HashMap<(String, String), crate::types::Visibility>,
    /// Kotlin declaration visibility per CLASS — `@Metadata` `Class.flags` must carry it
    /// (`internal class Hidden` writes explicit visibility 0) so a consumer enforces the module
    /// boundary; absent = public (the historical assumption).
    pub class_visibilities: std::collections::HashMap<TypeName, crate::types::Visibility>,
    /// 1-based source line of a class's primary-ctor closing `)` — kotlinc maps the ctor
    /// `$default` overload's `return` to it. Absent = single-line/unknown (the one-entry table).
    pub ctor_close_lines: std::collections::HashMap<TypeName, u32>,
    /// Function ids of `internal` members — `@Metadata` `Function.flags` visibility 0 (the JVM
    /// method stays public; only metadata carries the module boundary). `private_methods` keeps
    /// its own set because privacy ALSO changes dispatch (`invokespecial`).
    pub internal_methods: std::collections::HashSet<u32>,
    /// Declared PRIMARY-constructor visibility per class (`class C protected constructor(…)`);
    /// absent = public. `@Metadata` `Constructor.flags` carries it so a consumer rejects a
    /// construction the declaration forbids.
    pub ctor_visibilities: std::collections::HashMap<TypeName, crate::types::Visibility>,
    /// SOURCE index of a member function's `vararg` parameter (receiver excluded) — class
    /// `@Metadata` must emit `ValueParameter.vararg_element_type` (f4) or a consumer demands one
    /// literal array (`too many arguments`).
    pub fn_vararg_index: std::collections::HashMap<u32, usize>,
    /// Synthesized classes (function-reference/suspend-conversion adapters) that must be PUBLIC:
    /// they are referenced from a PUBLIC INLINE function's body, whose splice copies the reference
    /// into arbitrary other packages/modules (kotlinc marks such synthetics public for the same
    /// reason). Package-private would be an IllegalAccessError at every cross-package splice site.
    pub public_synthetics: std::collections::HashSet<TypeName>,
    /// Declaring class to indices in `statics` for class properties whose JVM field is static.
    pub declared_class_statics: std::collections::HashMap<TypeName, Vec<u32>>,
    /// (class internal name, property name) → 1-based source line of a BODY property's declaration.
    /// kotlinc attributes both the property's getter and its constructor-side initializer to this line.
    pub prop_decl_lines: std::collections::HashMap<(TypeName, String), u32>,
    /// FunId → 1-based source line of its `fun` declaration, for the method's `LineNumberTable`.
    /// A side map (not a field on `IrFunction`) so the 40-odd construction sites stay untouched.
    pub fn_decl_lines: std::collections::HashMap<u32, u32>,
    /// FunId → 1-based source line of a BLOCK body's closing `}` — kotlinc maps a `Unit` fn's
    /// implicit `return` there in the `LineNumberTable`. Same side-map rationale as `fn_decl_lines`.
    pub fn_close_lines: std::collections::HashMap<u32, u32>,
    /// FunId → 1-based source line of the DECLARATION itself, never body-rewritten. `fn_decl_lines`
    /// holds the line the method BODY maps to (an expression body's own line); a `$default` stub's
    /// LineNumberTable instead points here — the two differ exactly when an expression body starts
    /// on a later line than the signature. Same side-map rationale as `fn_decl_lines`.
    pub fn_sig_lines: std::collections::HashMap<u32, u32>,
    /// FunId → the SOURCE BYTE OFFSET of the declaration a class member realizes (a property's
    /// accessors carry the property's offset). kotlinc emits class members in DECLARATION order,
    /// interleaving property accessors among functions; lowering groups them by kind, so the JVM
    /// emitter re-sorts its emission (only) by this key. A method with no entry (a data-class
    /// synthetic, an appended lambda impl, an access bridge) keeps its position after the declared
    /// members. Resolution-facing indexes never see the sorted order.
    pub fn_source_order: std::collections::HashMap<u32, u32>,
    /// Class fq-internal-name → its generic-signature SHAPE (type parameters + bounds), for a generic
    /// class. The JVM backend formats it into the class `Signature` attribute.
    class_signatures: std::collections::HashMap<TypeName, IrGenericSig>,
    /// Class fq-internal-name → `(field name, type-parameter name)` for each field whose declared type
    /// is a bare type parameter (`class Pair<A, B>(val a: A)` → `[("a", "A")]`). The JVM backend formats
    /// each into a field `Signature` (`TA;`). Backend-agnostic: only the type-parameter name is stored.
    field_signatures: std::collections::HashMap<TypeName, Vec<(String, String)>>,
    /// Classpath `@JvmInline value class` (fq-internal-name → erased underlying `Ty`) REFERENCED in
    /// this file. The JVM value-class pass merges these into its erasure map so a dependency value class
    /// unboxes exactly like a same-file declaration. Populated by ir_lower (which has the classpath);
    /// native unsigned builtins keep their dedicated `Ty`/runtime handling and are not recorded here.
    external_value_classes: std::collections::HashMap<TypeName, Ty>,
    /// Expression identity → `(declared value-class name, erased underlying type)` for a construction
    /// rewritten in place by the JVM value-class pass. This records semantic origin rather than the
    /// generated helper's spelling: a source `new` remains distinguishable from an unrelated static call
    /// whose user-written name happens to resemble a backend helper. Consumers must treat the entry as
    /// valid only while the rewritten expression remains at the same arena index.
    erased_value_constructions: std::collections::HashMap<ExprId, (TypeName, Ty)>,
    /// Getter method name (`getV`) for each classpath `@JvmInline value class` in
    /// [`Self::external_value_classes`] — lets the value-class pass recognize a sole-property read emitted
    /// as `invokevirtual X.getV()` and rewrite it to identity (the receiver IS the unboxed underlying).
    /// Call `ExprId` → reified-type substitution for a `<reified T>` CLASSPATH inline extension whose
    /// compiled body the backend must splice: `[(type-parameter name, concrete JVM internal name)]`
    /// (`[("T", "lib/Prov")]`). The bytecode splicer feeds this to `substitute_reified` so a
    /// `reifiedOperationMarker`/`T::class` in the spliced body specializes to the concrete type — the
    /// classpath analogue of the IR inliner's `reified_subst` (which only has same-file bodies). The
    /// concrete type is a backend-agnostic `Ty`; the JVM splicer maps it to an internal name.
    pub reified_call_subst: std::collections::HashMap<u32, Vec<(String, Ty)>>,
    /// Extension-call `ExprId` → the extension's DECLARED (un-erased) receiver source type, forwarded
    /// verbatim from the resolved callable's `source_receiver`. `ir_lower` records it with NO value-class
    /// reasoning of its own; the value-class pass reads it to decide box/unbox at the receiver. The signal
    /// distinguishes `fun Result<T>.getOrThrow()` (receiver `kotlin/Result` — a value class whose facade
    /// method takes the UNBOXED underlying, so a `Boxed` receiver unboxes) from a generic `fun <T> T.foo()`
    /// (receiver a type variable — erases to `Object`, receiver stays boxed) even though both erase
    /// identically in the JVM descriptor. Only concrete declared receivers are recorded (a `Var` receiver
    /// is `None` at the source and never inserted).
    pub ext_call_source_receiver: std::collections::HashMap<u32, Ty>,
    /// Call `ExprId` → the callee's DECLARED (un-erased, pre-substitution) return type, forwarded
    /// verbatim from the resolved library member's `declared_ret`. `ir_lower` records it with NO
    /// value-class reasoning of its own; the value-class pass reads it to decide the RESULT's
    /// representation, exactly as `ext_call_source_receiver` does for the receiver.
    ///
    /// The distinction it carries cannot be recovered from the descriptor: a value class returned by
    /// declaration (`A.create(): A<String>`, whose mangled method hands back the erased carrier) and
    /// the same value class arriving BOXED out of a generic slot (`List<TokenBox>.get`) both
    /// spell `()Ljava/lang/Object;`. The declaration separates them — `create` declares `A`, `get`
    /// declares the type parameter `E` (never recorded, since it is not a class). Only NON-NULL
    /// declared returns are recorded: a nullable value class really is boxed.
    pub call_declared_ret: std::collections::HashMap<u32, Ty>,
    /// Realized dependency-call `ExprId` → declaration parameter types in the order of the call's
    /// ordinary argument vector. These are copied from the provider record selected by FIR, never
    /// reconstructed from a name or descriptor. A backend representation pass needs this sparse fact
    /// only where source and physical shapes are ambiguous—for example, both a direct `Result<T>`
    /// parameter and a generic `T` parameter erase to JVM `Object`, but only the latter takes a box.
    /// Dispatch receivers remain separate; static realizations that consume one prepend its selected
    /// semantic receiver before publishing this vector.
    pub call_declared_params: std::collections::HashMap<u32, Box<[Ty]>>,
    /// Stable property-operation identity → the declaration's semantic value type before
    /// use-site generic substitution. Resolution knows this fact uniformly for every source owner;
    /// recording it here lets a backend derive the physical accessor boundary without asking whether
    /// the declaration came from this file, a sibling file, or a dependency. This deliberately carries
    /// no accessor spelling or target descriptor. As with the JVM realization table below, the stable
    /// operation identity survives backend rewrites that move a property node to another arena slot.
    pub property_declaration_types: std::collections::HashMap<u32, Ty>,
    /// Stable property-operation identity → checker-selected accessor identity and physical return.
    /// This is a semantic selection, distinct from any backend rewrite of its platform spelling.
    pub property_selected_accessors: std::collections::HashMap<u32, (String, Ty)>,
    /// Stable property-operation identity → the exact provider accessor selected by checked FIR.
    /// The identity is target-neutral and opaque; only the owning backend may decode it into a
    /// storage or invocation realization.
    pub property_external_accessors: std::collections::HashMap<u32, crate::fir::ExternalCallableId>,
    /// Stable property-operation identity → JVM accessor spelling and physical property-value type,
    /// selected by the value-class pass for an owner in another source file. The common node keeps the
    /// Kotlin name and logical type; this backend side table carries the declaration-less target
    /// realization only after the JVM pass has enough erasure information. It is deliberately not keyed
    /// by expression arena index because boxing and identity rewrites can move a node.
    pub property_accessor_jvm_realizations: std::collections::HashMap<u32, (String, Ty)>,
    /// Lifted-lambda function id → the parameter INDEX at which the lambda's OWN parameters begin (its
    /// captured variables occupy the lower indices). A lambda's own parameters arrive through the
    /// `FunctionN` generic (`Object`) invoke slot, so a reference-underlying value-class parameter is
    /// BOXED there — the value-class pass reads this to type such a slot as the boxed value class (so
    /// `it.getOrThrow()` unboxes it), without the lowerer probing value-class-ness itself.
    pub lambda_own_params_from: std::collections::HashMap<u32, u32>,
    /// Lifted-lambda function id → the DECLARED parameter types and return type of the user
    /// `fun interface` method the lambda was SAM-converted to. Absent for a plain `FunctionN` lambda,
    /// whose `invoke` slots are all generic. The distinction only matters to a target that erases
    /// some declared types away (the JVM's value classes): a generic slot carries a value class
    /// BOXED, while a slot the SAM method spells as the value class itself carries the erased
    /// underlying — so the lambda's impl method must match whichever the interface actually declares.
    /// The lowerer records the declaration; deciding what erases is the backend pass's job.
    pub lambda_sam_signature: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
    /// Lifted-lambda function id → JVM-physical SAM method parameters and result after value-class
    /// representation has been chosen. Common lowering never populates this table: it retains only
    /// [`Self::lambda_sam_signature`]'s semantic declaration. The JVM value-class pass derives this
    /// realization so emission does not mistake a value-class spelling (`Token`) for the interface
    /// slot that actually exists (`String`, `int`, …).
    pub lambda_sam_jvm_signature: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
}

/// Exact function body currently owned by lowering. `source_name` is only the naming stem for
/// generated methods/classes; semantic properties are keyed by `function`, never reconstructed from
/// that spelling. `None` represents a constructor, property initializer, or class initializer.
#[derive(Clone, Debug, Default)]
pub struct IrFunctionScope {
    pub function: Option<u32>,
    pub source_name: String,
    /// Declaration-owned type-parameter identities whose class-literal operations may remain as
    /// reified placeholders in this emitted method. Kept on the lexical function scope so nested
    /// inline expansion saves/restores the fact with its owner instead of a parallel current-state
    /// field recovering parameters from source spelling.
    pub emitted_reified_parameters: std::collections::HashSet<String>,
}

impl IrFunctionScope {
    pub fn declared(function: u32, source_name: String) -> Self {
        Self {
            function: Some(function),
            source_name,
            emitted_reified_parameters: Default::default(),
        }
    }

    pub fn synthetic(source_name: String) -> Self {
        Self {
            function: None,
            source_name,
            emitted_reified_parameters: Default::default(),
        }
    }

    pub fn with_emitted_reified_parameters(
        mut self,
        parameters: std::collections::HashSet<String>,
    ) -> Self {
        self.emitted_reified_parameters = parameters;
        self
    }
}

/// Backend-agnostic generic-signature shape of a declaration (the data a JVM `Signature` / a future
/// platform's equivalent needs). NO target descriptors here — each backend formats its own.
#[derive(Clone, Debug)]
pub struct IrGenericSig {
    /// Each declared type parameter with its complete semantic bound shape. Whether that bound is an
    /// interface is declaration metadata, not something a backend may infer from a physical name.
    pub type_params: Vec<IrTypeParameter>,
    /// Complete semantic callable parameters for a function signature. An extension receiver occupies
    /// its physical context-boundary slot so a backend signature retains its declared generic type;
    /// declaration metadata separates that slot back into a receiver. Empty for a class signature.
    pub params: Vec<Ty>,
    /// Complete semantic return type for a function signature. `None` for a class signature.
    pub ret: Option<Ty>,
    /// For a CLASS signature with a PARAMETERIZED supertype: the superclass + superinterfaces as
    /// platform-agnostic `Ty`s carrying their type arguments (`[Any, Operation<Result<Int>>]`), so a
    /// cross-module reader recovers a member's concrete generic return. The backend formats these into the
    /// JVM `Signature` string. Empty ⇒ no parameterized supertype (backend emits the default `Object`
    /// superclass). Empty for a function signature.
    pub supers: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct IrTypeParameter {
    pub name: String,
    pub semantic_name: String,
    pub bounds: Vec<(Ty, bool)>,
    pub variance: crate::types::TypeVariance,
    /// Kotlin declaration capability retained for targets that materialize runtime type operations.
    /// The JVM consumes it when choosing its reified-operation marker representation.
    pub reified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrTypeAlias {
    pub name: String,
    pub formals: Vec<String>,
    pub expansion: Ty,
    pub visibility: crate::types::Visibility,
    pub expansion_spelling: crate::spelling::Spelled,
    pub source_order: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrPackageTypeParameter {
    pub name: String,
    pub semantic_name: String,
    pub bounds: Vec<Ty>,
    pub reified: bool,
}

/// Backend-neutral package-function declaration metadata. `function` is the exact common-IR
/// realization; all remaining fields describe the Kotlin declaration before target erasure.
#[derive(Clone, Debug, PartialEq)]
pub struct IrPackageFunction {
    pub function: FunId,
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub receiver: Option<Ty>,
    pub param_defaults: Vec<bool>,
    pub suspend: bool,
    pub inline: bool,
    pub operator: bool,
    pub infix: bool,
    pub contract: Option<crate::contracts::ResolvedContract>,
    pub type_params: Vec<IrPackageTypeParameter>,
    pub context_count: usize,
    pub vararg_index: Option<usize>,
    pub visibility: crate::types::Visibility,
    pub spellings: crate::spelling::DeclaredSpellings,
    pub equality_bound: Option<Ty>,
    pub source_order: u32,
}

/// Backend-neutral package-property declaration metadata. The checked property table retains its
/// exact stable identity while bodies stream; this compact record is what survives into backend
/// metadata formatting after common lowering has finished.
#[derive(Clone, Debug, PartialEq)]
pub struct IrPackageProperty {
    /// Stable semantic property identity. This joins the declaration header to the common-IR
    /// layout selected while its checked body streamed; it is not a source coordinate or a target
    /// storage identity.
    pub property: crate::fir::PropertyId,
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub type_params: Vec<IrPackageTypeParameter>,
    pub receiver: Option<Ty>,
    pub context_parameters: Vec<Ty>,
    pub context_parameter_names: Vec<String>,
    pub is_const: bool,
    pub has_constant: bool,
    pub visibility: crate::types::Visibility,
    /// Resolved Kotlin annotation identities. Backends interpret annotations in their own
    /// namespace; common lowering does not turn them into storage or calling-convention choices.
    pub annotations: Box<[TypeName]>,
    /// Final Kotlin declaration modifiers copied from the stable header. Representation passes may
    /// inspect these semantic restrictions without reopening FIR or recovering a declaration by
    /// spelling.
    pub flags: crate::fir::DeclarationFlags,
    pub spellings: crate::spelling::DeclaredSpellings,
    pub has_backing_field: bool,
    pub has_declared_getter: bool,
    pub source_order: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleSource {
    pub source: crate::fir::SourceFileId,
    pub package: TypeName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrModuleCallable {
    pub source: IrModuleSource,
    /// Declaring classifier for a member `$default` bridge. Ordinary member calls are already
    /// virtual/special common-IR calls and therefore never use this record.
    pub owner: Option<TypeName>,
    /// Final source declaration flags needed after stable module calls cross into target realization.
    pub flags: crate::fir::DeclarationFlags,
    /// Final declaration signature, including context and extension receiver parameters but never
    /// a target-specific dispatch receiver, continuation, default mask, or marker. Backends use it
    /// for representation ABI without reopening FIR or reverse-engineering a synthetic descriptor.
    pub parameters: Box<[Ty]>,
    pub result: Ty,
    /// Resolved declaration annotations with only the compact constant-string payload needed by
    /// target realization. No source spelling, expression, or parser coordinate survives here.
    pub annotations: Box<[IrHeaderAnnotation]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrHeaderAnnotation {
    pub identity: TypeName,
    pub string_arguments: Box<[Box<str>]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrModuleProperty {
    pub source: IrModuleSource,
    pub name: String,
    pub ty: Ty,
    pub context_parameters: Vec<Ty>,
    pub extension_receiver: Option<Ty>,
    pub mutable: bool,
    pub owner: Option<TypeName>,
    /// Source-level kind of `owner`. Common IR retains the Kotlin declaration fact; a target backend
    /// decides whether that kind uses interface dispatch, singleton storage, or another physical form.
    pub owner_kind: Option<IrClassifierKind>,
    pub companion_associated: bool,
    /// Outer classifier whose companion object owns this declaration. This is the Kotlin
    /// singleton-association edge; it says nothing about target storage.
    pub companion_owner: Option<TypeName>,
    pub visibility: crate::types::Visibility,
    pub setter_is_private: bool,
    /// Resolved Kotlin annotation identities. A target backend may interpret annotations in its
    /// namespace; common lowering never turns one into a physical access kind.
    pub annotations: Box<[TypeName]>,
    pub flags: crate::fir::DeclarationFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrClassifierKind {
    Class,
    Interface,
    Annotation,
    Enum,
    Object,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleClassifier {
    pub singleton: bool,
    pub companion_owner: Option<TypeName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrAppliedClassifier {
    pub classifier: TypeName,
    pub applied: Ty,
    pub depth: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrPropertyOverride {
    pub implementation: crate::fir::ResolvedPropertyOverrideTarget,
    pub implementation_owner: TypeName,
    pub overridden: crate::fir::ResolvedPropertyOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: String,
    pub declared_type: Ty,
    pub applied_type: Ty,
    pub implementation_type: Ty,
    pub overridden_mutable: bool,
    pub implementation_mutable: bool,
    pub depth: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrFunctionOverride {
    pub implementation: crate::fir::ResolvedFunctionOverrideTarget,
    /// Exact common-IR implementation for a compiler-generated declaration such as an interface
    /// delegation forwarder. Source overrides use their stable callable identity and leave this
    /// empty; backends consume either edge without matching a method by name.
    pub implementation_function: Option<FunId>,
    pub implementation_owner: TypeName,
    pub overridden: crate::fir::ResolvedFunctionOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: String,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub applied_parameters: Vec<Ty>,
    pub applied_result: Ty,
    pub implementation_parameters: Vec<Ty>,
    pub implementation_result: Ty,
    pub suspend: bool,
    pub depth: u32,
}

impl IrFile {
    pub(crate) fn expr_diverges_by(
        &self,
        expression: ExprId,
        leaf: &impl Fn(ExprId, &IrExpr) -> bool,
    ) -> bool {
        match self.expr(expression) {
            IrExpr::Return(_)
            | IrExpr::Throw { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. } => true,
            // A lowering block may retain a syntactic value after a preceding statement has already
            // transferred control (`{ throw e; unreachableValue }`). Either the statement tail or
            // the value can therefore make the block divergent.
            IrExpr::Block { stmts, value } => {
                stmts
                    .last()
                    .is_some_and(|stmt| self.expr_diverges_by(*stmt, leaf))
                    || value.is_some_and(|value| self.expr_diverges_by(value, leaf))
            }
            // Checked coercions do not restore fallthrough around a divergent operand.
            IrExpr::TypeOp { arg, .. } => self.expr_diverges_by(*arg, leaf),
            // Mutation operations evaluate their operands before the write. If either evaluation
            // transfers control, the store itself cannot fall through and no backend may append
            // the physical write or a trailing return after it.
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.expr_diverges_by(*value, leaf)
            }
            IrExpr::SetField {
                receiver, value, ..
            } => self.expr_diverges_by(*receiver, leaf) || self.expr_diverges_by(*value, leaf),
            IrExpr::When { branches } => {
                branches.iter().any(|(condition, _)| condition.is_none())
                    && branches
                        .iter()
                        .all(|(_, body)| self.expr_diverges_by(*body, leaf))
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                finally.is_some_and(|finally| self.expr_diverges_by(finally, leaf))
                    || (self.expr_diverges_by(*body, leaf)
                        && catches
                            .iter()
                            .all(|catch| self.expr_diverges_by(catch.body, leaf)))
            }
            _ => leaf(expression, self.expr(expression)),
        }
    }

    pub fn with_package(package: Option<String>) -> Self {
        IrFile {
            package,
            ..Default::default()
        }
    }

    pub fn class_const(&mut self, internal: Option<&str>) -> ExprId {
        let internal = internal.map(crate::types::type_name);
        self.add_expr(IrExpr::ClassConst { internal })
    }

    pub fn external_static_field(
        &mut self,
        owner: &str,
        name: impl Into<String>,
        descriptor: impl Into<String>,
    ) -> ExprId {
        let owner = crate::types::type_name(owner);
        self.add_expr(IrExpr::ExternalStaticField {
            owner,
            name: name.into(),
            descriptor: descriptor.into(),
        })
    }

    pub fn external_static_instance(
        &mut self,
        owner: &str,
        ty: &str,
        field: impl Into<String>,
    ) -> ExprId {
        let owner = crate::types::type_name(owner);
        let ty = crate::types::type_name(ty);
        self.add_expr(IrExpr::ExternalStaticInstance {
            owner,
            ty,
            field: field.into(),
        })
    }

    pub fn new_external(
        &mut self,
        internal: &str,
        ctor_desc: impl Into<String>,
        args: Vec<ExprId>,
    ) -> ExprId {
        let internal = crate::types::type_name(internal);
        self.add_expr(IrExpr::New {
            internal,
            args,
            ctor_params: None,
            ctor_desc: Some(ctor_desc.into()),
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        })
    }

    pub fn new_cross_file(&mut self, internal: &str, params: Vec<Ty>, args: Vec<ExprId>) -> ExprId {
        let internal = crate::types::type_name(internal);
        self.add_expr(IrExpr::New {
            internal,
            args,
            ctor_params: Some(params),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        })
    }

    pub fn mark_synthetic_class(&mut self, internal: &str) {
        self.synthetic_classes
            .insert(crate::types::type_name(internal));
    }

    pub fn is_synthetic_class(&self, internal: &str) -> bool {
        self.synthetic_classes
            .contains(&crate::types::type_name(internal))
    }

    pub fn mark_deprecated_class(&mut self, internal: &str) {
        self.deprecated_classes
            .insert(crate::types::type_name(internal));
    }

    pub fn is_deprecated_class(&self, internal: &str) -> bool {
        self.deprecated_classes
            .contains(&crate::types::type_name(internal))
    }

    pub fn mark_value_param_ctor(&mut self, internal: &str) {
        self.mark_value_param_ctor_name(crate::types::type_name(internal));
    }

    pub fn mark_value_param_ctor_name(&mut self, internal: TypeName) {
        self.value_param_ctors.insert(internal);
    }

    pub fn has_value_param_ctor(&self, internal: &str) -> bool {
        self.value_param_ctors
            .contains(&crate::types::type_name(internal))
    }

    pub fn record_vc_ctor_declared_params(&mut self, internal: TypeName, declared: Vec<Ty>) {
        self.vc_ctor_declared_params.insert(internal, declared);
    }

    pub fn vc_ctor_declared_params(&self, internal: TypeName) -> Option<&[Ty]> {
        self.vc_ctor_declared_params
            .get(&internal)
            .map(Vec::as_slice)
    }

    pub fn insert_class_ctor_defaults(&mut self, internal: &str, defaults: Vec<Option<u32>>) {
        self.insert_class_ctor_defaults_name(crate::types::type_name(internal), defaults);
    }

    pub fn insert_class_ctor_defaults_name(
        &mut self,
        internal: TypeName,
        defaults: Vec<Option<u32>>,
    ) {
        self.class_ctor_defaults.insert(internal, defaults);
    }

    pub fn class_ctor_defaults(&self, internal: &str) -> Option<&Vec<Option<u32>>> {
        self.class_ctor_defaults_name(crate::types::type_name(internal))
    }

    pub fn class_ctor_defaults_name(&self, internal: TypeName) -> Option<&Vec<Option<u32>>> {
        self.class_ctor_defaults.get(&internal)
    }

    pub fn take_class_ctor_defaults_name(
        &mut self,
        internal: TypeName,
    ) -> Option<Vec<Option<u32>>> {
        self.class_ctor_defaults.remove(&internal)
    }

    pub fn insert_class_signature(&mut self, internal: &str, sig: IrGenericSig) {
        self.insert_class_signature_name(crate::types::type_name(internal), sig);
    }

    pub fn insert_class_signature_name(&mut self, internal: TypeName, sig: IrGenericSig) {
        self.class_signatures.insert(internal, sig);
    }

    pub fn class_signature(&self, internal: &str) -> Option<&IrGenericSig> {
        self.class_signatures
            .get(&crate::types::type_name(internal))
    }

    pub fn class_signature_name(&self, internal: crate::types::TypeName) -> Option<&IrGenericSig> {
        self.class_signatures.get(&internal)
    }

    pub fn insert_field_signatures(&mut self, internal: &str, sigs: Vec<(String, String)>) {
        self.field_signatures
            .insert(crate::types::type_name(internal), sigs);
    }

    pub fn field_signatures(&self, internal: &str) -> Option<&Vec<(String, String)>> {
        self.field_signatures
            .get(&crate::types::type_name(internal))
    }

    /// Whether the class's declared type parameter `name` admits `null` — an unbounded `<T>` (implicitly
    /// `Any?`) or one whose every declared upper bound is nullable. kotlinc treats a value typed by a
    /// NON-null-bounded parameter as an ordinary non-null reference, so its field, accessors and
    /// constructor parameter carry `@NotNull` and its parameters are null-checked.
    ///
    /// Reads the RESOLVED bounds recorded in the class's generic signature; `Ty::upper_bound_admits_null`
    /// walks a bound that is itself a parameter (`<A : Cargo, B : A>`). `true` when the class declares no
    /// generic signature — a non-generic class has no such parameter to ask about.
    pub fn class_type_param_admits_null(&self, internal: &str, name: &str) -> bool {
        self.class_signatures
            .get(&crate::types::type_name(internal))
            .and_then(|signature| {
                signature
                    .type_params
                    .iter()
                    .find(|parameter| parameter.name == name)
            })
            .is_none_or(|parameter| {
                !parameter
                    .bounds
                    .iter()
                    .any(|(bound, _)| !bound.upper_bound_admits_null())
            })
    }

    pub fn insert_external_value_class_name(&mut self, internal: TypeName, underlying: Ty) {
        self.external_value_classes.insert(internal, underlying);
    }

    pub fn external_value_class_name(&self, internal: TypeName) -> Option<&Ty> {
        self.external_value_classes.get(&internal)
    }

    pub fn has_external_value_class_name(&self, internal: TypeName) -> bool {
        self.external_value_class_name(internal).is_some()
    }

    /// Resolve a value class's erased underlying type without making callers branch on whether the
    /// declaration belongs to this source file or was recovered from dependency metadata.
    pub(crate) fn value_class_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.external_value_class_name(internal)
            .copied()
            .or_else(|| {
                self.classes
                    .iter()
                    .find(|class| class.is_value && class.fq_name == internal)
                    .and_then(|class| class.fields.first().map(|field| field.ty))
            })
    }

    /// Preserve the source meaning of a value-class construction after the JVM pass replaces its
    /// generic `New` node with a target helper call. The safety gate uses this pass-produced fact instead
    /// of trusting generated method names, which are neither semantic identities nor reserved names.
    pub(crate) fn record_erased_value_construction(
        &mut self,
        expression: ExprId,
        owner: TypeName,
        underlying: Ty,
    ) {
        self.erased_value_constructions
            .insert(expression, (owner, underlying));
    }

    /// The in-IR (same-file) `ClassId` for `internal`, or `None` when the name is an external/other-module
    /// class not compiled in this file. The bridge from the unified [`IrExpr::New`]'s owner name back to a
    /// `ClassId` for consumers that need the in-IR class (emit, function/property-reference detection).
    pub fn class_id_by_name(&self, internal: TypeName) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|c| c.fq_name == internal)
            .map(|i| i as ClassId)
    }

    /// Whether `internal` names a value class — an in-IR (same-file) one OR an external/other-module one.
    /// The single name-keyed value-class test for the unified [`IrExpr::New`] (which no longer carries a
    /// same-file `ClassId` to branch on).
    pub fn is_value_class_name(&self, internal: TypeName) -> bool {
        self.classes
            .iter()
            .any(|c| c.is_value && c.fq_name == internal)
            || self.has_external_value_class_name(internal)
    }

    /// Record/query the JVM-only companion backing-storage realization selected after common
    /// lowering. Keeping this in a backend-populated side table prevents a physical JVM layout bit
    /// from becoming part of an ordinary common-IR static declaration.
    pub(crate) fn mark_jvm_companion_hoisted_static(&mut self, index: u32) {
        self.jvm_companion_hoisted_statics.insert(index);
    }

    pub(crate) fn mark_jvm_companion_property_static(
        &mut self,
        companion: TypeName,
        property: u32,
        index: u32,
    ) {
        self.jvm_companion_property_statics
            .insert((companion, property), index);
    }

    pub(crate) fn jvm_companion_property_static(
        &self,
        companion: TypeName,
        property: u32,
    ) -> Option<u32> {
        self.jvm_companion_property_statics
            .get(&(companion, property))
            .copied()
    }

    pub(crate) fn is_jvm_companion_hoisted_static(&self, index: u32) -> bool {
        self.jvm_companion_hoisted_statics.contains(&index)
    }

    /// Record/query the `@JvmField` realization of a hoisted companion property: the static IS the
    /// property's public JVM surface — no accessors, no `access$…$cp` bridges — so every reader and
    /// writer goes `getstatic`/`putstatic` on the owner directly (kotlinc's shape).
    pub(crate) fn mark_jvm_field_static(&mut self, index: u32) {
        self.jvm_field_statics.insert(index);
    }

    pub(crate) fn is_jvm_field_static(&self, index: u32) -> bool {
        self.jvm_field_statics.contains(&index)
    }

    pub fn param_defaults(&self, fid: u32) -> Option<&Vec<Option<ExprId>>> {
        self.fn_params.get(&fid)?.defaults.as_ref()
    }
    pub fn has_param_defaults(&self, fid: u32) -> bool {
        self.param_defaults(fid).is_some()
    }
    /// Whether `fid`'s registered defaults are STUB-ONLY (see [`FnParamInfo::stub_only`]): call-site
    /// routing that would evaluate or delegate to them outside the `$default` stub must decline.
    pub fn param_defaults_stub_only(&self, fid: u32) -> bool {
        self.fn_params.get(&fid).is_some_and(|info| info.stub_only)
    }
    pub fn param_names(&self, fid: u32) -> Option<&[String]> {
        Some(&self.fn_params.get(&fid)?.names)
    }
    pub fn expr(&self, id: ExprId) -> &IrExpr {
        &self.exprs[id as usize]
    }
    pub fn add_expr(&mut self, mut e: IrExpr) -> ExprId {
        let id = self.exprs.len() as u32;
        match &mut e {
            IrExpr::PropertyRead { operation, .. } | IrExpr::PropertyWrite { operation, .. } => {
                // Preserve an identity already assigned to a moved/cloned property operation. Fresh
                // lowering constructors pass `None` and receive their original arena index here.
                operation.get_or_insert(id);
            }
            _ => {}
        }
        self.exprs.push(e);
        id
    }
    pub fn add_fun(&mut self, f: IrFunction) -> FunId {
        let id = self.functions.len() as u32;
        self.functions.push(f);
        id
    }
    pub fn add_class(&mut self, c: IrClass) -> ClassId {
        let id = self.classes.len() as u32;
        self.classes.push(c);
        id
    }
}

mod traversal;
pub use traversal::*;
mod clone;
pub use clone::*;
mod semantic_validation;
pub use semantic_validation::UndeterminedIrType;

#[cfg(test)]
mod tests;
