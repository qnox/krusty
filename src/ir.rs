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

use crate::kt_string::KtString;
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

mod annotations;
mod bindings;
mod bottom_values;
mod bridges;
mod callee;
mod cast_targets;
mod catches;
mod companion_blocks;
mod constants;
mod constructors;
mod default_arguments;
mod expression_provenance;
mod field_flags;
mod fields;
mod function_scope;
mod inline_copies;
mod suspension_points;
pub use suspension_points::{IrIntrinsicSuspensionPoint, IrValueClassSuspendResult};
mod intrinsic;
mod jvm_static_realization;
mod lambda_classes;
mod lifting_sequences;
mod local_class_names;
mod local_delegates;
pub use local_delegates::IrLocalDelegateAccess;
pub(crate) use local_delegates::{
    IrInlineDeclaration, IrLocalDelegateAccessorPlan, IrLocalDelegatePlan,
};
mod local_property_references;
mod module_records;
mod null_checks;
mod operators;
mod overrides;
mod package_declarations;
mod progression;
mod properties;
mod property_layouts;
pub(crate) mod referenced_classifiers;
mod references;
mod sam_target;
mod static_properties;
mod type_check_role;
pub(crate) mod type_reflection;
mod value_class_constructors;
mod value_class_facts;
mod when_facts;

pub use crate::enclosing_declarations::EnclosingDeclaration;
pub use crate::types::EqualityMode;
pub use annotations::{
    AnnoRetention, AnnoValue, AppliedAnnotation, DeclarationAnnotations, FieldAnnotations,
    PropertyAnnotations, RetainedAnnotation,
};
pub use bindings::IrBindingStability;
pub(crate) use bottom_values::complete_bottom_value;
pub use bottom_values::IrBottomValueCompletion;
pub use bridges::{
    Bridge, BridgeAccessorRole, BridgeKind, BridgeParameter, BridgePropertyImplementation,
    CollectionBarrierPlan,
};
pub use callee::{Callee, IrSuperCallKind, IrVirtualTarget};
pub use catches::IrCatch;
pub use companion_blocks::{IrCompanionBlockProperty, IrCompanionBlocks, IrStaticPlacement};
pub use constants::IrConst;
pub(crate) use constructors::IrSecondaryConstructorRole;
pub use constructors::{
    IrCapturedReceiver, IrConstructorCapture, IrCtorParameterProvenance,
    IrJvmValueClassSecondaryCtor,
};
pub use constructors::{IrConstructorAccess, IrConstructorTarget, IrCustomSerializerConstruction};
pub use constructors::{IrSecondaryCtor, IrSecondaryCtorLines};
pub use expression_provenance::{EnumValueOfDeclaration, IrShortCircuitKind};
pub use field_flags::IrfFlags;
pub use fields::IrField;
pub use function_scope::IrFunctionScope;
pub use intrinsic::IrIntrinsic;
pub use lambda_classes::{IrInvokeBridge, IrLambdaCapture, IrLambdaClass, IrSamWrapperClass};
pub(crate) use lifting_sequences::{IrLiftingEntry, IrLiftingRoot, IrLiftingSequence};
pub(crate) use local_class_names::{IrLocalClassNameProvenance, IrLocalClassOwner};
pub use local_property_references::IrLocalPropertyReference;
pub use module_records::{
    IrCallableTypeParameter, IrClassifierKind, IrHeaderAnnotation, IrModuleCallable,
    IrModuleClassifier, IrModuleMemberAccess, IrModuleSource,
};
pub use null_checks::NullCheck;
pub use operators::{IrBinOp, IrTypeOp};
pub use overrides::{is_kotlin_primitive, IrFunctionOverride, IrPropertyOverride};
pub use package_declarations::{
    IrEntryPoint, IrPackageFunction, IrPackageProperty, IrPackageTypeParameter, MainEntryParameters,
};
pub use progression::{IrProgressionSource, IrRuntimeFunction};
pub use properties::{
    IrCheckedProperty, IrInlinePropertySplice, IrInlineTypeSubstitution, IrMemberKind,
    IrModuleProperty, IrProperty, IrPropertyModality, IrPropertyModifiers, MemberExtProp,
};
pub use property_layouts::IrLocalPropertyLayout;
pub use references::{
    FuncRef, IrCallableReference, IrCallableReferenceTarget, IrClassifierCallable, PropRef,
    ReflectedCallable,
};
pub use sam_target::{IrSamMethod, IrSamTarget};
pub use static_properties::{IrStatic, IrStaticAccessor, IrStaticAccessors};
pub use type_check_role::TypeCheckRole;
pub use type_reflection::IrGenericTopLevelProperty;
use type_reflection::TypeReflectionFacts;

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

/// One supplied argument edge whose declaration parameter mentions a type parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrDeclarationArgumentBoundary {
    pub argument: ExprId,
    pub parameter: u32,
    pub declaration: crate::types::Ty,
    pub retarget_coercion: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrCheckedSubstitution {
    pub parameter: crate::fir::FirTypeParameterRef,
    /// Exact declaration capability published by checked FIR; ordinary generic substitutions are
    /// not runtime reification operations.
    pub reified: bool,
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
    /// `step`'s failure on a step that is not positive: an `IllegalArgumentException` whose
    /// message is `Step must be positive, was: $step.`.
    IllegalProgressionStep { step: ExprId },
    RangeLoop {
        variable: u32,
        /// Source identity of the loop variable. A representation backend attaches it to the
        /// declaration it creates; it must not recover the name from a slot or generated spelling.
        variable_name: Option<Box<str>>,
        counter: Ty,
        source: IrProgressionSource,
        /// The selected comparison ordering an unsigned counter; `None` for any other counter.
        unsigned_compare: Option<IrRuntimeFunction>,
        body: ExprId,
        label: String,
        /// The index a destructured `withIndex()` loop over the progression counts beside it.
        with_index: Option<IrLoopIndex>,
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

/// Checked semantic shape of an annotation constructor call. The common IR retains the annotation
/// interface and lexical scope; a backend chooses the concrete runtime implementation and name.
#[derive(Clone, Debug)]
pub struct IrAnnotationConstruction {
    pub interface: TypeName,
    pub members: Vec<(String, Ty)>,
    /// Constructor defaults as lowered declarations, not evaluated call operands.
    pub defaults: Vec<Option<ExprId>>,
    /// The lexical scopes this construction is evaluated in; `None` is the file facade. A written
    /// construction has its own one. A construction inside an annotation declaration's default is
    /// evaluated at every construction that omits that element, so it has theirs, and none when no
    /// construction in this source uses that default.
    pub scopes: Vec<Option<TypeName>>,
}

/// kotlinc's `WithIndexLoopHeader` over a counted loop: each iteration binds the index, steps the
/// index when the loop counts its own, then binds the element.
#[derive(Clone, Debug, PartialEq)]
pub struct IrLoopIndex {
    /// The index's `var index = 0` declaration. A backend whose counter starts at constant `0` and
    /// steps by `1` uses this slot as that counter instead of declaring a second one.
    pub declaration: ExprId,
    /// A block of the declarations that read the index, opening each iteration.
    pub bindings: ExprId,
    /// Whether the source binds the element. A loop that does not binds no loop variable.
    pub element_bound: bool,
    /// A block of the declarations that copy the bound element.
    pub element_copies: ExprId,
}

/// An IR expression node (a subset of Kotlin IR's `IrExpression` hierarchy). Operands reference
/// other expressions by `ExprId` into the arena.
#[derive(Clone, Debug)]
pub enum IrExpr {
    Const(IrConst),
    /// A checked expression whose semantic type is the non-null bottom type, while its producer has
    /// a target-level value-returning path. Backends consume `completion` after emitting `producer`;
    /// they must not reconstruct bottom semantics from the producer's syntax or logical-type tables.
    BottomValue {
        producer: ExprId,
        completion: IrBottomValueCompletion,
    },
    /// A frontend-selected semantic operation. Backend realization consumes the stable declaration
    /// identity; it must not repeat lookup, overload selection, or argument mapping.
    Checked(IrCheckedOperation),
    /// Checked Kotlin callable-reference identity plus a fully lowered common invocation adapter.
    /// Unlike [`IrExpr::Lambda`], this preserves declaration-based equality and reflection without
    /// selecting a platform carrier in common lowering.
    CallableReference(IrCallableReference),
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
        /// The literal names a type parameter (`T::class`). Inlining may substitute a primitive
        /// for it, which still denotes the boxed class token rather than the primitive one.
        type_argument: bool,
    },
    /// Backend-neutral reflection value passed to local delegated-property conventions.
    LocalPropertyReference(IrLocalPropertyReference),
    /// One checked local delegated-property access. Its selected call template and source lifting
    /// provenance live in `IrFile::local_delegate_plans`; a target chooses the physical helper.
    LocalDelegateAccess(IrLocalDelegateAccess),
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
        /// Resolved semantic types the plugin needs. Kept separately from names so generic
        /// arguments and nullability remain semantic data across the plugin boundary.
        types: Vec<Ty>,
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
    /// Source `==`/`!=` whose mode was fixed by checking. Inlining copies `mode` unchanged; a
    /// backend realizes it and does not choose IEEE versus structural equality from the operand
    /// storage types substitution may have produced.
    Equality {
        op: IrBinOp,
        mode: EqualityMode,
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
        /// Which of the two declarations that spell this lookup the checker selected. They are not
        /// interchangeable: one is the classifier's own member, the other the standard library's
        /// INLINE top-level function, and a consumer that records source positions attributes an
        /// inline expansion to its call site rather than to a dispatch.
        declaration: EnumValueOfDeclaration,
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
    /// of `suspendCoroutineUninterceptedOrReturn { c -> … }`. Common lowering emits this placeholder;
    /// the CPS pass rewrites it to the real continuation value once the trailing `Continuation`
    /// parameter exists. It must never survive to the emitter.
    CurrentContinuation,
    /// A debugger frame boundary opened by an inline expansion. It is not a value: no code reads
    /// it, and it allocates no common-IR local. The callee is recorded in `value_names` and the
    /// role in `debug_local_provenance`. A backend that emits debug locals materializes the slot,
    /// the zero store, and the name.
    InlineFrameMarker,
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
    /// A not-null assertion: yields `operand`, throwing if it is null. `check` says whether the
    /// source wrote it (`operand!!`) or it guards a Java value committed to a declared non-null
    /// type. Every pass treats them alike; only the emitted intrinsic and the `-X` option that
    /// removes the implicit ones differ.
    NotNullAssert {
        operand: ExprId,
        check: NullCheck,
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
    /// new `Ref$IntRef`/`Ref$ObjectRef`/… whose `element` field is initialized to `init`, or keeps
    /// the field's default for a declaration without an initializer. `elem` is
    /// the boxed value's type (selects the `Ref` subclass + the `element` field descriptor). Evaluates
    /// to the holder, so it's the initializer of the local that holds the box.
    RefNew {
        elem: Ty,
        init: Option<ExprId>,
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
    /// Anonymous super-constructor operand evaluated at the construction site. Constructor
    /// finalization binds `slot` to the synthetic constructor parameter. Emission does not
    /// interpret this node.
    ForwardedSuperArgument {
        slot: u32,
    },
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
    pub param_checks: Vec<Option<IrParameterCheck>>,
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
    /// Semantic parameter list of the constructor selected for this entry. Enum entries may target
    /// secondary constructors whose parameters differ from the primary constructor; a backend
    /// prepends its own enum ABI operands to this already-resolved call shape.
    pub constructor_parameter_types: Vec<Ty>,
    /// Selected constructor parameters whose values come from declaration defaults. This is the
    /// frontend's final argument-mapping decision; a backend only realizes its default-argument ABI.
    pub default_parameters: Vec<u32>,
    /// `Some(subclass_internal)` when the entry has a body and is constructed as an instance of a synthesized
    /// anonymous subclass (`new Enum$ENTRY(name, ordinal, args)`); `None` when constructed as the enum
    /// itself.
    /// 1-based source line of the entry's declaration, for the `<clinit>` `LineNumberTable`.
    pub decl_line: u32,
    /// The entry declaration's stable source position, in the key space of the class's other
    /// members, so metadata can visit it in declaration order.
    pub source_order: u32,
    pub subclass: Option<TypeName>,
}

/// One primary-constructor parameter of an [`IrClass`], in declaration order: its type, storage,
/// null check and capture travel together so they cannot desync.
#[derive(Clone, Debug)]
pub struct IrCtorArg {
    /// Source parameter name. Synthetic constructor parameters have no name.
    pub name: Option<String>,
    /// Exact source role for a classifier context parameter. `None` means an ordinary or
    /// compiler-generated constructor argument; targets must not infer this from `name`.
    pub context_kind: crate::types::ContextParameterKind,
    /// The parameter type (carries declared nullability — a nullable value-class param erases like its
    /// field).
    pub ty: Ty,
    /// Declared source-level type before storage/JVM erasure (`None` for synthetic parameters), so
    /// metadata keeps `KClass<T>` rather than the erased `KClass<Any>` the constructor stores.
    pub declared_ty: Option<Ty>,
    /// `true` ⇒ stored to a field (`field_index`); `false` ⇒ a plain parameter, only a local in
    /// `<init>` for property initializers and `init` blocks.
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
    /// `Some(name)` for a non-null reference param the backend guards at `<init>` entry; `None` for a
    /// primitive, nullable or type-parameter param and for synthetic ones (`this$0`, captures).
    pub check: Option<String>,
    /// 1-based ordinal among an anonymous object's forwarded super-constructor parameters.
    /// `None` for every source parameter and for every other synthetic parameter. Kotlin metadata
    /// does not list this parameter; the JVM local-variable table names it `$super_call_param$N`.
    pub anonymous_super_forward: Option<u32>,
    /// The captured value a local or anonymous class's synthetic constructor prefix carries.
    pub capture: Option<IrConstructorCapture>,
    /// Why this parameter exists. An enclosing instance is selected by
    /// [`IrCtorParameterProvenance::EnclosingInstance`], not by the lack of a name.
    pub provenance: IrCtorParameterProvenance,
    /// Semantic closure coordinate of a capture parameter. Absent for source parameters and for
    /// the enclosing instance, whose role is [`IrCtorParameterProvenance::EnclosingInstance`].
    pub(crate) capture_identity: Option<crate::fir::ClassCaptureIdentity>,
}

/// The executable scope a local, anonymous or generated class is declared in. A backend realizes it
/// as its own enclosure record (the JVM's `EnclosingMethod`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrEnclosure {
    /// A function body, or a local function's own function.
    Function(FunId),
    /// A suspend lambda's body. Whether the lambda is a scope of its own is the backend's: one it
    /// realizes as a class of its own encloses what the body declares, and otherwise the body's
    /// declarations belong to the scope the lambda is written in (see
    /// [`IrFile::lambda_enclosures`]).
    Lambda(FunId),
    /// A source property accessor. The common IR keeps the property identity and accessor role;
    /// each backend resolves that pair through its finalized property realization.
    PropertyAccessor {
        property: crate::fir::PropertyId,
        setter: bool,
    },
    /// A source constructor. `ordinal == 0` is the primary declaration and later ordinals are
    /// secondary declarations in source order. Its physical descriptor remains backend-owned.
    Constructor { class: ClassId, ordinal: u32 },
    /// A top-level property initializer: the file itself.
    File,
    /// A classifier's property initializer or `init` block.
    ClassInitializer(ClassId),
    /// A classifier as a whole rather than any of its code: a class a backend generates for the
    /// classifier's own use, such as an annotation implementation shared by all its instantiations.
    Classifier(ClassId),
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
    /// A source anonymous-object declaration.
    pub is_anonymous_object: bool,
    /// The executable scope a local, anonymous or generated class is declared in, recorded as an
    /// exact identity so a backend realizes enclosure metadata without parsing generated names.
    pub enclosure: Option<IrEnclosure>,
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
    /// 1-based source line where the declaration starts, including annotations (0 = unknown).
    /// kotlinc maps the primary constructor's `super()` call here and its trailing `return` to
    /// [`Self::decl_line`]. These differ when an annotation precedes the class header.
    pub decl_start_line: u32,
    /// 1-based source line where the source declaration ends (0 = unknown). For a class body this
    /// is its closing `}`. Backends use this stable source fact for synthesized fall-through code;
    /// they must not recover it from source text.
    pub decl_end_line: u32,
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
    /// classes, the values a local class or anonymous object captures, and generated state machines
    /// each require such a store.
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
    /// The checker-selected superclass constructor. Backends consume its declaration facts instead
    /// of comparing erased descriptors with the primary shape.
    pub super_ctor: IrConstructorTarget,
    /// Whether this is an `enum class`, including one that declares no entries.
    pub is_enum: bool,
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
    /// Set when this class is a lambda realized as a class of its own (a target that cannot build
    /// the lambda at run time); see [`IrLambdaClass`].
    pub lambda: Option<IrLambdaClass>,
    /// Set when this class wraps function values converted to a fun interface; see
    /// [`IrSamWrapperClass`].
    pub sam_wrapper: Option<IrSamWrapperClass>,
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
    /// Kotlin names of compiler-generated classifiers this class publishes as direct nested
    /// declarations, in declaration order. Source-declared nested classifiers are represented by
    /// their own [`IrClass`] ownership; this list is only for generated declarations whose producer
    /// explicitly owns the language-level publication contract (for example `$serializer`).
    pub published_nested_classifiers: Vec<String>,
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
    /// none. The marker pass consumes every entry that retains an annotation; an entry left after
    /// it records only erased optional expectations, which reach `@Metadata` as a flag alone.
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
}

impl IrClass {
    /// Minimal backend-neutral shape for a compiler-generated class. The producer sets only the
    /// semantic payload it owns (for example `prop_ref`); target passes choose representation.
    pub fn synthetic(fq_name: TypeName) -> Self {
        Self {
            fq_name,
            is_source_declared: false,
            is_anonymous_object: false,
            enclosure: None,
            is_inner_class: false,
            is_local_class: false,
            is_value: false,
            is_data: false,
            decl_line: 0,
            decl_start_line: 0,
            decl_end_line: 0,
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
            super_ctor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            is_enum: false,
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            lambda: None,
            sam_wrapper: None,
            bridges: Vec::new(),
            interfaces: Default::default(),
            is_object: false,
            is_companion: false,
            companion_class: None,
            published_nested_classifiers: Vec::new(),
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
                name: parameter.name.as_deref().map(str::to_owned),
                context_kind: parameter.kind,
                ty: crate::types::stored_value_ty(parameter.ty.get()),
                declared_ty: Some(parameter.ty.get()),
                is_field: true,
                field_index: Some(u32::try_from(field).expect("too many context fields")),
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: IrCtorParameterProvenance::Value,
                capture_identity: None,
            })
            .collect::<Vec<_>>();
        let context_count =
            u32::try_from(context_arguments.len()).expect("too many classifier context parameters");
        Self {
            fq_name: header.classifier,
            is_source_declared: true,
            is_anonymous_object: flags.has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT),
            enclosure: None,
            is_inner_class: flags.has(crate::fir::DeclarationFlags::INNER),
            is_local_class: flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS),
            is_value: flags.has(crate::fir::DeclarationFlags::VALUE),
            is_data: flags.has(crate::fir::DeclarationFlags::DATA),
            decl_line: 0,
            decl_start_line: 0,
            decl_end_line: 0,
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
            super_ctor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            is_enum: flags.has(crate::fir::DeclarationFlags::ENUM),
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            lambda: None,
            sam_wrapper: None,
            bridges: Vec::new(),
            interfaces,
            is_object: flags.has(crate::fir::DeclarationFlags::SINGLETON),
            is_companion: flags.has(crate::fir::DeclarationFlags::COMPANION),
            companion_class: None,
            published_nested_classifiers: Vec::new(),
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
        self.superclass != crate::types::TypeName::ROOT
            && self.superclass != crate::types::wk::any()
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

/// The delegation target of a secondary constructor.
#[derive(Clone, Debug)]
pub enum CtorDelegateTarget {
    /// `this(args)` → `invokespecial` an own `<init>(target_params)` (the primary, or a sibling
    /// secondary). The class init body runs in the reached constructor, not here.
    This {
        target_params: Vec<Ty>,
        target: IrConstructorTarget,
        default_masks: Vec<i32>,
    },
    /// `super(args)` (or implicit) → the exact checker-selected superclass constructor.
    Super {
        owner: TypeName,
        target_params: Vec<Ty>,
        target: IrConstructorTarget,
        default_masks: Vec<i32>,
    },
    /// An enum secondary constructor with no written `this(…)` delegation. Kotlin implicitly
    /// initializes the language enum base with the compiler-supplied entry name and ordinal; a
    /// target backend chooses that base and its physical constructor prefix.
    ImplicitEnumBase,
}

/// A JVM call bound to one synthesized property accessor.
#[derive(Clone, Copy, Debug)]
pub struct SynthesizedAccessorCall {
    pub class: u32,
    pub property: u32,
}

/// Which JVM class a value-class type operation names.
///
/// An unsigned array's box (`kotlin/UIntArray`) and its carrier (`int[]`) share one descriptor.
/// Realization records which of the two this exact type operation names; emission reads the record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrValueClassTypeRole {
    /// The value class itself (`kotlin/UIntArray`).
    Box,
    /// The declared carrier (`[I`).
    Carrier,
}

/// Box owner, carrier, and the role of one type operation. The two classfile names can collide
/// as descriptors, so the role is part of the record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrValueClassTypeOperation {
    pub boxed_owner: TypeName,
    pub carrier: Ty,
    pub role: IrValueClassTypeRole,
}

/// A call-site copy of a lambda implementation.
///
/// The record keeps the checked declaration identities and lexical containment that caused the
/// copy. Source spellings are retained only as rendering provenance. A backend decides whether the
/// copy needs a separate artifact and owns every physical name and ordinal used for that artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSpecializedFunction {
    pub source: FunId,
    /// Stable declaration whose body contains the expansion.
    pub caller_declaration: crate::fir::DeclarationId,
    /// Exact executable scope containing the expansion. `None` means no declaration identity was
    /// available for the fragment.
    pub caller: Option<IrEnclosure>,
    /// The expansion is evaluated in a callable's default-argument fragment. The caller still
    /// records that callable or constructor identity; a target owns the physical `$default` or
    /// constructor-special rendering.
    pub caller_is_default: bool,
    /// Source spelling of that declaration, or the deliberately empty spelling of a constructor or
    /// unnamed initializer. This is rendering provenance, never the expansion identity.
    pub caller_source_name: String,
    /// Stable checked identity of the same-module inline callable that was expanded.
    pub inline_callee: crate::fir::CallableId,
    /// Source spelling retained for target-specific debug/artifact rendering.
    pub inline_callee_source_name: String,
    /// Specialized implementation lexically containing this one. Nested copies retain containment
    /// rather than being flattened into another top-level expansion ordinal.
    pub parent: Option<FunId>,
}

/// A call-site copy of an anonymous class whose members use a reified type parameter.
///
/// The declaration class stays in place for the inline method's own body. This record is the
/// expansion that copied it. A backend owns the physical `{owner}${caller}$$inlined${callee}$N`
/// name and shares that ordinal sequence with a specialized lambda of the same expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSpecializedAnonymousClass {
    pub source: ClassId,
    /// Expression identity of the copied construction, so the copy sorts with a specialized lambda
    /// of the same expansion.
    pub order: u32,
    pub caller_declaration: crate::fir::DeclarationId,
    pub caller: Option<IrEnclosure>,
    pub caller_is_default: bool,
    pub caller_source_name: String,
    pub inline_callee: crate::fir::CallableId,
    pub inline_callee_source_name: String,
    /// Source function → the function on this copy. Accessor functions materialized after the
    /// copy are added here before bridges and metadata read the copy.
    pub method_clones: std::collections::HashMap<FunId, FunId>,
    /// Call-site substitutions. Property accessors are materialized after this copy, and their
    /// bodies are specialized with these bindings.
    pub bindings: std::collections::HashMap<String, Ty>,
    pub reified_bindings: std::collections::HashMap<String, Ty>,
    /// Fields and properties already on the declaration class when this copy was taken.
    /// Later materialization appends backing fields and accessors past these prefixes.
    pub field_count: u32,
    pub property_count: u32,
    /// Property reads and writes in the inlined caller whose receiver is this copy's construction.
    /// Accessor functions are not available when the construction is retargeted, so the read is
    /// rebound once the copy's properties exist.
    pub caller_property_uses: Vec<ExprId>,
    /// Cloned property initializer and accessor roots. Accessor functions do not exist yet, and
    /// these roots are not constructor statements. Nested specialization walks this field;
    /// [`IrClass::init_body`] stays the constructor body.
    pub pending_property_roots: Vec<ExprId>,
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
    /// Target-neutral selected-call templates for local delegated properties. These are semantic
    /// plans, not function declarations: a backend may realize them without repeating resolution.
    pub(crate) local_delegate_plans: Vec<IrLocalDelegatePlan>,
    /// Stable declaration identity to the file-wide plan arena. Nested callables access the
    /// declaring property's plan without copying it or manufacturing another helper.
    pub(crate) local_delegate_plan_ids:
        std::collections::HashMap<crate::fir::LocalDelegatedPropertyId, u32>,
    /// JVM `$suspendImpl` body carriers (an interface member's, or an overridable class member's),
    /// keyed by carrier function id, with the exact owner and source-declaration function id. The JVM signature/default-stub boundaries consume
    /// these identities; they must not recover either one from the generated `$suspendImpl`
    /// spelling.
    pub(crate) jvm_suspend_impl_bodies: std::collections::HashMap<FunId, (TypeName, FunId)>,
    /// Exact generated function metadata/debug contracts, keyed by semantic owning classifier.
    /// Producers publish once; backends consume function identities without name/descriptor scans.
    generated_member_publications:
        std::collections::HashMap<TypeName, IrGeneratedMemberPublication>,
    /// Exact common-IR functions bound to compiler-synthesized data-class roles. The common
    /// producer records these before any backend rename; metadata and target realization consume
    /// the identities without scanning method spellings.
    synthesized_data_class_members:
        std::collections::HashMap<(TypeName, IrDataClassMemberRole), FunId>,
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
    /// Exact interface identities reached through each source classifier's direct superclass.
    /// Common resolution records the path distinction before flattening the complete hierarchy;
    /// target emitters consume it without reopening a classifier provider.
    pub superclass_interfaces: std::collections::HashMap<TypeName, Vec<TypeName>>,
    /// The frontend-selected custom serializer construction of each source classifier whose
    /// `@Serializable(with = …)` names a serializer class, keyed by that classifier.
    pub custom_serializer_constructions:
        std::collections::HashMap<TypeName, IrCustomSerializerConstruction>,
    /// Exact semantic property-override edges copied from stable FIR. A target backend may erase
    /// these types and materialize representation bridges, but it must not search declarations.
    pub property_overrides: std::collections::HashMap<TypeName, Vec<IrPropertyOverride>>,
    pub function_overrides: std::collections::HashMap<TypeName, Vec<IrFunctionOverride>>,
    /// Stable checked-FIR classifier declaration to its common-IR class realization. Member bodies
    /// attach through this edge; neither the sink nor a backend searches by rendered class name.
    pub checked_classifier_classes: std::collections::HashMap<crate::fir::DeclarationId, ClassId>,
    /// Backend-neutral lexical naming context for source classifiers declared in executable code.
    /// A target consumes this exact class-id ownership graph and chooses physical spellings.
    pub(crate) local_class_name_provenance:
        std::collections::HashMap<ClassId, IrLocalClassNameProvenance>,
    /// The same naming context for each source callable-reference node and each lambda node, by
    /// expression id. A target that realizes one as a class of its own names that class
    /// from it.
    pub(crate) callable_reference_provenance:
        std::collections::HashMap<u32, IrLocalClassNameProvenance>,
    /// The executable scope each source callable-reference and lambda node is written in,
    /// by expression id. A target that realizes one as a class of its own records that class's
    /// enclosure.
    pub(crate) callable_reference_enclosures: std::collections::HashMap<u32, IrEnclosure>,
    /// The scope each [`IrEnclosure::Lambda`] lambda is written in, by the lambda's function.
    pub(crate) lambda_enclosures: std::collections::HashMap<FunId, IrEnclosure>,
    /// Every lifting site the file's bodies declare (see [`crate::fir::FirLiftingSite`]), by
    /// sequence and source position: the step's source name, and whether it is lifted at all.
    pub(crate) lifting_sequences: std::collections::HashMap<
        IrLiftingSequence,
        std::collections::BTreeMap<u32, IrLiftingEntry>,
    >,
    /// Earliest declaring-member order for each logical lifting sequence. Overloads with the same
    /// source name share one sequence (and therefore one `$lambda$N` counter), while this separate
    /// fact lets a target restore declaration order when it realizes a helper later.
    pub(crate) lifting_sequence_source_order: std::collections::HashMap<IrLiftingSequence, u32>,
    /// The sequence and lifting site of each function lowered from a lambda or local function.
    pub(crate) lifted_functions:
        std::collections::HashMap<FunId, (IrLiftingSequence, crate::fir::FirLiftingSite)>,
    /// kotlinc's lifted name of each function in [`Self::lifted_functions`] whose enclosing
    /// callables are all lifted, once a target has numbered the sequences.
    pub(crate) lifted_names: std::collections::HashMap<FunId, String>,
    /// The class name a target chose for each source callable reference and lambda, by
    /// expression id.
    pub(crate) callable_reference_names: std::collections::HashMap<u32, TypeName>,
    /// The class a JVM naming pass realized for each lambda's inline-depth marker, by
    /// implementation. Only that pass writes it, from [`IrLambdaOrigin::class_provenance`];
    /// common lowering never stores a target spelling here.
    pub(crate) lambda_class_names: std::collections::HashMap<FunId, TypeName>,
    /// The declaration path a target sorts each class it named from provenance by, keyed by that
    /// name: kotlinc's `fqNameWhenAvailable`.
    pub(crate) declaration_paths: std::collections::HashMap<TypeName, String>,
    /// Qualified Kotlin source name for each source-declared class, keyed by its exact IR identity.
    /// This is an external-name boundary fact for metadata/plugins (for example a serialization wire
    /// name), not classifier identity. Keeping it on `ClassId` avoids guessing lexical nesting from
    /// a JVM `$` spelling, where a backticked `$` is indistinguishable from a physical separator.
    class_source_qualified_names: std::collections::HashMap<ClassId, KtString>,
    /// Exact stable-FIR declaration order for every source-declared classifier. Class metadata
    /// consumes this semantic order directly; a backend must not reconstruct it from debug lines,
    /// arena layout, or classifier spelling.
    class_source_orders: std::collections::HashMap<ClassId, u32>,
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
    /// A superclass-constructor parameter that forwards a mutable capture's shared holder, keyed by
    /// `(subclass, selected-super-parameter)`. `Ty` is the logical captured element type. This exact
    /// edge survives independently of the argument expression and of the selected constructor's
    /// semantic parameter type, so a backend can realize its holder ABI without reselecting a
    /// constructor or inferring capture storage from an erased descriptor.
    pub shared_super_capture_parameters: std::collections::HashMap<(ClassId, u32), Ty>,
    /// The same holder edge for a secondary `super(…)` that lowering prefixed with superclass
    /// captures. Keyed by `(subclass, secondary index, selected-super-parameter)`.
    pub(crate) shared_secondary_super_capture_parameters:
        std::collections::HashMap<(ClassId, u32, u32), Ty>,
    /// Exact semantic closure identity for every local/anonymous-class capture field. Transitive
    /// superclass forwarding consumes this coordinate instead of matching synthetic field names.
    pub(crate) class_capture_identities:
        std::collections::HashMap<(ClassId, u32), crate::fir::ClassCaptureIdentity>,
    /// Anonymous-object super-constructor arguments evaluated at the construction site.
    /// Keyed by `(anonymous classifier, super-parameter slot before an outer-instance prefix)`.
    /// The value is `(anonymous constructor parameter, source type-operator shells that stay in
    /// the anonymous constructor around the forwarded value)`.
    pub(crate) anonymous_super_forwards:
        std::collections::HashMap<(crate::fir::DeclarationId, u32), (u32, u8)>,
    /// Body-local static functions physically owned by a class, by exact function and owner
    /// identity. Their `$default` ABI uses the ordinary function marker rather than
    /// constructor/value-class markers; a target also uses the owner to re-enter a suspend local
    /// without scanning classes or recovering ownership from its generated name.
    pub(crate) class_static_local_functions: std::collections::HashMap<FunId, TypeName>,
    pub classes: Vec<IrClass>,
    /// Exact generated-constructor identities keyed by their semantic role within a class.
    generated_secondary_constructors:
        std::collections::HashMap<(TypeName, IrSecondaryConstructorRole), u32>,
    /// Exact `New` expressions targeting a generated secondary constructor. This call-site edge
    /// preserves the selected declaration through target representation passes.
    generated_secondary_constructor_calls:
        std::collections::HashMap<ExprId, (ClassId, IrSecondaryConstructorRole, u32)>,
    /// Source type aliases declared directly in a classifier, keyed by that classifier's semantic
    /// identity. These are pending-free declaration headers copied by common lowering; they carry
    /// no body, parser identity, source range, or backend representation.
    pub class_type_aliases: std::collections::HashMap<TypeName, Vec<IrTypeAlias>>,
    /// Source functions declared directly in this file's package. These are complete semantic
    /// declaration records copied from finalized Pass-1 headers. A backend may combine them with
    /// the post-pass physical function realization, but must not reopen the frontend index.
    pub package_functions: Vec<IrPackageFunction>,
    /// The Kotlin `main` this file declares, if any. See [`IrEntryPoint`].
    pub entry_point: Option<IrEntryPoint>,
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
    /// A class companion's initializer body (property stores that stayed in source order, then
    /// `init` blocks), run by the outer class `<clinit>` after the companion instance is stored.
    /// The companion constructor does not run it. Interface and enum companions are absent: their
    /// initializers stay on the companion's own storage path.
    companion_clinit_bodies: std::collections::HashMap<TypeName, ExprId>,
    /// JVM field name of a static whose source name its owner already uses for another static
    /// field (a hoisted companion property beside a same-named `companion { … }` property).
    jvm_static_field_names: std::collections::HashMap<u32, String>,
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
    /// Each annotation class this file declares → the closed declaration default of each element
    /// that has one, by element name. A backend records them with the annotation's declaration.
    pub annotation_element_defaults: std::collections::HashMap<TypeName, Vec<(String, AnnoValue)>>,
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
    /// Lines generated expressions map to at one physical point; see `debug_lines`.
    generated_lines: debug_lines::GeneratedLineMarks,
    /// Source names for `IrExpr::Variable` nodes included in `LocalVariableTable`.
    /// Compiler-generated temporaries are omitted.
    pub value_names: std::collections::HashMap<u32, String>,
    /// Sparse inline origin for debug-visible local declarations. Source spelling remains in
    /// `value_names`; target-specific decoration is deliberately deferred to the backend.
    debug_local_provenance: std::collections::HashMap<ExprId, IrDebugLocalProvenance>,
    /// What same-module inline expansions copied and left unread; see `inline_copies`.
    inline_expansions: inline_copies::InlineExpansions,
    /// Lifted lambda implementation id → stable source origin and lexical binding context.
    pub lambda_origins: std::collections::HashMap<u32, IrLambdaOrigin>,
    /// Specialized function → the implementation it was copied from and the expansion that copied it.
    pub specialized_functions: std::collections::HashMap<FunId, IrSpecializedFunction>,
    /// Copied construction → its position in the expansion, shared with
    /// [`Self::specialized_anonymous_classes`] so one call numbers lambdas and anonymous objects
    /// together.
    pub(crate) specialized_expansion_order: std::collections::HashMap<FunId, u32>,
    /// Specialized anonymous class → the class it was copied from and the expansion that copied it.
    pub(crate) specialized_anonymous_classes:
        std::collections::HashMap<ClassId, IrSpecializedAnonymousClass>,
    /// Declaration classes of anonymous objects whose members use a reified inline parameter.
    /// Recorded when a call site copies the class. The JVM emits `needClassReification` for these
    /// classes from this set alone.
    pub(crate) reified_anonymous_declarations: std::collections::HashSet<ClassId>,
    /// Lambda implementations whose bodies execute a runtime reified operation. Every source
    /// implementation, including a nested one, and each specialized call-site copy are recorded
    /// semantically. A backend independently chooses the physical closure representation needed
    /// to realize that operation.
    pub(crate) runtime_reified_lambda_implementations: std::collections::HashSet<FunId>,
    /// Class index of a function already published as that class's method.
    pub(crate) class_method_owners: std::collections::HashMap<FunId, Vec<u32>>,
    /// `ExprId` → the expression's LOGICAL (source) type as the checker inferred it, recorded verbatim by
    /// the lowerer — NOT erased. The value-class pass consults it to recover the representation of a value
    /// whose IR node alone is ambiguous: a library call returns a physical `Object` descriptor, but its
    /// logical type may be a value class (`runCatching{…}: Result`), so the pass knows the result is the
    /// value class's UNBOXED underlying, not an opaque `Object`. Populated for every lowered expression;
    /// consumed by the value-class pass (the sole owner of value-class knowledge) and — for scalar and
    /// `String` types only, where logical = physical representation — by the suspend pass's operand
    /// snapshot typing (`hoisted_value_ty`) for external callees.
    pub logical_types: std::collections::HashMap<u32, Ty>,
    /// Deferred source-local declaration → its declared semantic type before an inline expansion
    /// specializes type parameters. The parser supplies a target-neutral zero expression because
    /// the source omitted an initializer; a backend uses this provenance to choose that target's
    /// storage representation and physical zero without changing the specialized semantic type.
    pub deferred_local_types: std::collections::HashMap<ExprId, Ty>,
    /// What common lowering decided about each `when` (and `if`) a backend emits.
    pub(crate) whens: when_facts::IrWhenFacts,
    /// Source binding reads and the checked binding's reassignment contract. The expression key is
    /// always an [`IrExpr::GetValue`]; storage realization remains backend-owned.
    pub binding_read_stability: std::collections::HashMap<ExprId, IrBindingStability>,
    /// The null guard of every lowered safe call (`a?.f()`), and of an elvis over one
    /// (`a?.f() ?: b`), keyed by its `When`: the first branch tests the temporary against `null` and
    /// yields the null result (the elvis's right side), the `else` is the selector (the elvis's
    /// left value). Recorded where they are lowered, so a backend lays the guard out as its platform
    /// compiler does without recognizing the shape again.
    pub null_guards: std::collections::HashSet<ExprId>,
    /// `::prop.isInitialized`, recorded when lowering `LateinitFieldRead`. The probe compares the
    /// property with null and does not throw. A backend chooses the raw field, getter, or bridge.
    pub lateinit_initialization_probes: std::collections::HashSet<ExprId>,
    /// Lowered `&&`/`||` identities and their source operators. Backends consume this provenance;
    /// the same generic `when` written by hand must remain distinguishable.
    pub short_circuits: std::collections::HashMap<ExprId, IrShortCircuitKind>,
    /// Local assignments a lowering generated without the increment or compound-assignment form a
    /// source update has (kotlinc's `index = index + 1` in `WithIndexLoopHeader`). A target emits
    /// them as the plain arithmetic and store they are, never as a fused increment.
    pub plain_updates: std::collections::HashSet<ExprId>,
    /// Pre-test loops whose body block is a transparent scope, as the body of a `for` loop kotlinc's
    /// `ForLoopsLowering` rebuilt as a `while` is (an `IrComposite`): the declarations it opens with
    /// stay in scope until the loop ends.
    pub transparent_loop_bodies: std::collections::HashSet<ExprId>,
    /// Post-test loops a counted `for` loop was realized as that keep the `for` loop's origin, with
    /// a body opening with the loop variable's initialization (kotlinc's `FOR_LOOP_NEXT` block
    /// heading a `FOR_LOOP_INNER_WHILE` body: the guarded do-while that steps first). kotlinc's
    /// `visitDoWhileLoop` emits that initialization ahead of the loop's labels, so the body's locals
    /// keep their ranges through the condition. Any other post-test loop, a written one or the
    /// overflow-guarded counted shape (`doWhileCounterLoopOrigin`), ends a local the condition does
    /// not read at the condition (`endUnreferencedDoWhileLocals`).
    pub for_loop_next_loops: std::collections::HashSet<ExprId>,
    /// The casts the source wrote (`x as T`). Every other cast is compiler-inserted, kotlinc's
    /// `IMPLICIT_CAST` (an `as?`'s narrowing after its `is`, a carrier handed on as its function
    /// type): the value is already known to be a `T`, so a backend narrows it with a plain cast
    /// rather than the checked cast a written `as` needs.
    pub written_casts: std::collections::HashSet<ExprId>,
    /// Constants that are the value of an operation over constants (`1 + 2`), folded like kotlinc's
    /// `ConstEvaluationLowering`, rather than a literal the source wrote. Kotlin metadata's
    /// `HAS_CONSTANT` describes only a literal initializer, so a backend must not read it from these.
    pub folded_constants: std::collections::HashSet<ExprId>,
    /// The subset of [`Self::null_guards`] introduced by an elvis over a safe call. A backend may
    /// need this provenance when statement emission differs from a safe call's literal-null arm;
    /// it must not recover that distinction from the lowered branch shape.
    pub elvis_safe_call_guards: std::collections::HashSet<ExprId>,
    /// Physical type before a semantic read coercion.
    pub physical_types: std::collections::HashMap<u32, Ty>,
    /// Value-class type operations whose box and carrier are different JVM classes.
    /// Realization writes the role; emission reads it and does not rediscover it.
    pub value_class_type_operations: std::collections::HashMap<ExprId, IrValueClassTypeOperation>,
    /// JVM value-class unbox call -> the exact type operation selected as its receiver. The
    /// adaptation boundary records this edge when it creates the call; later representation
    /// analysis consumes the identity and never recognizes an unbox operation by its spelling.
    pub(crate) value_class_unbox_type_operation_edges: std::collections::HashMap<ExprId, ExprId>,
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
    /// Exact source declaration name for every checked function realization. Common lowering
    /// publishes it while the stable declaration identity is live; a backend may then replace
    /// [`IrFunction::name`] with a physical spelling such as JVM `@JvmName` or a value-class mangle.
    /// Metadata, reflection, and receiver-name projection consume this semantic name and never
    /// recover it from the realized function spelling.
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
    /// Members this file's `companion { … }` blocks declared, placed on their classes.
    pub companion_blocks: IrCompanionBlocks,
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
    type_reflection: TypeReflectionFacts,
    /// Function ids declared `inline`. This is the declaration-semantic set used by metadata;
    /// visibility-specific inline handling remains in [`Self::public_inline_functions`].
    pub inline_fns: std::collections::HashSet<u32>,
    /// Checked inline-accessor functions and use-site splice records, owned with property IR.
    pub(crate) inline_property_access: properties::IrInlinePropertyAccess,
    /// Function ids with a value parameter (context parameters excluded) or extension receiver of a
    /// function type, as the checker classified the declaration.
    pub function_typed_parameter_fns: std::collections::HashSet<u32>,
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
    /// Blocks that are a callable's own scope: its body block, the source block the body lowers
    /// from, and any block lowering wraps between them. A value declared in one is in scope until
    /// the callable ends, as a Kotlin function's own locals are; a nested block's end with it.
    pub callable_scopes: std::collections::HashSet<ExprId>,
    /// Anonymous initializer (`init { … }`) bodies. kotlinc gives such a block's opening and
    /// closing lines entries of their own, even where no instruction of the block sits on them.
    pub initializer_blocks: std::collections::HashSet<ExprId>,
    /// Compiler-generated `Variable` declarations whose semantic role is holding a call operand:
    /// an inline expansion's parameter (argument, receiver, or capture), or an argument preserved
    /// for evaluation order. This is provenance, not a storage decision; a backend decides how
    /// that role participates in its own frame or register allocation.
    pub call_operand_bindings: std::collections::HashSet<ExprId>,
    /// Function ids declared `operator` — `@Metadata` `Function.flags` bit 8, so a consumer admits
    /// the conventional call form (`recv(args)`, `a[i]`); the JVM method carries no such bit.
    pub operator_fns: std::collections::HashSet<u32>,
    /// Function ids declared `infix` (metadata bit 9, `a f b`) and, separately, `tailrec` (bit 11).
    pub infix_fns: std::collections::HashSet<u32>,
    pub tailrec_fns: std::collections::HashSet<u32>,
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
    /// Function id → checked non-default Kotlin return-value status, which Kotlin metadata records.
    pub fn_return_value_statuses: std::collections::HashMap<u32, crate::types::ReturnValueStatus>,
    /// Class identity → per-primary-constructor-parameter checked default expression (`None` =
    /// required). This is the target-neutral constructor contract. A target backend consumes it when
    /// choosing that class's physical default-argument ABI; common lowering does not distinguish value
    /// classes from ordinary classes here. Expressions use the ordinary constructor frame (`this` =
    /// value 0, parameters = 1..=n) until a backend deliberately reframes them.
    class_ctor_defaults: std::collections::HashMap<TypeName, Vec<Option<u32>>>,
    /// Overridable methods: `open`, `abstract`, a non-`final` `override` (the data-class
    /// `Object`-overrides among them), and interface members. Every other instance method is
    /// final, in an open class as much as in a final one. The JVM backend omits `ACC_FINAL` for a
    /// `FunId` in this set.
    pub open_methods: std::collections::HashSet<u32>,
    /// Kotlin visibility of methods whose declaration is not public. This is the single semantic
    /// source for method access, dispatch, metadata, parameter checks, and generated bridges;
    /// backends map it to their own access representation without parallel private/protected sets.
    pub method_visibilities: std::collections::HashMap<FunId, crate::types::Visibility>,
    /// Operations realized as a call to one exact function: value-class `-impl` calls, private getters.
    pub(crate) jvm_member_targets: std::collections::HashMap<ExprId, FunId>,
    /// Member operations tied to an exact selected declaration in this module. The target identity
    /// survives physical virtual/property realization so a backend can implement access bridges
    /// without owner/name lookup.
    pub(crate) module_member_accesses: std::collections::HashMap<ExprId, IrModuleMemberAccess>,
    /// JVM-realized protected dependency calls, keyed by the physical call operation. JVM
    /// emission turns one whose caller is outside the subclass into that subclass's `access$`
    /// bridge. This is populated only by the JVM external-call realization boundary.
    pub(crate) jvm_protected_dependency_calls: std::collections::HashMap<
        ExprId,
        crate::jvm::protected_dependency_calls::ProtectedDependencyCall,
    >,
    /// Private methods a synthesized callable-reference class calls; each gets one access bridge.
    pub function_reference_access_bridges: std::collections::HashSet<u32>,
    /// Lambda impls pre-marked `inline_only` by `mark_must_inline_lambdas` (a must-inline callee's
    /// message lambda, assumed spliced). If emission nonetheless records an `invokedynamic` for one,
    /// the two-pass driver RESCUES it — emits the method after all — so the reference never dangles.
    pub must_inline_lambdas: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_SYNTHETIC` — currently a value class's `box-impl`/`unbox-impl` (the
    /// compiler-manufactured box adapters). The JVM backend ORs `0x1000` for a `FunId` in this set.
    pub synthetic_methods: std::collections::HashSet<u32>,
    /// Exact generated JVM value-class representation methods and their classfile order after the
    /// private primary constructor. The JVM value-class pass records the function identities when
    /// it creates them; emission must not recover their roles from generated method spellings.
    pub(crate) jvm_value_class_representation_order: std::collections::HashMap<u32, u8>,
    /// The static `constructor-impl` realizing each value-class constructor, with that constructor's
    /// ordinal (`0` is the primary). Its `$default` stub takes kotlinc's `DefaultConstructorMarker`.
    pub(crate) jvm_value_class_constructor_impls: std::collections::HashMap<u32, u32>,
    /// The Kotlin-level signature of each value-class method the JVM pass creates or moves onto the
    /// carrier before its main erasure (`constructor-impl`, the synthesized and user-written
    /// `equals`/`hashCode`/`toString` statics, computed accessors), recorded while still semantic.
    pub(crate) jvm_value_class_member_signatures: std::collections::HashMap<u32, IrGenericSig>,
    /// Generated JVM methods kotlinc writes without nullability annotations. The JVM value-class
    /// pass records exact function identities; common lowering does not interpret this set.
    pub(crate) jvm_nullability_unannotated_methods: std::collections::HashSet<u32>,
    /// Lambda implementations `LambdaMetafactory` cannot adapt that the JVM lambda-class pass left
    /// unrealized. Emitting one as an `invokedynamic` is an error, never a fallback.
    pub(crate) jvm_unrealized_lambda_classes: std::collections::HashSet<u32>,
    /// JVM value-class member implementations whose leading physical carrier parameter realizes a
    /// source dispatch receiver and therefore carries no nullability annotation.
    pub(crate) jvm_value_class_receiver_impls: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_BRIDGE` (0x40) — e.g. a `@Serializable` serializer's
    /// `typeParametersSerializers`. The JVM backend ORs `0x40` for a `FunId` in this set.
    pub bridge_methods: std::collections::HashSet<u32>,
    /// Per-method `(param index, boxed type)` for DEFAULTED params whose value class's carrier accepts
    /// null: the base method unboxes them; its `$default` stub and call sites keep them boxed (kotlinc).
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
    /// Provenance and initialization placement for statics generated without a Kotlin declaration.
    generated_static_facts: std::collections::HashMap<u32, generated_members::GeneratedStaticFacts>,
    /// `FunId`s of methods carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's `get<Prop>$annotations()` markers, which kotlinc deprecates HIDDEN. ASM
    /// surfaces the attribute as `ACC_DEPRECATED` (0x20000) in the access int, so the ABI gate compares it.
    pub deprecated_methods: std::collections::HashSet<u32>,
    /// Internal names of classes carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's generated `$$serializer` object, which kotlinc deprecates HIDDEN.
    deprecated_classes: std::collections::HashSet<TypeName>,
    /// Target-neutral declaration and selected-call facts for constructors that mention value classes.
    value_class_constructor_facts: value_class_constructors::ValueClassConstructorFacts,
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
    /// `FunId`s of `suspend fun`s, tagged by common lowering. The coroutine pass (`jvm::suspend`) owns the
    /// whole transform: it rewrites each to the continuation-passing-style ABI (an extra
    /// `kotlin.coroutines.Continuation` parameter, return type erased to `Object`) and, for a function
    /// with suspension points, builds the state machine + continuation class. Common lowering keeps a
    /// `suspend fun` plain, mirroring how value classes stay plain until their target pass.
    pub suspend_funs: Vec<u32>,
    /// Functions that exist only to forward an interface member to its delegate (`: I by d`).
    /// A suspend forwarder threads its own continuation into that one call. It is not a user tail
    /// call: a reference-carrier value class still forwards, and the backend checkcasts the carrier
    /// after returning `COROUTINE_SUSPENDED` unchanged.
    pub(crate) interface_delegation_forwarders: std::collections::HashSet<u32>,
    /// `FunId`s the source declared `tailrec` that KOTLIN loops and the checked lowering does not.
    /// The declaration promises constant stack and the body still recurses, so a backend that
    /// cannot supply the guarantee itself must decline the function rather than emit a program that
    /// overflows the stack at a depth the source expects to survive. (The JVM lane reaches the same
    /// conclusion by skipping the file in `ir_lower`; this table is how the CHECKED lowering states
    /// the same fact to its own consumers.)
    ///
    /// Only the context-parameter shape qualifies. A `tailrec` that was loop-transformed is absent,
    /// and so are the two shapes that look like failures and are not: an overridable member, which
    /// kotlinc refuses to loop as well, and a self-call the sweep leaves behind, which kotlinc also
    /// leaves — it reports NON_TAIL_RECURSIVE_CALL for exactly those and emits the call.
    pub unlooped_tailrec: std::collections::HashSet<u32>,
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
    /// Each function whose JVM name the value-class pass chose (a `-<hash>` mangle, a member's
    /// `-impl`, a constructor's `constructor-impl`). Only that pass writes it; kotlinc names the
    /// callables it lifts out of such a function after this name.
    pub(crate) value_class_renamed_functions: std::collections::HashSet<FunId>,
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
    /// return, before CPS erasure to `Object`), recorded from the checked FIR call target: the
    /// cross-unit complement of `suspend_funs`, for a callee in ANOTHER file or a dependency.
    pub suspend_calls: std::collections::HashMap<u32, Ty>,
    /// A cross-unit suspend call → the declared results of the declarations its callee overrides,
    /// nearest first, when it overrides any: how its result crosses the continuation is the target's.
    pub suspend_call_overridden_results: std::collections::HashMap<ExprId, Box<[Ty]>>,
    /// Non-call expressions whose evaluation is a coroutine SUSPENSION POINT → their logical result
    /// type. Unlike [`Self::suspend_calls`], these nodes do not name a callee and must not have a
    /// continuation argument appended by the coroutine pass. The motivating intrinsic is an inlined
    /// `suspendCoroutineUninterceptedOrReturn` block: lowering has already materialized its continuation
    /// use inside the block, while the coroutine pass still splits and resumes around it as one atomic
    /// point, never mistaking it for a cross-unit call merely because both can suspend.
    pub intrinsic_suspension_points: std::collections::HashMap<u32, IrIntrinsicSuspensionPoint>,
    /// `FunId` → the backend-agnostic generic-signature SHAPE of a type-parameterized function. The JVM
    /// backend formats this into a `Signature` attribute; the IR itself holds no target descriptors.
    pub signatures: std::collections::HashMap<u32, IrGenericSig>,
    /// Exact non-local type-parameter declarations referenced by a generic callable's own bounds.
    /// Common lowering resolves each identity in the callable's declaration scope and closes over
    /// further bound references, so a backend never reconnects `<T : S>` by source spelling.
    pub(crate) callable_bound_type_parameters:
        std::collections::HashMap<FunId, Vec<IrTypeParameter>>,
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
    /// The declaration-owned source lines of each SECONDARY constructor, by its stable declaration.
    /// The declaration metadata handoff records them while the parser unit is live; lowering copies
    /// them onto the constructor it builds, which happens after that unit's syntax is gone.
    pub secondary_ctor_lines:
        std::collections::HashMap<crate::fir::DeclarationId, IrSecondaryCtorLines>,
    /// Declared PRIMARY-constructor visibility per class (`class C protected constructor(…)`);
    /// absent = public. It sets `@Metadata` `Constructor.flags` and the JVM access.
    pub ctor_visibilities: std::collections::HashMap<TypeName, crate::types::Visibility>,
    /// A declared function's `vararg` parameter — class `@Metadata` must emit
    /// `ValueParameter.vararg_element_type` (f4) or a consumer demands one literal array (`too many
    /// arguments`).
    pub fn_varargs: std::collections::HashMap<u32, IrVarargParameter>,
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
    /// Functions whose debug representation retains receiver/parameter bindings even when the body
    /// deliberately has no executable source line. Generated declarations use this explicit
    /// source/debug contract instead of inventing a line merely to retain their binding names.
    pub fn_debug_locals: std::collections::HashSet<FunId>,
    /// FunId → 1-based source line of a BLOCK body's closing `}` — kotlinc maps a `Unit` fn's
    /// implicit `return` there in the `LineNumberTable`. Same side-map rationale as `fn_decl_lines`.
    pub fn_close_lines: std::collections::HashMap<u32, u32>,
    /// FunId → 1-based source line of the DECLARATION itself, never body-rewritten. `fn_decl_lines`
    /// holds the line the method BODY maps to (an expression body's own line); a `$default` stub's
    /// LineNumberTable instead points here — the two differ exactly when an expression body starts
    /// on a later line than the signature. Same side-map rationale as `fn_decl_lines`.
    pub fn_sig_lines: std::collections::HashMap<u32, u32>,
    /// FunId → source byte offset of the `fun` keyword. The JVM backend reports a platform
    /// declaration clash at this position. A method with no entry is a bridge, a default stub, or
    /// another representation, not a source declaration.
    pub fn_signature_offsets: std::collections::HashMap<u32, u32>,
    /// FunId → the SOURCE BYTE OFFSET of the declaration a class member realizes (a property's
    /// accessors carry the property's offset). kotlinc emits class members in DECLARATION order,
    /// interleaving property accessors among functions; lowering groups them by kind, so the JVM
    /// emitter re-sorts its emission (only) by this key. A method with no entry (a data-class
    /// synthetic, an appended lambda impl, an access bridge) keeps its position after the declared
    /// members. Resolution-facing indexes never see the sorted order.
    pub fn_source_order: std::collections::HashMap<u32, u32>,
    /// FunId → the position this `suspend` function's continuation class takes in the
    /// generated-class sequence its enclosing scope numbers, 1-based in declaration order.
    ///
    /// That sequence is shared with the anonymous objects those bodies declare, and the pass that
    /// names them is the only one that can say which positions it left free. This is that answer,
    /// carried as provenance; the JVM backend is where it becomes a class name. A suspend function
    /// with no entry has no source declaration behind it.
    pub fn_continuation_ordinal: std::collections::HashMap<u32, u32>,
    /// Class fq-internal-name → its generic-signature SHAPE (type parameters + bounds), for a generic
    /// class. The JVM backend formats it into the class `Signature` attribute.
    class_signatures: std::collections::HashMap<TypeName, IrGenericSig>,
    /// Class fq-internal-name → `(field name, type-parameter name)` for each field whose declared type
    /// is a bare type parameter (`class Pair<A, B>(val a: A)` → `[("a", "A")]`). The JVM backend formats
    /// each into a field `Signature` (`TA;`). Backend-agnostic: only the type-parameter name is stored.
    field_signatures: std::collections::HashMap<TypeName, Vec<(String, String)>>,
    /// Value classes REFERENCED in this file but declared elsewhere (fq identity → declared
    /// underlying semantic type): sibling-file declarations and dependencies share this one table.
    /// Checked classifier facts populate it before plugins run, and a representation backend may
    /// extend it while inventorying the rest of the file. A value class the type model carries as a
    /// native scalar (the unsigned integers) is recorded like any other; its representation is the
    /// backend's question.
    external_value_classes: std::collections::HashMap<TypeName, Ty>,
    /// The declarations of [`Self::external_value_classes`] as their providers published them: the
    /// underlying type over the declaration's own type parameters.
    external_value_class_declarations:
        std::collections::HashMap<TypeName, crate::types::DeclaredValueClass>,
    /// Expression identity → `(declared value-class name, erased underlying type)` for a construction
    /// rewritten in place by the JVM value-class pass. This records semantic origin rather than the
    /// generated helper's spelling: a source `new` remains distinguishable from an unrelated static call
    /// whose user-written name happens to resemble a backend helper. Consumers must treat the entry as
    /// valid only while the rewritten expression remains at the same arena index.
    erased_value_constructions: std::collections::HashMap<ExprId, (TypeName, Ty)>,
    /// Checked [`crate::types::ClassifierRole`] of each referenced classifier that has one.
    classifier_roles: std::collections::HashMap<TypeName, crate::types::ClassifierRole>,
    /// Call `ExprId` → checked reified-type substitutions for a classpath inline declaration whose
    /// compiled body a target may splice. The values stay backend-agnostic [`Ty`]s here. At the JVM
    /// boundary, a concrete value becomes a class-pool operand while a reified parameter of the host
    /// declaration keeps the callee's `reifiedOperationMarker` for the host's caller to specialize.
    /// This is the classpath analogue of the IR inliner's same-file `reified_subst`.
    pub reified_call_subst: std::collections::HashMap<u32, Vec<(String, Ty)>>,
    /// Try `ExprId` → source spelling of each catch that is still a reified parameter of a
    /// declaration in this file, parallel to that try's catches. `None` is an ordinary clause.
    /// The JVM reified-operation pass records the declaration's source spelling while the semantic
    /// identity is still on the catch. Try emission reads it for `reifiedOperationMarker` mode 7
    /// and does not recover the spelling from the identity.
    pub(crate) reified_catch_markers: std::collections::HashMap<ExprId, Vec<Option<String>>>,
    /// Call `ExprId` → every checked type argument of a classpath inline declaration, under the
    /// dependency metadata's formal name. JVM anonymous-object regeneration specializes generic
    /// class signatures with this full map; runtime reified operations use the narrower map above.
    pub(crate) inline_call_type_arguments: std::collections::HashMap<u32, Vec<(String, Ty)>>,
    /// Exact methods the serialization child cache generated: its factories and accessor.
    ///
    /// kotlinc's member order for a `@Serializable` class puts the deserialization `<init>`
    /// directly after `write$Self`, and the child-serializer cache's factories and `…$cp` accessor
    /// after it. They carry no source order or source-callable generic signature; this identity set
    /// lets the JVM boundary realize both facts without treating every synthetic method alike.
    pub serialization_cache_methods: std::collections::HashSet<u32>,

    /// Extension-call `ExprId` → the extension's DECLARED (un-erased) receiver source type, forwarded
    /// verbatim from the checked callable's `source_receiver`. Common lowering records it with no
    /// value-class reasoning; the value-class pass reads it to decide box/unbox at the receiver. The signal
    /// distinguishes `fun Result<T>.getOrThrow()` (receiver `kotlin/Result` — a value class whose facade
    /// method takes the UNBOXED underlying, so a `Boxed` receiver unboxes) from a generic `fun <T> T.foo()`
    /// (receiver a type variable — erases to `Object`, receiver stays boxed) even though both erase
    /// identically in the JVM descriptor. Only concrete declared receivers are recorded (a `Var` receiver
    /// is `None` at the source and never inserted).
    pub ext_call_source_receiver: std::collections::HashMap<u32, Ty>,
    /// JVM call expression → exact physical realization inherited through its checked override
    /// target. A JVM planning pass selects this from provider-published candidates and stable
    /// function/property override identities; emission never scans source/member spellings.
    pub(crate) jvm_overridden_call_realizations:
        std::collections::HashMap<ExprId, crate::libraries::OverriddenCallRealization>,
    /// Exact provider-selected language-member roles retained on their call expressions. This is
    /// declaration identity data, not a spelling-based backend lookup.
    pub semantic_call_roles: std::collections::HashMap<ExprId, crate::types::SemanticCallRole>,
    /// Member call or property access `ExprId` → the classifier its dispatch receiver statically
    /// has, after smart casts: the class the member was selected through, whether this module or a
    /// dependency declares it. A type-parameter receiver records nothing. What a target names for
    /// the call is its own decision.
    pub dispatch_classes: std::collections::HashMap<ExprId, TypeName>,
    /// Call `ExprId` → the callee's DECLARED (un-erased, pre-substitution) return type, forwarded
    /// verbatim from the checked external call's `declared_ret`. Common lowering records it with no
    /// value-class reasoning; the value-class pass reads it to decide the RESULT's
    /// representation, exactly as `ext_call_source_receiver` does for the receiver.
    ///
    /// The distinction it carries cannot be recovered from the descriptor: a value class returned by
    /// declaration (`A.create(): A<String>`, whose mangled method hands back the erased carrier) and
    /// the same value class arriving BOXED out of a generic slot (`List<TokenBox>.get`) both
    /// spell `()Ljava/lang/Object;`. The declaration separates them — `create` declares `A`, while
    /// `get` declares the type parameter `E`, which remains a type-parameter identity rather than
    /// classifying as a value class. Only NON-NULL declared returns are recorded: a nullable value
    /// class really is boxed.
    pub call_declared_ret: std::collections::HashMap<u32, Ty>,
    /// The `ImplicitCoercion`s FIR lowering places between a call's declared result and its
    /// call-site substitution (`fun <T> f(): T` read as `Int`). Which coercion a node is comes from
    /// where it was lowered, not from its shape: an adaptation or widening over the same call is
    /// another coercion. A target decides whether the conversion crosses a physical result slot.
    pub declaration_result_coercions: std::collections::HashSet<ExprId>,
    /// Call-owned supplied-argument edges whose declaration parameters mention type parameters.
    /// A target may adapt only these exact edges, and only when its own carrier rules select them.
    pub declaration_argument_boundaries:
        std::collections::HashMap<ExprId, Box<[IrDeclarationArgumentBoundary]>>,
    /// Realized dependency-call `ExprId` → declaration parameter types in the order of the call's
    /// ordinary argument vector. These are copied from the provider record selected by FIR, never
    /// reconstructed from a name or descriptor. A backend representation pass needs this sparse fact
    /// only where source and physical shapes are ambiguous—for example, both a direct `Result<T>`
    /// parameter and a generic `T` parameter erase to JVM `Object`, but only the latter takes a box.
    /// Dispatch receivers stay separate; a static realization prepends its selected receiver.
    pub call_declared_params: std::collections::HashMap<u32, Box<[Ty]>>,
    /// Call `ExprId` → JVM parameter types in declaration order. Semantic `call_declared_params`
    /// stay the Kotlin types; this plan is only the physical slot a selected argument edge is
    /// adapted to. Mask and marker operands are not entries.
    pub physical_call_parameters: std::collections::HashMap<ExprId, Box<[Ty]>>,
    /// Construction `ExprId` → the selected constructor's declared semantic parameter types in
    /// argument order. A generic constructor can consume a value-class box through bare `T` even
    /// when its physical descriptor and that value class's carrier are both `Object`; JVM emission
    /// consumes this identity-backed fact instead of reinterpreting the descriptor.
    pub(crate) construction_declared_params: std::collections::HashMap<ExprId, Box<[Ty]>>,
    /// Construction `ExprId` → the selected module constructor.
    pub(crate) construction_targets: std::collections::HashMap<ExprId, IrConstructorTarget>,
    /// Realized static call `ExprId` → the operand index its extension receiver was placed at. A
    /// JVM inliner reads an extension receiver differently from the parameters after it.
    pub static_extension_receivers: std::collections::HashMap<u32, u32>,
    /// Realized inline-call `ExprId` → each call operand's parameter `crossinline`/`noinline`
    /// modifier, published after the physical receiver operands are inserted.
    pub call_inline_modifiers:
        std::collections::HashMap<u32, Box<[crate::types::InlineParameterModifier]>>,
    /// Stable property-operation identity → the declaration's semantic value type before
    /// use-site generic substitution. Resolution knows this fact uniformly for every source owner;
    /// recording it here lets a backend derive the physical accessor boundary without asking whether
    /// the declaration came from this file, a sibling file, or a dependency. This deliberately carries
    /// no accessor spelling or target descriptor. As with the JVM realization table below, the stable
    /// operation identity survives backend rewrites that move a property node to another arena slot.
    pub property_declaration_types: std::collections::HashMap<u32, Ty>,
    /// A call expression the JVM serialization plugin bound to one synthesized property
    /// accessor, by class and property index. The value-class pass renames only these calls
    /// when it mangles that accessor. Other calls that share a JVM spelling stay bound to
    /// their own declarations.
    pub synthesized_accessor_calls: std::collections::HashMap<ExprId, SynthesizedAccessorCall>,
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
    /// Lambda implementation id → the body's own inferred result when the selected language
    /// level keeps that type as the implementation signature (before 2.4). Absence at current
    /// levels is the checked decision to use the caller-facing result. A backend consumes this
    /// semantic fact without receiving a second language-version switch.
    pub lambda_inferred_results: std::collections::HashMap<FunId, Ty>,
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

impl IrTypeParameter {
    /// A use of this parameter as a type.
    pub(crate) fn ty(&self) -> Ty {
        Ty::ty_param(&self.semantic_name, self.upper_bound())
    }

    /// The representative upper bound: the first declared bound, else `Any?`.
    pub(crate) fn upper_bound(&self) -> Ty {
        self.bounds
            .first()
            .map_or(Ty::nullable(Ty::obj("kotlin/Any")), |(bound, _)| *bound)
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrAppliedClassifier {
    pub classifier: TypeName,
    pub applied: Ty,
    pub depth: u32,
}

impl IrFile {
    /// A method's Kotlin declaration visibility. Public is the compact default for generated and
    /// ordinary declarations; every non-public source or generated method is recorded explicitly.
    pub fn method_visibility(&self, function: FunId) -> crate::types::Visibility {
        self.method_visibilities
            .get(&function)
            .copied()
            .unwrap_or(crate::types::Visibility::Public)
    }

    pub fn set_method_visibility(&mut self, function: FunId, visibility: crate::types::Visibility) {
        if visibility.is_public() {
            self.method_visibilities.remove(&function);
        } else {
            self.method_visibilities.insert(function, visibility);
        }
    }
    pub(crate) fn record_class_source_qualified_name(
        &mut self,
        class: ClassId,
        name: impl Into<String>,
    ) {
        assert!(
            self.class_source_qualified_names
                .insert(class, KtString::from(name.into()))
                .is_none(),
            "a source classifier has one qualified declaration name"
        );
    }

    pub(crate) fn class_source_qualified_name(&self, class: ClassId) -> Option<KtString> {
        self.class_source_qualified_names.get(&class).cloned()
    }

    pub(crate) fn record_class_source_order(&mut self, class: ClassId, source_order: u32) {
        assert!(
            self.class_source_orders
                .insert(class, source_order)
                .is_none(),
            "a source classifier has one stable declaration order"
        );
    }

    pub(crate) fn class_source_order(&self, class: ClassId) -> Option<u32> {
        self.class_source_orders.get(&class).copied()
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

    pub fn mark_synthetic_class(&mut self, internal: TypeName) {
        self.synthetic_classes.insert(internal);
    }

    pub fn is_synthetic_class(&self, internal: TypeName) -> bool {
        self.synthetic_classes.contains(&internal)
    }

    pub fn mark_deprecated_class(&mut self, internal: TypeName) {
        self.deprecated_classes.insert(internal);
    }

    pub fn is_deprecated_class(&self, internal: TypeName) -> bool {
        self.deprecated_classes.contains(&internal)
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

    /// `class` applied to its own type parameters (`C<T>`): the type of its `this`.
    pub(crate) fn class_type(&self, class: &IrClass) -> Ty {
        let arguments: Vec<Ty> = self
            .class_signature_name(class.fq_name)
            .map(|signature| {
                signature
                    .type_params
                    .iter()
                    .map(IrTypeParameter::ty)
                    .collect()
            })
            .unwrap_or_default();
        Ty::obj_args_name(class.fq_name, &arguments)
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

    /// Record that `function` is a method of `class`. Specialization copies this edge instead of
    /// scanning every class method list to rediscover it.
    pub(crate) fn note_class_method(&mut self, class: u32, function: FunId) {
        self.class_method_owners
            .entry(function)
            .or_default()
            .push(class);
    }
}

mod data_class_members;
mod debug_lines;
mod debug_locals;
mod generated_members;
pub(crate) use data_class_members::IrDataClassMemberRole;
pub(crate) use debug_lines::UnitBodyExit;
pub use debug_locals::{IrCatchBinding, IrLambdaForm, IrLambdaOrigin};
pub(crate) use debug_locals::{IrDebugLocalProvenance, IrInlineLocalRole};
pub use generated_members::{
    IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadata, IrGeneratedFunctionMetadataScope,
    IrGeneratedFunctionPublication, IrGeneratedMemberPublication,
};
mod function_parameters;
pub use function_parameters::*;
mod traversal;
pub use traversal::*;
mod clone;
pub use clone::clone_expression_dag;
#[cfg(test)]
pub(crate) use clone::make_expression_children_unique;
pub(crate) use clone::{
    clone_class_method, clone_function_implementation, make_expression_children_unique_tracked,
};
mod semantic_validation;
pub use semantic_validation::{
    IncompleteIrFact, InvalidIrContract, NullableSamContractViolation, UndeterminedIrType,
};
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
