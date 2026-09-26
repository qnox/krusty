use super::delegate_calls::{FirDelegateCall, FirPropertyDelegatePlan};
use super::local_callables::BodyLocalCallableDeclarationId;
use std::collections::HashMap;

mod context_parameters;
mod value_parameters;
pub use value_parameters::{FirDefaultValue, FirValueParameter, FirVarargParameter};
pub(crate) mod debug_lines;
pub use debug_lines::{FirExpressionDebugLines, FirStatementDebugLines};
mod ranges;
pub use ranges::{
    FirProgressionClass, FirProgressionSource, FirRangeCounterKind, FirRangeOperation,
    FirRuntimeFunction,
};

use crate::diag::Span;
use crate::kt_string::KtString;
use crate::types::TypeName;

use super::body_work::BodyWorkItem;
use super::capture::{FirCapture, FirCaptureSource, FirImplicitReceiverCapture};
use super::header::{
    next_id, BodyOwnerId, CallableId, ControlTargetId, DeclarationId, DeclarationNameId, FirExprId,
    FirPlatformNarrowingId, FirSamConversionId, FirStatementId, LocalCallableId, LocalValueId,
    OriginId, PropertyId, SourceFileId, TypeParameterId,
};
use super::identities::ExternalCallableId;
use super::inline_body::FirInlineBodyPlan;
use super::local_class_capture::FirLocalClassCapture;
use super::signature::ResolvedTy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntheticOriginKind {
    ImplicitReceiver,
    ImplicitConversion,
    DefaultArgument,
    VarargArray,
    StringTemplateLiteral,
    MissingElseUnit,
    GeneratedAccessor,
    GeneratedControlFlow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    Source {
        file: SourceFileId,
        span: Span,
    },
    Synthetic {
        cause: OriginId,
        kind: SyntheticOriginKind,
    },
}

#[derive(Debug, Default)]
pub struct OriginStore {
    origins: Vec<Origin>,
}

impl OriginStore {
    pub fn source(&mut self, file: SourceFileId, span: Span) -> OriginId {
        self.push(Origin::Source { file, span })
    }

    pub fn synthetic(&mut self, cause: OriginId, kind: SyntheticOriginKind) -> OriginId {
        assert!(
            self.get(cause).is_some(),
            "a synthetic FIR origin must reference an existing cause"
        );
        self.push(Origin::Synthetic { cause, kind })
    }

    fn push(&mut self, origin: Origin) -> OriginId {
        let id = OriginId::from_raw(next_id(self.origins.len(), "origins"));
        self.origins.push(origin);
        id
    }

    pub fn get(&self, id: OriginId) -> Option<Origin> {
        self.origins.get(id.raw() as usize).copied()
    }

    pub fn len(&self) -> usize {
        self.origins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.origins.len() * std::mem::size_of::<Origin>()
    }
}

/// A checked implicit conversion selected by the frontend. Lowering applies this decision and does
/// not decide assignability, boxing, coercion, or smart-cast eligibility again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirConversionKind {
    NumericWidening {
        to: ResolvedTy,
    },
    NumericConversion {
        to: ResolvedTy,
    },
    NullabilityWidening {
        to: ResolvedTy,
    },
    SmartCast {
        to: ResolvedTy,
    },
    Sam(FirSamConversionId),
    /// A Java/platform value committed to Kotlin's non-null type. The checker records both the
    /// boundary and the source expression text used by Kotlin's failure; common lowering merely
    /// realizes the already-selected yields-or-throws operation.
    PlatformNarrowing {
        narrowing: FirPlatformNarrowingId,
        to: ResolvedTy,
    },
    /// Kotlin's one-way adaptation of an already-materialized regular function value to a suspend
    /// function value. Both complete callable shapes were selected by the frontend; lowering only
    /// synthesizes the forwarding closure.
    SuspendFunction {
        from: ResolvedTy,
        to: ResolvedTy,
    },
    CoerceToUnit,
}

/// Exact semantic SAM declaration selected by the ordinary checker. Backend descriptors and holder
/// classes are deliberately absent; the classifier, method declaration shape, and source function
/// shape are sufficient for target realization after overload selection is complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirSamConversion {
    pub classifier: TypeName,
    pub method: Box<str>,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub declared_parameters: Box<[ResolvedTy]>,
    pub declared_result: ResolvedTy,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// A nullable function value converts conditionally: `null` remains `null`; only a non-null
    /// function object is wrapped as the selected SAM classifier.
    pub nullable: bool,
}

/// Checked payload for one platform-type narrowing. This is language-level Java interop behavior,
/// not a JVM linkage decision: every target either realizes the same yields-or-throws boundary or
/// diagnoses that it cannot support Java platform values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirPlatformNarrowing {
    pub message: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirConversion {
    pub origin: OriginId,
    pub kind: FirConversionKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirReceiver {
    pub value: FirExprId,
    pub conversion: Option<FirConversion>,
}

/// One value whose consumer type and representation conversion were fixed by the checker. This is
/// used where source order is positional but there is no callable parameter identity, such as a
/// builtin array/String index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirConvertedValue {
    pub value: FirExprId,
    pub conversion: Option<FirConversion>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirTypeParameterRef {
    Module(TypeParameterId),
    External {
        callable: ExternalCallableId,
        ordinal: u32,
    },
}

impl From<TypeParameterId> for FirTypeParameterRef {
    fn from(parameter: TypeParameterId) -> Self {
        Self::Module(parameter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirTypeSubstitution {
    pub parameter: FirTypeParameterRef,
    pub value: ResolvedTy,
    /// Additional constituents of an inferred flow-intersection type argument. `value` is the
    /// primary/JVM-erasure constituent; these bounds retain the rest of the checked Kotlin type for
    /// reified inline operations without introducing a lookup-visible synthetic classifier.
    pub additional_bounds: Box<[ResolvedTy]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirVarargElement {
    pub value: FirExprId,
    pub spread: bool,
    pub conversion: Option<FirConversion>,
}

/// One checker-typed element of a compiler-provided array construction. This is not a call-site
/// vararg mapping: the selected synthetic declaration has already been replaced by its semantic
/// array construction before FIR is published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirArrayElement {
    pub value: FirExprId,
    pub spread: bool,
    pub conversion: Option<FirConversion>,
}

/// The final argument-to-parameter decision for one selected call. Source argument order is retained
/// by the surrounding slice; `parameter` is the selected declaration's stable parameter ordinal.
/// A vararg with several source contributions is represented by consecutive `Vararg` entries for
/// the same parameter. This keeps evaluation order explicit without retaining source argument IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirCallArgument {
    Expression {
        parameter: u32,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    Default {
        parameter: u32,
        origin: OriginId,
    },
    Vararg {
        parameter: u32,
        origin: OriginId,
        elements: Box<[FirVarargElement]>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirIntrinsic {
    /// The exact `kotlin.assert` declaration selected by resolution. Its operands remain lazy until
    /// target realization: a disabled assertion evaluates neither the condition nor its message.
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
    PrimitiveCompare {
        operand: ResolvedTy,
    },
    CoroutineContext,
    /// The selected stdlib coroutine primitive. Its function block is checked as an ordinary
    /// argument, but common lowering must inline that exact checked block against the current
    /// continuation rather than emit a call to the stdlib declaration's intrinsic-only stub.
    SuspendCoroutineUninterceptedOrReturn,
    /// The selected safe coroutine primitive. This is distinct from the unintercepted primitive:
    /// target realization must invoke the block with a one-shot safe, intercepted continuation and
    /// use that continuation's completed value or suspension sentinel as the call result.
    SuspendCoroutine,
    UnsignedToString {
        source: ResolvedTy,
    },
    PrimitiveArrayNew {
        element: ResolvedTy,
    },
}

/// Language-defined callable contributed by a classifier. Its identity is the classifier plus this
/// operation, never the source spelling or a platform method descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirClassifierCallable {
    EnumValues,
    /// The classifier's own implicit `valueOf` member — `E.valueOf(name)`.
    EnumValueOf,
    /// The standard library's top-level `enumValueOf<E>(name)`. It resolves to the same lookup as
    /// the member, but it is a different — and `inline` — declaration, so the two stay apart.
    TopLevelEnumValueOf,
    ArrayConstructor {
        element: ResolvedTy,
    },
    SamConstructor {
        conversion: Box<FirSamConversion>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirCallTarget {
    Module(CallableId),
    External {
        declaration: ExternalCallableId,
        /// Exact dependency declaration supplying inherited defaults. The selected implementation
        /// remains `declaration`; this provider is consulted only for checked omitted arguments.
        default_provider: Option<ExternalCallableId>,
        receiver: Option<ResolvedTy>,
        declared_receiver: Option<ResolvedTy>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        declared_result: Option<ResolvedTy>,
        suspend: bool,
        can_inline: bool,
        inline_plan: Option<Box<FirInlineBodyPlan>>,
        /// Parameter slot occupied by the explicit extension receiver when the selected external
        /// declaration is a member extension. Ordinary members and top-level extensions use `None`.
        extension_receiver_parameter: Option<u32>,
    },
    Intrinsic {
        operation: FirIntrinsic,
        receiver: Option<ResolvedTy>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
    },
    Classifier {
        classifier: crate::types::TypeName,
        operation: FirClassifierCallable,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
    },
    /// A `super`-qualified call: the checker has already picked the exact supertype declaration, so
    /// dispatch is NON-VIRTUAL and no later phase may re-resolve it against the receiver's runtime
    /// class. This is its own target because neither a module nor a dependency callable id can
    /// express "this declaration, bypassing overriding" — the same source declaration is an ordinary
    /// virtual target at every other call site.
    Super {
        owner: crate::types::TypeName,
        /// Classifier whose instance supplies `this` for this selected super dispatch.
        dispatch_owner: crate::types::TypeName,
        /// The selected dispatch instance belongs to an enclosing lexical classifier rather than
        /// the body currently containing the call. Targets such as the JVM must cross that physical
        /// class boundary through an owner-local nonvirtual bridge.
        enclosing_dispatch: bool,
        kind: FirSuperCallKind,
        name: String,
        /// The selected declaration's physical parameters, as its provider published them. They
        /// are parallel to the call's parameters for a source declaration, whose `descriptor` the
        /// target derives from them; a provider-described declaration keeps its own ABI shape.
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        /// The selected owner is an interface, so the call reaches a default method.
        interface: bool,
        /// Provider-owned realization of the already-selected declaration. This must survive FIR:
        /// a dependency may expose a semantic interface member through a receiver-first static
        /// holder, while a source declaration remains ordinary nonvirtual dispatch until its
        /// target backend chooses an output mode.
        realization: crate::libraries::MemberRealization,
        /// Provider-supplied physical descriptor, if any. A SOURCE declaration leaves it empty, and
        /// the target derives it at realization from the declaration's own, unsubstituted
        /// `parameters` and from `physical_result`. It is not a semantic fact FIR may pin.
        descriptor: String,
        /// The declaration's physical result type, which differs from `result` when the selected
        /// declaration erases or boxes (a generic override, a value class).
        physical_result: ResolvedTy,
        /// Stable current-module callable selected for the super declaration. This is the exact
        /// owner of retained checked defaults; dependency declarations leave it unset.
        source: Option<CallableId>,
        source_member: Option<crate::libraries::SourceMember>,
        /// The selected declaration is a `suspend` function: the call is a suspension point, and
        /// a target passes it a continuation.
        suspend: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirSuperCallKind {
    Function,
    PropertyGetter,
    PropertySetter,
}

impl From<CallableId> for FirCallTarget {
    fn from(target: CallableId) -> Self {
        Self::Module(target)
    }
}

impl FirCallTarget {
    pub fn module(&self) -> Option<CallableId> {
        match self {
            Self::Module(target) => Some(*target),
            Self::External { .. }
            | Self::Intrinsic { .. }
            | Self::Classifier { .. }
            | Self::Super { .. } => None,
        }
    }
}

/// A fully selected callable application. There is no name, lookup scope, candidate list, or
/// provider-origin branch left for lowering to interpret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirCall {
    pub target: FirCallTarget,
    pub dispatch_receiver: Option<FirReceiver>,
    pub extension_receiver: Option<FirReceiver>,
    /// Final call-site-applied semantic value-parameter types, excluding receivers. These are the
    /// slots against which `arguments` were checked. A stable module declaration can still spell
    /// an owner parameter such as `T`; a call through `Box<Int>` records `Int` here so lowering does
    /// not reconstruct class substitutions from the receiver or declaration graph.
    pub parameter_types: Box<[ResolvedTy]>,
    pub arguments: Box<[FirCallArgument]>,
    pub substitutions: Box<[FirTypeSubstitution]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalBinding {
    pub(crate) value: LocalValueId,
    pub(crate) ty: ResolvedTy,
    pub(crate) lateinit: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DelegateStorage {
    Local(LocalBinding),
    ClassField(ClassCaptureBinding),
}

impl DelegateStorage {
    pub(crate) const fn ty(self) -> ResolvedTy {
        match self {
            Self::Local(binding) => binding.ty,
            Self::ClassField(binding) => binding.ty,
        }
    }

    pub(crate) const fn local(self) -> Option<LocalBinding> {
        match self {
            Self::Local(binding) => Some(binding),
            Self::ClassField(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalDelegateBinding {
    pub(crate) storage: DelegateStorage,
    pub(crate) property_ty: ResolvedTy,
    pub(crate) get_value: FirDelegateCall,
    pub(crate) set_value: Option<FirDelegateCall>,
    pub(crate) name: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClassCaptureBinding {
    pub(crate) owner: DeclarationId,
    pub(crate) field: u32,
    pub(crate) ty: ResolvedTy,
    pub(crate) shared_cell: bool,
    pub(crate) enclosing_depth: u32,
    /// Receiver-tower coordinate used by member bodies of this classifier when this binding is a
    /// captured receiver. `None` for ordinary captured values/delegates.
    pub(crate) semantic_receiver_depth: Option<u32>,
    pub(crate) receiver_source: Option<ClassReceiverCaptureSource>,
    /// Original classifier field whose closure value this field forwards. A directly captured
    /// value has its own `(owner, field)` identity; a transitive recapture preserves the upstream
    /// identity so independently streamed members never join closure operands by source spelling.
    pub(crate) capture_identity: Option<ClassCaptureIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClassCaptureIdentity {
    pub(crate) owner: TypeName,
    pub(crate) field: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClassReceiverCaptureSource {
    pub(crate) enclosing_depth: u32,
    pub(crate) current: bool,
    pub(crate) depth: u32,
}

/// Checked lexical environment required by ordinary members of a local/anonymous classifier
/// declared inside a retained inline/default FIR fragment. It contains semantic FIR identities and
/// types only; no AST ids, source ranges, names used for lookup, or unresolved types survive.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClassBodyContext {
    pub(crate) values: HashMap<String, ClassCaptureBinding>,
    /// Every captured value keyed by semantic closure identity. `values` is only the nearest
    /// source-name view used for ordinary reads; two forwarded closures may legitimately capture
    /// different shadowed bindings with the same spelling.
    pub(crate) capture_values: Vec<(String, ClassCaptureBinding)>,
    pub(crate) delegates: HashMap<String, LocalDelegateBinding>,
    pub(crate) callables: HashMap<BodyLocalCallableDeclarationId, (u32, LocalCallableId)>,
    pub(crate) receivers: Vec<ClassCaptureBinding>,
    pub(crate) enclosing_property: Option<PropertyId>,
}

impl ClassBodyContext {
    pub(crate) fn record_value(&mut self, name: String, binding: ClassCaptureBinding) {
        self.capture_values.push((name.clone(), binding));
        self.values.insert(name, binding);
    }

    pub(crate) fn merge(&mut self, context: Self) {
        self.values.extend(context.values);
        self.capture_values.extend(context.capture_values);
        self.delegates.extend(context.delegates);
        self.callables.extend(context.callables);
        self.receivers.extend(context.receivers);
        self.enclosing_property = context.enclosing_property.or(self.enclosing_property);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyDispatch {
    Ordinary,
    /// Non-virtual dispatch selected through `super`. This says nothing about physical storage:
    /// the target backend still decides whether the exact property is a field or an accessor.
    Super {
        owner: TypeName,
        interface: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyTarget {
    Module(PropertyId),
    External {
        property: super::ExternalPropertyId,
        receiver: Option<ResolvedTy>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        /// Parameter slot occupied by the extension receiver when this accessor belongs to a
        /// member-extension property. Ordinary members and top-level extensions use `None`.
        extension_receiver_parameter: Option<u32>,
        dispatch: FirPropertyDispatch,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyReferenceTarget {
    Module(PropertyId),
    /// A source-module property after callable-reference selection and generic specialization.
    /// `property` is the stable declaration identity; the remaining fields are the final callable
    /// view consumed by an adapted function value. Lowering must not reconstruct these types from
    /// a symbolic declaration signature.
    SpecializedModule {
        property: PropertyId,
        /// Accessor declaration spellings carried by the provider-selected property candidate.
        /// These are declaration facts, not names reconstructed from the property spelling by a
        /// later backend pass.
        getter_name: Box<str>,
        setter_name: Option<Box<str>>,
        receiver: Option<ResolvedTy>,
        extension_receiver: bool,
        property_type: ResolvedTy,
        /// The extension receiver and property type the declaration itself declares. Its
        /// accessors are compiled once against these, so the reference's specialized receiver
        /// and result cross into and out of them.
        declared_receiver: Option<ResolvedTy>,
        declared_property_type: ResolvedTy,
    },
    Classifier {
        owner: TypeName,
        property: FirClassifierProperty,
        property_type: ResolvedTy,
    },
    /// A dependency property after callable-reference selection.
    ///
    /// It carries no name of its own. The property's is the provider's, decoded from the
    /// declaration's metadata and reachable through `getter`'s [`ExternalPropertyId`]. A copy here
    /// was the REFERENCE SITE's spelling, which is a different fact — a lookup may reach a
    /// declaration under an import alias — and the accessor cannot stand in for it either, its name
    /// being a physical call target a JVM realization may rename or value-class-mangle.
    External {
        reflection_owner: Option<ResolvedTy>,
        getter: Box<FirPropertyTarget>,
        setter: Option<Box<FirPropertyTarget>>,
        extension_receiver: bool,
        property_type: ResolvedTy,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirClassifierProperty {
    EnumEntries,
}

impl From<PropertyId> for FirPropertyReferenceTarget {
    fn from(target: PropertyId) -> Self {
        Self::Module(target)
    }
}

impl FirPropertyReferenceTarget {
    pub const fn module(&self) -> Option<PropertyId> {
        match self {
            Self::Module(target) => Some(*target),
            Self::SpecializedModule { property, .. } => Some(*property),
            Self::Classifier { .. } | Self::External { .. } => None,
        }
    }

    fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module(_) | Self::SpecializedModule { .. } | Self::Classifier { .. } => 0,
            Self::External { getter, setter, .. } => {
                getter.storage_payload_bytes()
                    + setter
                        .as_deref()
                        .map_or(0, FirPropertyTarget::storage_payload_bytes)
            }
        }
    }
}

impl From<PropertyId> for FirPropertyTarget {
    fn from(target: PropertyId) -> Self {
        Self::Module(target)
    }
}

impl FirPropertyTarget {
    pub fn module(&self) -> Option<PropertyId> {
        match self {
            Self::Module(target) => Some(*target),
            Self::External { .. } => None,
        }
    }

    fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module(_) => 0,
            Self::External {
                receiver,
                parameters,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
        }
    }
}

/// A constructor declaration identity after overload selection. Module constructors use their
/// stable callable identity. Dependency constructors retain a backend-neutral classifier and
/// semantic parameter signature; a backend maps that exact declaration to physical linkage.
/// Compact declaration default retained only for an annotation-construction plan. Ordinary source
/// defaults remain checked retained bodies; dependency metadata contributes only these closed values.
#[derive(Clone, Debug, PartialEq)]
pub enum FirAnnotationDefaultValue {
    Constant(FirConstant),
    Singleton(TypeName),
}

/// Complete checked declaration shape needed to realize an annotation value. This is Kotlin
/// semantics rather than a platform implementation detail: a backend may allocate a concrete value,
/// synthesize an implementation, or use a native annotation representation without new lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct FirAnnotationConstruction {
    pub members: Box<[(Box<str>, ResolvedTy)]>,
    pub defaults: Box<[Option<FirAnnotationDefaultValue>]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FirConstructorTarget {
    Module {
        declaration: CallableId,
        annotation: Option<Box<FirAnnotationConstruction>>,
    },
    External {
        declaration: ExternalCallableId,
        classifier: TypeName,
        parameters: Box<[ResolvedTy]>,
        annotation: Option<Box<FirAnnotationConstruction>>,
    },
}

impl FirConstructorTarget {
    fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module { annotation, .. } => annotation.as_ref().map_or(0, |annotation| {
                annotation.members.len() * std::mem::size_of::<(Box<str>, ResolvedTy)>()
                    + annotation
                        .members
                        .iter()
                        .map(|(name, _)| name.len())
                        .sum::<usize>()
                    + annotation.defaults.len()
                        * std::mem::size_of::<Option<FirAnnotationDefaultValue>>()
            }),
            Self::External {
                parameters,
                annotation,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + annotation.as_ref().map_or(0, |annotation| {
                        annotation.members.len() * std::mem::size_of::<(Box<str>, ResolvedTy)>()
                            + annotation
                                .members
                                .iter()
                                .map(|(name, _)| name.len())
                                .sum::<usize>()
                            + annotation.defaults.len()
                                * std::mem::size_of::<Option<FirAnnotationDefaultValue>>()
                    })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FirConstructorCall {
    pub target: FirConstructorTarget,
    /// Leading classifier context parameters already present in `parameter_types` and
    /// `arguments`. They are physical constructor inputs but do not participate in source value-
    /// parameter default-mask ordinals.
    pub context_parameter_count: u32,
    /// Semantic type of the compiler-supplied enclosing-instance parameter. This is separate from
    /// source value parameters, so default-mask ordinals and source argument mapping remain stable.
    /// It is present exactly when `outer_receiver` supplies an `inner` constructor receiver.
    pub outer_parameter: Option<ResolvedTy>,
    pub outer_receiver: Option<FirReceiver>,
    /// Compiler capture operands that must cross a separately streamed body boundary. `None`
    /// means the construction is in the FIR body that declares the local classifier, whose
    /// checked `LocalDeclaration` supplies the same capture prefix.
    pub external_capture_arguments: Option<Box<[FirConstructorCaptureArgument]>>,
    /// Final call-site-applied source value-parameter types. Constructor lowering consumes these
    /// directly; the stable declaration signature remains available separately for target ABI.
    pub parameter_types: Box<[ResolvedTy]>,
    pub arguments: Box<[FirCallArgument]>,
    pub substitutions: Box<[FirTypeSubstitution]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirPluginOperand {
    pub value: FirExprId,
    pub conversion: Option<FirConversion>,
}

impl FirConstructorCall {
    fn storage_payload_bytes(&self) -> usize {
        self.target.storage_payload_bytes()
            + self.parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            + self.arguments.len() * std::mem::size_of::<FirCallArgument>()
            + self
                .external_capture_arguments
                .as_ref()
                .map_or(0, |arguments| {
                    arguments.len() * std::mem::size_of::<FirConstructorCaptureArgument>()
                })
            + self
                .arguments
                .iter()
                .map(FirCallArgument::storage_payload_bytes)
                .sum::<usize>()
            + self.substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
    }
}

/// One checked compiler-capture operand for a local-class construction outside the FIR body that
/// declares the class. `shared_cell_holder` distinguishes the invisible physical cell operand from
/// an ordinary Kotlin value read without exposing any backend storage type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirConstructorCaptureArgument {
    pub value: FirExprId,
    pub shared_cell_holder: bool,
}

impl FirCall {
    fn storage_payload_bytes(&self) -> usize {
        let external_parameters = match &self.target {
            FirCallTarget::Module(_) => 0,
            FirCallTarget::External {
                receiver,
                declared_receiver,
                parameters,
                declared_result,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
                    + usize::from(declared_receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
                    + usize::from(declared_result.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            FirCallTarget::Super {
                parameters,
                name,
                descriptor,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>() + name.len() + descriptor.len()
            }
            FirCallTarget::Intrinsic {
                receiver,
                parameters,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            FirCallTarget::Classifier { parameters, .. } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
            }
        };
        external_parameters
            + self.parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            + self.arguments.len() * std::mem::size_of::<FirCallArgument>()
            + self
                .arguments
                .iter()
                .map(FirCallArgument::storage_payload_bytes)
                .sum::<usize>()
            + self.substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
    }
}

impl FirCallArgument {
    fn storage_payload_bytes(&self) -> usize {
        match self {
            FirCallArgument::Vararg { elements, .. } => {
                elements.len() * std::mem::size_of::<FirVarargElement>()
            }
            FirCallArgument::Expression { .. } | FirCallArgument::Default { .. } => 0,
        }
    }
}

impl FirReferenceAdaptation {
    fn storage_payload_bytes(&self) -> usize {
        self.arguments.len() * std::mem::size_of::<FirAdaptedReferenceArgument>()
            + self
                .arguments
                .iter()
                .map(|argument| match argument {
                    FirAdaptedReferenceArgument::Vararg { values, .. } => {
                        values.len() * std::mem::size_of::<u32>()
                    }
                    FirAdaptedReferenceArgument::Value(_)
                    | FirAdaptedReferenceArgument::Default => 0,
                })
                .sum::<usize>()
            + self.parameter_types.len() * std::mem::size_of::<ResolvedTy>()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirJumpKind {
    /// Return to the current callable body (`target_depth == 0`) or through that many enclosing
    /// inline-lambda bodies. The checker resolves the source label once; lowering never sees it.
    Return { target_depth: u32 },
    /// Break from a loop in this FIR body (`target_depth == 0`) or through that many enclosing
    /// inline-spliced lambda bodies. A non-inline body can never publish a nonzero depth.
    Break { target_depth: u32 },
    /// Continue a loop in this FIR body or an enclosing inline-spliced lambda body.
    Continue { target_depth: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirControlTargetKind {
    Body(BodyOwnerId),
    Loop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirControlTarget {
    pub origin: OriginId,
    pub kind: FirControlTargetKind,
}

/// One checked interface-delegate value evaluated at an anonymous-object construction site. The
/// stable delegation ordinal binds it to the resolved classifier plan; the value itself is ordinary
/// checked FIR, so lowering performs no lexical lookup or source reconstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirInterfaceDelegateArgument {
    pub delegation: u32,
    pub value: FirExprId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirAnonymousObject {
    pub declaration: DeclarationId,
    pub captures: Box<[FirLocalClassCapture]>,
    pub delegate_arguments: Box<[FirInterfaceDelegateArgument]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLocalCallableRef {
    /// Zero denotes the current FIR body; one denotes its immediately enclosing body.
    pub body_depth: u32,
    pub callable: LocalCallableId,
    /// Stable declaration-stream identity. The owner prevents ordinals from distinct bounded
    /// source units colliding while a file is streamed through common lowering.
    pub declaration: Option<BodyLocalCallableDeclarationId>,
    /// Present only when the target lives outside the independently streamed body containing this
    /// reference. Each expression is the checker-published physical closure argument in target ABI
    /// order; an empty slice therefore distinguishes an external capture-free target from an
    /// ordinary nested-body reference.
    pub external_capture_arguments: Option<Box<[FirExprId]>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirTypeOperation {
    Is,
    NotIs,
    Cast,
    SafeCast,
    NotNullAssertion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirUnaryOperation {
    Negate,
    BooleanNot,
    Identity,
    Increment,
    Decrement,
    BitwiseNot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirBinaryOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    BooleanAnd,
    BooleanOr,
    ReferentialEqual,
    ReferentialNotEqual,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    ShiftLeft,
    ShiftRight,
    UnsignedShiftRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirBuiltinIterableKind {
    Array,
    String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirIndexedAccessKind {
    Array,
    String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirCallableReferenceBinding {
    Static,
    Bound,
    Unbound,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FirCallableReferenceTarget {
    Module(CallableId),
    /// Reference to an exact compiler-supplied Kotlin array-factory declaration. The provider tag
    /// proves the declaration identity; these checked semantic types are the complete construction
    /// plan, so common lowering never reopens lookup or asks a backend for a nonexistent method.
    ArrayFactory {
        operation: crate::types::ArrayFactoryKind,
        array_type: ResolvedTy,
        element_type: ResolvedTy,
        parameters: Box<[ResolvedTy]>,
    },
    /// A reference to a selected CONSTRUCTOR (`::A`). It is not an ordinary callable reference: the
    /// adapter must construct rather than call, so both the operation and declaration provenance
    /// have to survive to lowering.
    Constructor {
        target: FirConstructorTarget,
        classifier: crate::types::TypeName,
        /// Inner-class outer instance type. `binding` says whether it is captured or supplied as
        /// the leading unbound reference parameter; ordinary/nested constructors leave it absent.
        outer: Option<ResolvedTy>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
    },
    External {
        declaration: ExternalCallableId,
        default_provider: Option<ExternalCallableId>,
        receiver: Option<ResolvedTy>,
        /// The provider's declared extension receiver before call-site substitution, exactly as an
        /// ordinary external call records it.
        declared_receiver: Option<ResolvedTy>,
        extension_receiver: bool,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        /// The provider's declared result before call-site substitution.
        declared_result: Option<ResolvedTy>,
        /// The declaration itself is `suspend`; a suspend-converted reference to an ordinary
        /// function is not.
        suspend: bool,
    },
    Classifier {
        classifier: crate::types::TypeName,
        operation: FirClassifierCallable,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
    },
}

impl From<CallableId> for FirCallableReferenceTarget {
    fn from(target: CallableId) -> Self {
        Self::Module(target)
    }
}

impl FirCallableReferenceTarget {
    pub const fn module(&self) -> Option<CallableId> {
        match self {
            Self::Module(target) => Some(*target),
            Self::ArrayFactory { .. }
            | Self::Constructor { .. }
            | Self::External { .. }
            | Self::Classifier { .. } => None,
        }
    }

    fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module(_) => 0,
            Self::ArrayFactory { parameters, .. } => {
                (2 + parameters.len()) * std::mem::size_of::<ResolvedTy>()
            }
            Self::Constructor {
                target,
                outer,
                parameters,
                ..
            } => {
                target.storage_payload_bytes()
                    + parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(outer.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            Self::External {
                receiver,
                parameters,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            Self::Classifier { parameters, .. } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirAdaptedReferenceArgument {
    Value(u32),
    Default,
    Vararg {
        values: Box<[u32]>,
        whole_array: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirReferenceAdaptation {
    /// Target-parameter plan in declaration order. Value ordinals address the adapter function's
    /// parameters; defaults and varargs are final checker decisions.
    pub arguments: Box<[FirAdaptedReferenceArgument]>,
    pub parameter_types: Box<[ResolvedTy]>,
    pub result_type: ResolvedTy,
    pub suspend_conversion: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FirConstant {
    Int(i64),
    Long(i64),
    UInt(i64),
    ULong(i64),
    Double(f64),
    Float(f32),
    Boolean(bool),
    String(KtString),
    Char(u16),
    Null,
}

/// Checked, source-oriented expression shapes. Operations whose Kotlin meaning depends on lookup or
/// conventions carry a [`FirCall`]; syntax spellings never select behavior after this point.
#[derive(Clone, Debug, PartialEq)]
pub enum FirExprKind {
    Constant(FirConstant),
    AnnotationArray(Box<[FirExprId]>),
    ArrayLiteral {
        array_type: ResolvedTy,
        elements: Box<[FirArrayElement]>,
    },
    /// A checker-selected sized array construction. `initializer`, when present, is the already
    /// checked `(Int) -> element_type` function value. Common lowering may synthesize allocation
    /// and fill control flow, but performs no callable lookup or type inference.
    ArrayConstruction {
        array_type: ResolvedTy,
        element_type: ResolvedTy,
        size: FirExprId,
        size_conversion: Option<FirConversion>,
        initializer: Option<FirExprId>,
    },
    /// Runtime receiver already selected from the lexical receiver tower. `depth` is nearest-first;
    /// `current` distinguishes the active `this` from an enclosing captured receiver at that rung.
    ImplicitReceiver {
        current: bool,
        depth: u32,
    },
    /// A receiver reached by following the semantic enclosing-instance edges of `inner`
    /// classifiers from the current dispatch receiver. The declarations are ordered from the
    /// current classifier outwards and name the exact edges selected by resolution; no field name,
    /// field ordinal, or other backend storage decision is part of checked FIR.
    EnclosingReceiver {
        path: Box<[DeclarationId]>,
    },
    CapturedImplicitReceiver {
        enclosing_depth: u32,
        current: bool,
        depth: u32,
        path: Box<[DeclarationId]>,
    },
    /// Singleton declaration selected by ordinary resolution. Target storage is not part of FIR.
    SingletonValue {
        classifier: TypeName,
    },
    /// Enum entry selected during resolution. The classifier and declaration-owned entry name are
    /// the stable semantic identity; the ordinal remains useful to source-order analyses.
    EnumEntry {
        classifier: TypeName,
        ordinal: u32,
        name: Box<str>,
    },
    ClassifierPropertyRead {
        owner: TypeName,
        property: FirClassifierProperty,
    },
    ValueRead(LocalValueId),
    CapturedValueRead {
        enclosing_depth: u32,
        source: LocalValueId,
    },
    /// A checked read of language-level `lateinit` local storage. The operand identifies the
    /// already-selected lexical or captured value; the source name is retained only for the
    /// required uninitialized-property exception, never for lookup.
    LateinitRead {
        value: FirExprId,
        name: Box<str>,
    },
    /// Read a checker-selected storage field of the current classifier-like declaration. Enum
    /// entries are stable owners here as well: their anonymous subclass is realized only after
    /// FIR, but lowering never has to reconstruct that ownership from the enclosing enum.
    ClassStorageRead {
        owner: DeclarationId,
        field: u32,
    },
    /// Read a local-class capture from the constructor prefix. Delegation and defaults run before
    /// its field is readable from `this`, so this source is explicit in FIR.
    ConstructorCaptureRead {
        owner: DeclarationId,
        field: u32,
        shared_cell: bool,
        /// Where the value is: the prefix parameter itself, or this body's capture of it.
        site: super::FirConstructorCaptureSite,
    },
    /// Read a language-level class context from the constructor's semantic prefix. `parameter` is
    /// its declaration ordinal, not a storage-field or backend ABI identity.
    ConstructorContextRead {
        owner: DeclarationId,
        parameter: u32,
    },
    /// Read the element of a shared mutable-local cell stored in a local classifier capture field.
    ClassStorageSharedRead {
        owner: DeclarationId,
        field: u32,
    },
    /// Write a shared mutable-local cell in this or an enclosing local classifier. Its synthetic
    /// field stays immutable; `element` is the declared cell type, not inferred from the RHS.
    ClassStorageSharedWrite {
        owner: DeclarationId,
        enclosing_depth: u32,
        field: u32,
        element: ResolvedTy,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    /// Write through a shared capture in the constructor prefix before its field is readable from
    /// `this`; `element` is the captured cell's exact declared type.
    ConstructorCaptureSharedWrite {
        owner: DeclarationId,
        field: u32,
        element: ResolvedTy,
        value: FirExprId,
        conversion: Option<FirConversion>,
        /// Where the cell is: the prefix parameter itself, or this body's capture of it.
        site: super::FirConstructorCaptureSite,
    },
    /// Read a generated capture field through one or more enclosing local-class instances.
    EnclosingClassStorageRead {
        owner: DeclarationId,
        enclosing_depth: u32,
        field: u32,
        shared_cell: bool,
    },
    /// Read a local-class capture through a receiver captured by a lifted local callable. `path`
    /// is the checked inner-class chain from `receiver` to the capture `owner`.
    CapturedClassStorageRead {
        owner: DeclarationId,
        receiver: FirExprId,
        path: Box<[DeclarationId]>,
        field: u32,
        shared_cell: bool,
    },
    /// Write the shared cell stored in a checked local-class capture field through an explicitly
    /// captured class receiver. `element` is the captured cell's exact declared type.
    CapturedClassStorageSharedWrite {
        owner: DeclarationId,
        receiver: FirExprId,
        path: Box<[DeclarationId]>,
        field: u32,
        element: ResolvedTy,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    CapturedValueWrite {
        enclosing_depth: u32,
        source: LocalValueId,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    ValueWrite {
        target: LocalValueId,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    PropertyRead {
        target: FirPropertyTarget,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        context_arguments: Box<[FirReceiver]>,
        substitutions: Box<[FirTypeSubstitution]>,
    },
    PropertyWrite {
        target: FirPropertyTarget,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        context_arguments: Box<[FirReceiver]>,
        value: FirExprId,
        conversion: Option<FirConversion>,
        substitutions: Box<[FirTypeSubstitution]>,
    },
    /// The accessor soft-keyword `field`, bound by the checker to its stable property declaration.
    /// Raw read of a `lateinit` backing field for `::v.isInitialized`. Distinct from
    /// [`Self::BackingFieldRead`], whose realization inserts the uninitialized-access THROW — the
    /// initialization test must observe the raw `null`, not raise.
    LateinitFieldRead {
        target: PropertyId,
        dispatch_receiver: Option<FirReceiver>,
    },
    BackingFieldRead {
        target: PropertyId,
        dispatch_receiver: Option<FirReceiver>,
    },
    BackingFieldWrite {
        target: PropertyId,
        dispatch_receiver: Option<FirReceiver>,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    /// Exact implementation plan attached by a frontend plugin after ordinary declaration and
    /// overload selection. The operation name is private to that plugin; operands already carry
    /// their checked target conversions and resolved classifier data uses stable identities.
    PluginExpression {
        plugin: &'static str,
        operation: &'static str,
        data: Box<[TypeName]>,
        types: Box<[ResolvedTy]>,
        operands: Box<[FirPluginOperand]>,
    },
    Call(FirCall),
    ConstructorCall(FirConstructorCall),
    AnonymousObject(FirAnonymousObject),
    LocalCall {
        target: FirLocalCallableRef,
        extension_receiver: Option<FirReceiver>,
        arguments: Box<[FirCallArgument]>,
    },
    /// Invocation of an already-typed function value. The callable expression and final parameter
    /// types are semantic inputs; lowering only chooses their backend representation.
    FunctionInvoke {
        callee: FirExprId,
        context_arguments: Box<[FirReceiver]>,
        arguments: Box<[FirCallArgument]>,
        parameter_types: Box<[ResolvedTy]>,
        result: ResolvedTy,
        suspend: bool,
    },
    /// A receiver-function value with its extension receiver bound by `receiver.(callable)`.
    /// The target invocation shape and exact receiver parameter are checker-owned; common lowering
    /// only synthesizes the forwarding closure described here.
    ExtensionFunctionBinding {
        receiver: FirReceiver,
        callable: FirExprId,
        target_parameters: Box<[ResolvedTy]>,
        receiver_parameter: u32,
        target_result: ResolvedTy,
        suspend: bool,
    },
    /// Bound reference to the selected `invoke` operation of a function value. The captured value
    /// and both final callable shapes are sufficient to synthesize a forwarding closure; no
    /// classifier/member lookup is permitted after this point.
    FunctionInvokeReference {
        callee: FirExprId,
        target_parameters: Box<[ResolvedTy]>,
        target_result: ResolvedTy,
        target_suspend: bool,
        reference_parameters: Box<[ResolvedTy]>,
        reference_result: ResolvedTy,
        suspend: bool,
    },
    ComparisonCall {
        operation: FirBinaryOperation,
        call: FirCall,
    },
    ContainmentCall {
        call: FirCall,
        negated: bool,
    },
    CallableReference {
        target: FirCallableReferenceTarget,
        /// Exact callable shape selected by the checker. This remains a function type even when
        /// the expression's public type is a reflective `KFunction` classifier.
        function_type: ResolvedTy,
        reflective: bool,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        substitutions: Box<[FirTypeSubstitution]>,
        adaptation: Option<Box<FirReferenceAdaptation>>,
    },
    LocalCallableReference {
        target: FirLocalCallableRef,
        function_type: ResolvedTy,
        reflective: bool,
        extension_receiver: Option<FirReceiver>,
        adaptation: Option<Box<FirReferenceAdaptation>>,
    },
    /// Reflection value supplied to a checked local delegated-property convention call.
    LocalPropertyReference {
        name: Box<str>,
        property_type: ResolvedTy,
    },
    PropertyReference {
        target: FirPropertyReferenceTarget,
        /// Exact getter/invocation shape, independent of the nominal `KProperty` expression type.
        function_type: ResolvedTy,
        reflective: bool,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        mutable: bool,
        substitutions: Box<[FirTypeSubstitution]>,
        adaptation: Option<Box<FirReferenceAdaptation>>,
    },
    ClassLiteral {
        classifier: Option<ResolvedTy>,
        value: Option<FirExprId>,
    },
    TypeOperation {
        operation: FirTypeOperation,
        operand: FirExprId,
        target: ResolvedTy,
    },
    /// An implicit adaptation selected by checking. The child retains its source type while this
    /// node publishes the exact assignment/data-flow boundary consumed by common lowering.
    ImplicitConversion {
        value: FirExprId,
        conversion: FirConversion,
    },
    Unary {
        operation: FirUnaryOperation,
        operand: FirExprId,
    },
    Binary {
        operation: FirBinaryOperation,
        lhs: FirExprId,
        rhs: FirExprId,
    },
    /// Kotlin structural equality between one nullable primitive wrapper and its non-null primitive.
    /// The wrapper is null-tested and unboxed before primitive comparison; this is a frontend-selected
    /// semantic operation, not a physical-type guess left to a backend.
    NullablePrimitiveComparison {
        operation: FirBinaryOperation,
        nullable: FirExprId,
        primitive: FirExprId,
        primitive_ty: ResolvedTy,
    },
    /// Kotlin numeric equality between two nullable scalar values. Both operands are evaluated,
    /// nulls compare structurally, and two present values are unboxed and promoted to `comparison`
    /// before IEEE primitive equality. The checker fixes every type; lowering only expands control
    /// flow and conversions.
    NullableNumericComparison {
        operation: FirBinaryOperation,
        lhs: FirExprId,
        rhs: FirExprId,
        lhs_primitive: ResolvedTy,
        rhs_primitive: ResolvedTy,
        comparison: ResolvedTy,
    },
    Range {
        operation: FirRangeOperation,
        start: FirExprId,
        start_type: ResolvedTy,
        end: FirExprId,
        end_type: ResolvedTy,
    },
    InRange {
        operation: FirRangeOperation,
        /// Exact primitive comparison representation selected by the frontend. This is independent
        /// of counted-loop support: `Double`/`Float` ranges are valid membership comparisons.
        comparison: ResolvedTy,
        value: FirExprId,
        start: FirExprId,
        end: FirExprId,
        negated: bool,
    },
    IndexedRead {
        kind: FirIndexedAccessKind,
        receiver: FirExprId,
        indices: Box<[FirConvertedValue]>,
    },
    IndexedWrite {
        receiver: FirExprId,
        indices: Box<[FirConvertedValue]>,
        value: FirExprId,
        conversion: Option<FirConversion>,
    },
    /// Null-guarded member/extension selection. The nested selector owns the receiver exactly once;
    /// lowering guards that selected receiver and never repeats lookup or overload selection.
    SafeCall {
        receiver: FirReceiver,
        selector: FirExprId,
    },
    Elvis {
        lhs: FirExprId,
        rhs: FirExprId,
    },
    StringTemplate(Box<[FirExprId]>),
    Throw(FirExprId),
    Jump {
        kind: FirJumpKind,
        target: ControlTargetId,
        value: Option<FirExprId>,
    },
    Lambda {
        callable: LocalCallableId,
        body: Box<FirBody>,
    },
    Try {
        body: FirExprId,
        catches: Box<[FirCatch]>,
        finally: Option<FirExprId>,
    },
    Conditional {
        condition: FirExprId,
        then_branch: FirExprId,
        then_conversion: Option<FirConversion>,
        else_branch: FirExprId,
        else_conversion: Option<FirConversion>,
    },
    When {
        subject: Option<FirExprId>,
        branches: Box<[FirWhenBranch]>,
    },
    Block {
        statements: Box<[FirStatementId]>,
        result: Option<FirExprId>,
    },
}

impl FirExprKind {
    fn storage_payload_bytes(&self) -> usize {
        match self {
            FirExprKind::Constant(FirConstant::String(value)) => value.len_utf16() * 2,
            FirExprKind::Constant(
                FirConstant::Int(_)
                | FirConstant::Long(_)
                | FirConstant::UInt(_)
                | FirConstant::ULong(_)
                | FirConstant::Double(_)
                | FirConstant::Float(_)
                | FirConstant::Boolean(_)
                | FirConstant::Char(_)
                | FirConstant::Null,
            )
            | FirExprKind::ImplicitReceiver { .. }
            | FirExprKind::SingletonValue { .. }
            | FirExprKind::EnumEntry { .. }
            | FirExprKind::ClassifierPropertyRead { .. }
            | FirExprKind::ValueRead(_)
            | FirExprKind::CapturedValueRead { .. }
            | FirExprKind::ClassStorageRead { .. }
            | FirExprKind::ConstructorCaptureRead { .. }
            | FirExprKind::ConstructorContextRead { .. }
            | FirExprKind::ClassStorageSharedRead { .. }
            | FirExprKind::ClassStorageSharedWrite { .. }
            | FirExprKind::ConstructorCaptureSharedWrite { .. }
            | FirExprKind::EnclosingClassStorageRead { .. }
            | FirExprKind::LateinitFieldRead { .. }
            | FirExprKind::CapturedValueWrite { .. }
            | FirExprKind::ValueWrite { .. }
            | FirExprKind::TypeOperation { .. }
            | FirExprKind::ImplicitConversion { .. }
            | FirExprKind::Unary { .. }
            | FirExprKind::Binary { .. }
            | FirExprKind::NullablePrimitiveComparison { .. }
            | FirExprKind::NullableNumericComparison { .. }
            | FirExprKind::Range { .. }
            | FirExprKind::InRange { .. }
            | FirExprKind::SafeCall { .. }
            | FirExprKind::Elvis { .. }
            | FirExprKind::Throw(_)
            | FirExprKind::Jump { .. }
            | FirExprKind::Conditional { .. } => 0,
            FirExprKind::LateinitRead { name, .. } => name.len(),
            FirExprKind::EnclosingReceiver { path }
            | FirExprKind::CapturedImplicitReceiver { path, .. }
            | FirExprKind::CapturedClassStorageRead { path, .. }
            | FirExprKind::CapturedClassStorageSharedWrite { path, .. } => {
                path.len() * std::mem::size_of::<DeclarationId>()
            }
            FirExprKind::ClassLiteral { .. } => 0,
            FirExprKind::LocalPropertyReference { name, .. } => name.len(),
            FirExprKind::IndexedRead { indices, .. }
            | FirExprKind::IndexedWrite { indices, .. } => {
                indices.len() * std::mem::size_of::<FirConvertedValue>()
            }
            FirExprKind::AnnotationArray(expressions)
            | FirExprKind::StringTemplate(expressions) => {
                expressions.len() * std::mem::size_of::<FirExprId>()
            }
            FirExprKind::ArrayLiteral { elements, .. } => {
                elements.len() * std::mem::size_of::<FirArrayElement>()
            }
            FirExprKind::ArrayConstruction { .. } => 0,
            FirExprKind::PropertyRead {
                target,
                context_arguments,
                substitutions,
                ..
            }
            | FirExprKind::PropertyWrite {
                target,
                context_arguments,
                substitutions,
                ..
            } => {
                target.storage_payload_bytes()
                    + context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
            }
            FirExprKind::BackingFieldRead { .. } | FirExprKind::BackingFieldWrite { .. } => 0,
            FirExprKind::PluginExpression {
                data,
                types,
                operands,
                ..
            } => {
                data.len() * std::mem::size_of::<TypeName>()
                    + types.len() * std::mem::size_of::<ResolvedTy>()
                    + operands.len() * std::mem::size_of::<FirPluginOperand>()
            }
            FirExprKind::FunctionInvokeReference {
                target_parameters,
                reference_parameters,
                ..
            } => {
                (target_parameters.len() + reference_parameters.len())
                    * std::mem::size_of::<ResolvedTy>()
            }
            FirExprKind::CallableReference {
                target,
                substitutions,
                adaptation,
                ..
            } => {
                target.storage_payload_bytes()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
                    + adaptation.as_deref().map_or(0, |adaptation| {
                        std::mem::size_of::<FirReferenceAdaptation>()
                            + adaptation.storage_payload_bytes()
                    })
            }
            FirExprKind::PropertyReference {
                target,
                substitutions,
                adaptation,
                ..
            } => {
                target.storage_payload_bytes()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
                    + adaptation.as_deref().map_or(0, |adaptation| {
                        std::mem::size_of::<FirReferenceAdaptation>()
                            + adaptation.storage_payload_bytes()
                    })
            }
            FirExprKind::LocalCallableReference { adaptation, .. } => {
                adaptation.as_deref().map_or(0, |adaptation| {
                    std::mem::size_of::<FirReferenceAdaptation>()
                        + adaptation.storage_payload_bytes()
                })
            }
            FirExprKind::Call(call) => call.storage_payload_bytes(),
            FirExprKind::ConstructorCall(call) => call.storage_payload_bytes(),
            FirExprKind::AnonymousObject(object) => {
                object.captures.len() * std::mem::size_of::<FirLocalClassCapture>()
                    + object
                        .captures
                        .iter()
                        .map(|capture| capture.name.len() + capture.source.storage_payload_bytes())
                        .sum::<usize>()
                    + object.delegate_arguments.len()
                        * std::mem::size_of::<FirInterfaceDelegateArgument>()
            }
            FirExprKind::LocalCall { arguments, .. } => {
                arguments.len() * std::mem::size_of::<FirCallArgument>()
                    + arguments
                        .iter()
                        .map(FirCallArgument::storage_payload_bytes)
                        .sum::<usize>()
            }
            FirExprKind::FunctionInvoke {
                context_arguments,
                arguments,
                parameter_types,
                ..
            } => {
                context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + arguments.len() * std::mem::size_of::<FirCallArgument>()
                    + arguments
                        .iter()
                        .map(FirCallArgument::storage_payload_bytes)
                        .sum::<usize>()
                    + parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            }
            FirExprKind::ExtensionFunctionBinding {
                target_parameters, ..
            } => target_parameters.len() * std::mem::size_of::<ResolvedTy>(),
            FirExprKind::ComparisonCall { call, .. }
            | FirExprKind::ContainmentCall { call, .. } => call.storage_payload_bytes(),
            FirExprKind::Lambda { body, .. } => {
                std::mem::size_of::<FirBody>() + body.storage_payload_bytes()
            }
            FirExprKind::Try { catches, .. } => catches.len() * std::mem::size_of::<FirCatch>(),
            FirExprKind::When { branches, .. } => {
                branches.len() * std::mem::size_of::<FirWhenBranch>()
                    + branches
                        .iter()
                        .map(|branch| {
                            branch.conditions.len() * std::mem::size_of::<FirWhenCondition>()
                        })
                        .sum::<usize>()
            }
            FirExprKind::Block { statements, .. } => {
                statements.len() * std::mem::size_of::<FirStatementId>()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FirExpr {
    pub origin: OriginId,
    pub ty: ResolvedTy,
    pub kind: FirExprKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirCatch {
    pub origin: OriginId,
    pub parameter: LocalValueId,
    pub parameter_ty: ResolvedTy,
    pub body: FirExprId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirWhenBranch {
    pub origin: OriginId,
    pub conditions: Box<[FirWhenCondition]>,
    pub guard: Option<FirExprId>,
    pub result: FirExprId,
}

/// A `when` condition after the checker has distinguished value patterns from predicates that
/// already consume the subject (`is`/`!is` and `in`/`!in`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirWhenCondition {
    SubjectEquals(FirExprId),
    Predicate(FirExprId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FirLoopHeader {
    While {
        condition: FirExprId,
    },
    DoWhile {
        condition: FirExprId,
    },
    /// `unsigned_compare` is the comparison resolution selected for an unsigned counter.
    Range {
        variable: LocalValueId,
        counter: FirRangeCounterKind,
        operation: FirRangeOperation,
        start: FirExprId,
        end: FirExprId,
        unsigned_compare: Option<FirRuntimeFunction>,
    },
    /// A counted loop over a progression built by `kotlin.ranges` or held in a value.
    Progression {
        variable: LocalValueId,
        counter: FirRangeCounterKind,
        source: FirProgressionSource,
        unsigned_compare: Option<FirRuntimeFunction>,
    },
    Iterable {
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        kind: FirBuiltinIterableKind,
        iterable: FirExprId,
    },
    Iterator {
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        iterable: FirExprId,
        iterator_ty: ResolvedTy,
        iterator: Box<FirIteratorCall>,
        has_next: Box<FirIteratorCall>,
        next: Box<FirIteratorCall>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirIteratorReceiver {
    Dispatch,
    Extension,
    MemberExtension { dispatch_receiver: FirReceiver },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirIteratorCall {
    pub target: FirCallTarget,
    pub receiver: FirIteratorReceiver,
    /// Checker-selected implicit context operands for this convention call. Iterator protocol
    /// calls have no source value arguments, but their declarations may have context parameters.
    pub context_arguments: Box<[FirIteratorContextArgument]>,
    /// The receiver's conversion to the extension receiver the selected declaration declares. The
    /// protocol call is not specialized per site, so a substituted receiver (`Int` for `T?`)
    /// crosses into the declared type here.
    pub receiver_conversion: Option<FirConversion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirIteratorContextArgument {
    /// Final applied type used to specialize a stable module declaration.
    pub parameter_type: ResolvedTy,
    pub receiver: FirReceiver,
}

/// Checked statement shapes. Assignments and convention operations are already represented as
/// selected expression nodes, so this enum contains no source-name-based write variant.
#[derive(Clone, Debug, PartialEq)]
pub enum FirStatementKind {
    Local {
        target: LocalValueId,
        ty: ResolvedTy,
        mutable: bool,
        lateinit: bool,
        initializer: Option<FirExprId>,
        conversion: Option<FirConversion>,
    },
    /// One initializer evaluation followed by the checker-selected component calls. An underscore
    /// entry has neither a target nor a component expression and therefore performs no call.
    Destructure {
        initializer: FirExprId,
        entries: Box<[FirDestructureEntry]>,
    },
    /// The checker-selected superclass/peer action of a primary or secondary constructor. The
    /// target, argument mapping, omitted defaults, and vararg packing are final; lowering only
    /// chooses their representation.
    ConstructorDelegation(FirConstructorCall),
    /// Evaluate one already-resolved interface-delegate value in a named classifier's primary
    /// constructor and store it into that delegation's generated field. The classifier and ordinal
    /// are stable identities; lowering only materializes storage and assignment.
    InterfaceDelegationInitializer {
        classifier: DeclarationId,
        delegation: u32,
        value: FirExprId,
    },
    Expression(FirExprId),
    Loop {
        target: ControlTargetId,
        header: FirLoopHeader,
        body: FirExprId,
    },
    /// A checked local type alias has no runtime realization. All uses after this declaration already
    /// carry their expanded semantic [`ResolvedTy`], so retaining the alias spelling or its syntax
    /// would only reintroduce source lookup into later phases.
    LocalTypeAlias,
    LocalDeclaration {
        declaration: DeclarationId,
        captures: Box<[FirLocalClassCapture]>,
    },
    LocalFunction {
        declaration: BodyLocalCallableDeclarationId,
        callable: LocalCallableId,
        suspend: bool,
        /// The source declared this local function `tailrec`. Carried like `suspend` because it is
        /// a fact about the DECLARATION that lowering needs and cannot recover from the body: a
        /// self-call in a tail position looks the same whether or not the author asked for the
        /// loop, and only this says the constant-stack promise was made.
        tailrec: bool,
        body: Box<FirBody>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FirDestructureEntry {
    Ignored {
        origin: OriginId,
    },
    Binding {
        origin: OriginId,
        target: LocalValueId,
        ty: ResolvedTy,
        mutable: bool,
        component: FirExprId,
        conversion: Option<FirConversion>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FirStatement {
    pub origin: OriginId,
    pub kind: FirStatementKind,
}

/// Where a lambda or local function sits among the callables kotlinc lifts out of one declaration:
/// the sequence (its lexical `owner` and outermost declaration name `container`, as the source
/// spells them) and one step per enclosing local callable, down to this one. `lifted` is `false`
/// for a callable kotlinc turns into a class of its own (a suspend lambda), which takes no place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLiftingSite {
    pub owner: Box<str>,
    pub container: Box<str>,
    pub path: Box<[FirLiftingStep]>,
    pub lifted: bool,
}

/// One enclosing local callable of a [`FirLiftingSite`]: its source name (`None` for a lambda or a
/// local delegated property's accessor) and its position in the sequence's source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLiftingStep {
    pub name: Option<Box<str>>,
    pub position: u32,
}

impl FirLiftingSite {
    pub fn from_source(site: &crate::ast::LiftingSite, lifted: bool) -> Self {
        Self {
            owner: site.owner.as_str().into(),
            container: site.container.as_str().into(),
            path: site
                .path
                .iter()
                .map(|step| FirLiftingStep {
                    name: step.name.as_deref().map(Into::into),
                    position: step.position,
                })
                .collect(),
            lifted,
        }
    }
}

/// Exact lexical context a target needs to name the class it realizes for one expression: the
/// stable source classifier that owns the executable context (`None` for the file), the source
/// declaration names below it, and the shared generated-artifact ordinal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirGeneratedClassProvenance {
    pub lexical_owner: Option<DeclarationId>,
    pub segments: Box<[String]>,
    pub ordinal: Option<u32>,
}

/// One checked body unit. Its arenas are body-local and are moved as a single value into lowering;
/// parser ids and unresolved types cannot be represented here.
#[derive(Clone, Debug, PartialEq)]
pub struct FirBody {
    owner: BodyOwnerId,
    local_callable: Option<LocalCallableId>,
    /// Stable classifier-like declaration whose lexical code container owns local callable
    /// implementations from this body. Enum-entry member bodies name the entry declaration; enum
    /// construction arguments name the enclosing enum. This is semantic ownership, not a JVM name.
    lexical_class_owner: Option<DeclarationId>,
    /// Number of checked synthetic constructor-prefix values preceding source parameters for this
    /// body. This is published by the frontend because retained defaults may lower before the class
    /// skeleton has received its capture fields; lowering must not recover the offset from IR state.
    constructor_capture_parameter_count: u32,
    /// Number of language-level classifier context parameters in a constructor body. These follow
    /// synthetic local/inner captures but precede source value parameters.
    constructor_context_parameter_count: u32,
    receiver_type: Option<ResolvedTy>,
    result_type: Option<ResolvedTy>,
    implicit_return: bool,
    default_fragment: bool,
    property_storage_type: Option<ResolvedTy>,
    property_delegate: Option<FirPropertyDelegatePlan>,
    debug_name: Option<Box<str>>,
    vararg_parameter: Option<FirVarargParameter>,
    source_lambda: bool,
    debug_binding_name: Option<Box<str>>,
    /// Checked execution-scope fact; nested callable bodies own their own value.
    pub(super) direct_suspension: bool,
    debug_value_names: HashMap<LocalValueId, Box<str>>,
    /// Physical source-line count for debug output; it carries no source lookup capability.
    source_line_count: u32,
    expression_debug_lines: Vec<FirExpressionDebugLines>,
    statement_debug_lines: Vec<FirStatementDebugLines>,
    /// Naming provenance of each expression the reference compiler realizes as a class of its own
    /// (a callable reference). A naming fact, not a lowering decision.
    generated_class_provenance: HashMap<FirExprId, FirGeneratedClassProvenance>,
    /// This callable's own lifting site, for a lambda or local function body.
    lifting_site: Option<FirLiftingSite>,
    /// Lifted callables declared in this body that have no body of their own: the accessors of a
    /// local delegated property.
    bodiless_lifting_sites: Vec<FirLiftingSite>,
    context_receiver_types: Vec<ResolvedTy>,
    context_parameter_kinds: Vec<crate::types::ContextParameterKind>,
    parameters: Vec<FirValueParameter>,
    default_values: Vec<FirDefaultValue>,
    captures: Vec<FirCapture>,
    implicit_receiver_captures: Vec<FirImplicitReceiverCapture>,
    sam_conversions: Vec<FirSamConversion>,
    platform_narrowings: Vec<FirPlatformNarrowing>,
    control_targets: Vec<FirControlTarget>,
    expressions: Vec<FirExpr>,
    statements: Vec<FirStatement>,
    roots: Vec<FirStatementId>,
    local_value_count: u32,
    local_callable_count: u32,
    /// Checked nested declarations retained only as part of this inline payload.
    inline_nested_declaration_bodies: Vec<FirBody>,
    /// Local-class closure environments retained with inline/default bodies.
    class_body_contexts: HashMap<DeclarationId, ClassBodyContext>,
}

#[derive(Clone)]
struct LocalCallableCaptureRequirements {
    values: Vec<FirCapture>,
    implicit_receivers: Vec<FirImplicitReceiverCapture>,
}

type LocalCallableCaptureScope = HashMap<LocalCallableId, LocalCallableCaptureRequirements>;

impl FirBody {
    pub fn new(owner: BodyOwnerId) -> Self {
        Self {
            owner,
            local_callable: None,
            lexical_class_owner: None,
            constructor_capture_parameter_count: 0,
            constructor_context_parameter_count: 0,
            receiver_type: None,
            result_type: None,
            implicit_return: false,
            default_fragment: false,
            property_storage_type: None,
            property_delegate: None,
            debug_name: None,
            vararg_parameter: None,
            source_lambda: false,
            debug_binding_name: None,
            direct_suspension: false,
            debug_value_names: HashMap::new(),
            source_line_count: 0,
            expression_debug_lines: Vec::new(),
            statement_debug_lines: Vec::new(),
            generated_class_provenance: HashMap::new(),
            lifting_site: None,
            bodiless_lifting_sites: Vec::new(),
            context_receiver_types: Vec::new(),
            context_parameter_kinds: Vec::new(),
            parameters: Vec::new(),
            default_values: Vec::new(),
            captures: Vec::new(),
            implicit_receiver_captures: Vec::new(),
            sam_conversions: Vec::new(),
            platform_narrowings: Vec::new(),
            control_targets: Vec::new(),
            expressions: Vec::new(),
            statements: Vec::new(),
            roots: Vec::new(),
            local_value_count: 0,
            local_callable_count: 0,
            inline_nested_declaration_bodies: Vec::new(),
            class_body_contexts: HashMap::new(),
        }
    }

    pub const fn owner(&self) -> BodyOwnerId {
        self.owner
    }

    pub fn new_local(owner: BodyOwnerId, callable: LocalCallableId) -> Self {
        let mut body = Self::new(owner);
        body.local_callable = Some(callable);
        body
    }

    pub const fn local_callable(&self) -> Option<LocalCallableId> {
        self.local_callable
    }

    pub(crate) fn set_lexical_class_owner(&mut self, owner: Option<DeclarationId>) {
        assert!(
            self.lexical_class_owner.is_none(),
            "a FIR body's lexical class owner may be published only once"
        );
        self.lexical_class_owner = owner;
    }

    pub(crate) const fn lexical_class_owner(&self) -> Option<DeclarationId> {
        self.lexical_class_owner
    }

    pub(crate) fn set_constructor_capture_parameter_count(&mut self, count: u32) {
        assert_eq!(
            self.constructor_capture_parameter_count, 0,
            "constructor capture parameter count may be published only once"
        );
        self.constructor_capture_parameter_count = count;
    }

    pub(crate) const fn constructor_capture_parameter_count(&self) -> u32 {
        self.constructor_capture_parameter_count
    }

    pub(crate) fn set_constructor_context_parameter_count(&mut self, count: u32) {
        assert_eq!(
            self.constructor_context_parameter_count, 0,
            "constructor context parameter count may be published only once"
        );
        self.constructor_context_parameter_count = count;
    }

    pub(crate) const fn constructor_context_parameter_count(&self) -> u32 {
        self.constructor_context_parameter_count
    }

    pub fn set_receiver_type(&mut self, receiver: ResolvedTy) {
        assert!(
            self.receiver_type.replace(receiver).is_none(),
            "a FIR body may publish its receiver type only once"
        );
    }

    pub const fn receiver_type(&self) -> Option<ResolvedTy> {
        self.receiver_type
    }

    pub fn set_result_type(&mut self, result: ResolvedTy) {
        assert!(
            self.result_type.replace(result).is_none(),
            "a FIR body may publish its result type only once"
        );
    }

    pub const fn result_type(&self) -> Option<ResolvedTy> {
        self.result_type
    }

    pub(crate) fn replace_result_type_with_property_storage(&mut self, storage: ResolvedTy) {
        assert!(
            self.result_type.is_some(),
            "only a declaration-owned property body has a result to replace"
        );
        self.result_type = Some(storage);
    }

    pub fn set_implicit_return(&mut self) {
        assert!(
            !self.implicit_return,
            "a FIR body may select implicit return only once"
        );
        self.implicit_return = true;
    }

    pub const fn has_implicit_return(&self) -> bool {
        self.implicit_return
    }

    /// Mark this body as a signature-owned default-expression fragment. It may attach defaults to
    /// a callable, but cannot supply or replace that callable's ordinary executable body.
    pub fn set_default_fragment(&mut self) {
        assert!(
            !self.default_fragment && self.roots.is_empty(),
            "a default fragment cannot contain ordinary callable roots"
        );
        self.default_fragment = true;
    }

    pub const fn is_default_fragment(&self) -> bool {
        self.default_fragment
    }

    pub fn set_property_storage_type(&mut self, storage: ResolvedTy) {
        assert!(
            self.property_storage_type.replace(storage).is_none(),
            "a FIR property body may publish its storage type only once"
        );
    }

    pub const fn property_storage_type(&self) -> Option<ResolvedTy> {
        self.property_storage_type
    }

    pub fn set_property_delegate(&mut self, plan: FirPropertyDelegatePlan) {
        assert!(
            self.property_delegate.replace(plan).is_none(),
            "a FIR body may publish its delegated-property plan only once"
        );
    }

    pub fn property_delegate(&self) -> Option<&FirPropertyDelegatePlan> {
        self.property_delegate.as_ref()
    }

    pub fn set_debug_name(&mut self, name: impl Into<Box<str>>) {
        assert!(
            self.debug_name.replace(name.into()).is_none(),
            "a FIR body may publish its debug name only once"
        );
    }

    pub fn debug_name(&self) -> Option<&str> {
        self.debug_name.as_deref()
    }

    pub fn set_lifting_site(&mut self, site: FirLiftingSite) {
        assert!(
            self.lifting_site.replace(site).is_none(),
            "a FIR body has one lifting site"
        );
    }

    pub fn lifting_site(&self) -> Option<&FirLiftingSite> {
        self.lifting_site.as_ref()
    }

    pub fn add_bodiless_lifting_site(&mut self, site: FirLiftingSite) {
        self.bodiless_lifting_sites.push(site);
    }

    /// Every lifting site this body and the callables nested in it declare, its own included.
    pub fn collect_lifting_sites<'a>(&'a self, out: &mut Vec<&'a FirLiftingSite>) {
        out.extend(self.lifting_site.iter());
        out.extend(self.bodiless_lifting_sites.iter());
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                body.collect_lifting_sites(out);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                body.collect_lifting_sites(out);
            }
        }
    }

    pub fn mark_source_lambda(&mut self, binding_name: Option<impl Into<Box<str>>>) {
        assert!(
            !self.source_lambda,
            "a FIR body may be marked as a source lambda only once"
        );
        self.source_lambda = true;
        self.debug_binding_name = binding_name.map(Into::into);
    }

    pub const fn is_source_lambda(&self) -> bool {
        self.source_lambda
    }

    pub fn debug_binding_name(&self) -> Option<&str> {
        self.debug_binding_name.as_deref()
    }

    pub fn set_debug_value_name(&mut self, value: LocalValueId, name: impl Into<Box<str>>) {
        let previous = self.debug_value_names.insert(value, name.into());
        assert!(
            previous.is_none(),
            "a FIR value may publish its debug name only once"
        );
    }

    pub fn debug_value_name(&self, value: LocalValueId) -> Option<&str> {
        self.debug_value_names.get(&value).map(Box::as_ref)
    }

    pub fn set_context_receiver_types(&mut self, receivers: Vec<ResolvedTy>) {
        assert!(
            self.context_receiver_types.is_empty(),
            "a FIR body may publish its context receivers only once"
        );
        self.context_receiver_types = receivers;
    }

    pub fn context_receiver_types(&self) -> &[ResolvedTy] {
        &self.context_receiver_types
    }

    pub fn add_parameter(&mut self, parameter: FirValueParameter) {
        self.parameters.push(parameter);
    }

    pub fn parameters(&self) -> &[FirValueParameter] {
        &self.parameters
    }

    pub fn add_default_value(&mut self, default: FirDefaultValue) {
        assert!(
            !self
                .default_values
                .iter()
                .any(|existing| existing.parameter == default.parameter),
            "a FIR body may define one default expression per parameter"
        );
        self.default_values.push(default);
    }

    pub fn default_values(&self) -> &[FirDefaultValue] {
        &self.default_values
    }

    pub fn add_capture(&mut self, capture: FirCapture) {
        self.merge_capture(capture);
    }

    pub fn captures(&self) -> &[FirCapture] {
        &self.captures
    }

    pub fn add_implicit_receiver_capture(&mut self, capture: FirImplicitReceiverCapture) {
        self.merge_implicit_receiver_capture(capture);
    }

    pub fn implicit_receiver_captures(&self) -> &[FirImplicitReceiverCapture] {
        &self.implicit_receiver_captures
    }

    pub fn add_sam_conversion(&mut self, conversion: FirSamConversion) -> FirSamConversionId {
        let id = FirSamConversionId::from_raw(next_id(
            self.sam_conversions.len(),
            "body-local SAM conversions",
        ));
        self.sam_conversions.push(conversion);
        id
    }

    pub fn sam_conversion(&self, id: FirSamConversionId) -> Option<&FirSamConversion> {
        self.sam_conversions.get(id.raw() as usize)
    }

    pub fn add_platform_narrowing(
        &mut self,
        narrowing: FirPlatformNarrowing,
    ) -> FirPlatformNarrowingId {
        let id = FirPlatformNarrowingId::from_raw(next_id(
            self.platform_narrowings.len(),
            "body-local platform narrowings",
        ));
        self.platform_narrowings.push(narrowing);
        id
    }

    pub fn platform_narrowing(&self, id: FirPlatformNarrowingId) -> Option<&FirPlatformNarrowing> {
        self.platform_narrowings.get(id.raw() as usize)
    }

    /// Finalize the complete capture ABI of every nested callable. A body must forward both values
    /// read by a descendant and values required to invoke/reference a selected lexical callable.
    /// The latter is transitive (`lambda -> local A -> local B -> value`) and local functions may be
    /// mutually recursive, so monotonically close the finite capture sets to a fixed point.
    pub fn finalize_capture_forwarding(&mut self) {
        loop {
            let mut scopes = Vec::new();
            if !self.finalize_capture_forwarding_pass(&mut scopes) {
                break;
            }
        }
        self.normalize_shared_capture_cells();
    }

    /// Give every closure over one lexical value the same storage ABI. A captured `var`, or a write
    /// in any sibling/descendant closure, makes that value a shared cell for all of them; otherwise
    /// one closure can accept the holder while another declares the holder's payload type for the
    /// same runtime capture slot.
    fn normalize_shared_capture_cells(&mut self) {
        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                body.normalize_shared_capture_cells();
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                body.normalize_shared_capture_cells();
            }
        }

        let mut shared = std::collections::HashSet::new();
        shared.extend(
            self.statements
                .iter()
                .filter_map(|statement| match &statement.kind {
                    FirStatementKind::Local {
                        target,
                        mutable: true,
                        ..
                    } => Some(*target),
                    _ => None,
                }),
        );
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                body.collect_shared_captures_targeting_ancestor(0, &mut shared);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                body.collect_shared_captures_targeting_ancestor(0, &mut shared);
            }
        }
        if shared.is_empty() {
            return;
        }
        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                body.upgrade_captures_targeting_ancestor(0, &shared);
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                body.upgrade_captures_targeting_ancestor(0, &shared);
            }
        }
    }

    fn collect_shared_captures_targeting_ancestor(
        &self,
        ancestor_depth: u32,
        shared: &mut std::collections::HashSet<LocalValueId>,
    ) {
        shared.extend(self.captures.iter().filter_map(|capture| {
            (capture.enclosing_depth == ancestor_depth && capture.shared_cell)
                .then_some(capture.source)
                .and_then(FirCaptureSource::value)
        }));
        let nested_depth = ancestor_depth
            .checked_add(1)
            .expect("too many nested capture bodies");
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                body.collect_shared_captures_targeting_ancestor(nested_depth, shared);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                body.collect_shared_captures_targeting_ancestor(nested_depth, shared);
            }
        }
    }

    fn upgrade_captures_targeting_ancestor(
        &mut self,
        ancestor_depth: u32,
        shared: &std::collections::HashSet<LocalValueId>,
    ) {
        for capture in &mut self.captures {
            if capture.enclosing_depth == ancestor_depth
                && capture
                    .source
                    .value()
                    .is_some_and(|source| shared.contains(&source))
            {
                capture.shared_cell = true;
            }
        }
        let nested_depth = ancestor_depth
            .checked_add(1)
            .expect("too many nested capture bodies");
        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                body.upgrade_captures_targeting_ancestor(nested_depth, shared);
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                body.upgrade_captures_targeting_ancestor(nested_depth, shared);
            }
        }
    }

    fn finalize_capture_forwarding_pass(
        &mut self,
        scopes: &mut Vec<LocalCallableCaptureScope>,
    ) -> bool {
        scopes.push(self.local_callable_capture_scope());
        let mut changed = false;

        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                changed |= body.finalize_capture_forwarding_pass(scopes);
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                changed |= body.finalize_capture_forwarding_pass(scopes);
            }
        }

        let mut callable_values = Vec::new();
        let mut callable_receivers = Vec::new();
        for expression in &self.expressions {
            let target = match &expression.kind {
                FirExprKind::LocalCall { target, .. }
                | FirExprKind::LocalCallableReference { target, .. } => target,
                _ => continue,
            };
            let Some(scope) = scopes
                .len()
                .checked_sub(target.body_depth as usize + 1)
                .and_then(|index| scopes.get(index))
            else {
                continue;
            };
            let Some(requirements) = scope.get(&target.callable) else {
                continue;
            };
            callable_values.extend(requirements.values.iter().filter_map(|capture| {
                target
                    .body_depth
                    .checked_add(capture.enclosing_depth)?
                    .checked_sub(1)
                    .map(|enclosing_depth| FirCapture {
                        enclosing_depth,
                        ..capture.clone()
                    })
            }));
            callable_receivers.extend(requirements.implicit_receivers.iter().filter_map(
                |capture| {
                    target
                        .body_depth
                        .checked_add(capture.enclosing_depth)?
                        .checked_sub(1)
                        .map(|enclosing_depth| FirImplicitReceiverCapture {
                            enclosing_depth,
                            ..capture.clone()
                        })
                },
            ));
        }
        for capture in callable_values {
            changed |= self.merge_capture(capture);
        }
        for capture in callable_receivers {
            changed |= self.merge_implicit_receiver_capture(capture);
        }

        let mut forwarded = Vec::new();
        let mut collect = |nested: &FirBody| {
            forwarded.extend(
                nested
                    .captures()
                    .iter()
                    .filter(|capture| capture.enclosing_depth > 0)
                    .map(|capture| FirCapture {
                        origin: capture.origin,
                        enclosing_depth: capture.enclosing_depth - 1,
                        source: capture.source,
                        ty: capture.ty,
                        shared_cell: capture.shared_cell,
                    }),
            );
        };
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                collect(body);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                collect(body);
            }
        }
        for capture in forwarded {
            changed |= self.merge_capture(capture);
        }
        let mut forwarded_receivers = Vec::new();
        let mut collect_receivers = |nested: &FirBody| {
            forwarded_receivers.extend(
                nested
                    .implicit_receiver_captures()
                    .iter()
                    .filter(|capture| capture.enclosing_depth > 0)
                    .map(|capture| FirImplicitReceiverCapture {
                        enclosing_depth: capture.enclosing_depth - 1,
                        ..capture.clone()
                    }),
            );
        };
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                collect_receivers(body);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                collect_receivers(body);
            }
        }
        for capture in forwarded_receivers {
            changed |= self.merge_implicit_receiver_capture(capture);
        }

        scopes.pop();
        changed
    }

    fn local_callable_capture_scope(&self) -> LocalCallableCaptureScope {
        let mut scope = HashMap::new();
        for statement in &self.statements {
            let FirStatementKind::LocalFunction { callable, body, .. } = &statement.kind else {
                continue;
            };
            let previous = scope.insert(
                *callable,
                LocalCallableCaptureRequirements {
                    values: body.captures.clone(),
                    implicit_receivers: body.implicit_receiver_captures.clone(),
                },
            );
            debug_assert!(
                previous.is_none(),
                "a local callable is declared once per body"
            );
        }
        for expression in &self.expressions {
            let FirExprKind::Lambda { callable, body } = &expression.kind else {
                continue;
            };
            let previous = scope.insert(
                *callable,
                LocalCallableCaptureRequirements {
                    values: body.captures.clone(),
                    implicit_receivers: body.implicit_receiver_captures.clone(),
                },
            );
            debug_assert!(
                previous.is_none(),
                "a local callable is declared once per body"
            );
        }
        scope
    }

    fn merge_capture(&mut self, capture: FirCapture) -> bool {
        if let Some(existing) = self.captures.iter_mut().find(|existing| {
            existing.enclosing_depth == capture.enclosing_depth && existing.source == capture.source
        }) {
            let changed = capture.shared_cell && !existing.shared_cell;
            existing.shared_cell |= capture.shared_cell;
            changed
        } else {
            self.captures.push(capture);
            true
        }
    }

    fn merge_implicit_receiver_capture(&mut self, capture: FirImplicitReceiverCapture) -> bool {
        if self.implicit_receiver_captures.iter().any(|existing| {
            existing.enclosing_depth == capture.enclosing_depth
                && existing.current == capture.current
                && existing.depth == capture.depth
                && existing.path == capture.path
        }) {
            false
        } else {
            self.implicit_receiver_captures.push(capture);
            true
        }
    }

    pub fn add_control_target(&mut self, target: FirControlTarget) -> ControlTargetId {
        let id =
            ControlTargetId::from_raw(next_id(self.control_targets.len(), "FIR control targets"));
        self.control_targets.push(target);
        id
    }

    pub fn control_target(&self, target: ControlTargetId) -> Option<FirControlTarget> {
        self.control_targets.get(target.raw() as usize).copied()
    }

    pub fn add_expr(&mut self, expression: FirExpr) -> FirExprId {
        let id = FirExprId::from_raw(next_id(self.expressions.len(), "FIR expressions"));
        self.expressions.push(expression);
        self.expression_debug_lines.push(Default::default());
        id
    }

    pub(crate) fn set_generated_class_provenance(
        &mut self,
        expression: FirExprId,
        provenance: FirGeneratedClassProvenance,
    ) {
        self.generated_class_provenance
            .insert(expression, provenance);
    }

    pub fn generated_class_provenance(
        &self,
        expression: FirExprId,
    ) -> Option<&FirGeneratedClassProvenance> {
        self.generated_class_provenance.get(&expression)
    }

    pub fn expr(&self, id: FirExprId) -> Option<&FirExpr> {
        self.expressions.get(id.raw() as usize)
    }

    pub(crate) fn expr_mut(&mut self, id: FirExprId) -> Option<&mut FirExpr> {
        self.expressions.get_mut(id.raw() as usize)
    }

    pub fn expression_count(&self) -> usize {
        self.expressions.len()
    }

    pub fn add_statement(&mut self, statement: FirStatement) -> FirStatementId {
        let id = FirStatementId::from_raw(next_id(self.statements.len(), "FIR statements"));
        self.statements.push(statement);
        self.statement_debug_lines
            .push(FirStatementDebugLines::default());
        id
    }

    pub fn statement(&self, id: FirStatementId) -> Option<&FirStatement> {
        self.statements.get(id.raw() as usize)
    }

    pub fn statement_count(&self) -> usize {
        self.statements.len()
    }

    pub fn push_root(&mut self, statement: FirStatementId) {
        assert!(
            self.statement(statement).is_some(),
            "a FIR body root must belong to that body"
        );
        self.roots.push(statement);
    }

    pub fn roots(&self) -> &[FirStatementId] {
        &self.roots
    }

    pub fn allocate_local_value(&mut self) -> LocalValueId {
        let value = LocalValueId::from_raw(self.local_value_count);
        self.local_value_count = self
            .local_value_count
            .checked_add(1)
            .expect("too many body-local FIR values");
        value
    }

    pub const fn local_value_count(&self) -> u32 {
        self.local_value_count
    }

    pub(crate) fn attach_inline_nested_declaration_body(&mut self, body: FirBody) {
        assert_ne!(
            self.owner, body.owner,
            "an inline declaration payload cannot contain its root body twice"
        );
        assert!(
            self.inline_nested_declaration_bodies
                .iter()
                .all(|nested| nested.owner != body.owner),
            "an inline payload may retain one checked body per nested declaration"
        );
        self.inline_nested_declaration_bodies.push(body);
    }

    pub(crate) fn inline_nested_declaration_bodies(&self) -> &[FirBody] {
        &self.inline_nested_declaration_bodies
    }

    pub(crate) fn collect_inline_local_declarations(
        &self,
        declarations: &mut std::collections::HashSet<DeclarationId>,
    ) {
        for statement in &self.statements {
            match &statement.kind {
                FirStatementKind::LocalDeclaration { declaration, .. } => {
                    declarations.insert(*declaration);
                }
                FirStatementKind::LocalFunction { body, .. } => {
                    body.collect_inline_local_declarations(declarations);
                }
                _ => {}
            }
        }
        for expression in &self.expressions {
            match &expression.kind {
                FirExprKind::AnonymousObject(object) => {
                    declarations.insert(object.declaration);
                }
                FirExprKind::Lambda { body, .. } => {
                    body.collect_inline_local_declarations(declarations);
                }
                _ => {}
            }
        }
        for body in &self.inline_nested_declaration_bodies {
            body.collect_inline_local_declarations(declarations);
        }
    }

    pub(crate) fn collect_referenced_module_callables(
        &self,
        callables: &mut std::collections::HashSet<CallableId>,
    ) {
        for expression in &self.expressions {
            match &expression.kind {
                FirExprKind::Call(call)
                | FirExprKind::ComparisonCall { call, .. }
                | FirExprKind::ContainmentCall { call, .. } => {
                    if let Some(callable) = call.target.module() {
                        callables.insert(callable);
                    }
                }
                FirExprKind::ConstructorCall(call) => {
                    if let FirConstructorTarget::Module {
                        declaration: callable,
                        ..
                    } = call.target
                    {
                        callables.insert(callable);
                    }
                }
                FirExprKind::CallableReference { target, .. } => {
                    if let Some(callable) = target.module() {
                        callables.insert(callable);
                    }
                }
                FirExprKind::Lambda { body, .. } => {
                    body.collect_referenced_module_callables(callables);
                }
                _ => {}
            }
        }
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                body.collect_referenced_module_callables(callables);
            }
        }
        for body in &self.inline_nested_declaration_bodies {
            body.collect_referenced_module_callables(callables);
        }
    }

    pub fn allocate_local_callable(&mut self) -> LocalCallableId {
        let callable = LocalCallableId::from_raw(self.local_callable_count);
        self.local_callable_count = self
            .local_callable_count
            .checked_add(1)
            .expect("too many body-local FIR callables");
        callable
    }

    pub(crate) fn record_class_body_context(
        &mut self,
        declaration: DeclarationId,
        context: ClassBodyContext,
    ) {
        self.class_body_contexts
            .entry(declaration)
            .or_default()
            .merge(context);
    }

    pub(crate) fn owns_class_body_context(&self, declaration: DeclarationId) -> bool {
        self.class_body_contexts.contains_key(&declaration)
    }

    pub(crate) fn collect_class_body_contexts(
        &self,
        contexts: &mut HashMap<DeclarationId, ClassBodyContext>,
    ) {
        for (declaration, context) in &self.class_body_contexts {
            contexts
                .entry(*declaration)
                .or_default()
                .merge(context.clone());
        }
        for statement in &self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                body.collect_class_body_contexts(contexts);
            }
        }
        for expression in &self.expressions {
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                body.collect_class_body_contexts(contexts);
            }
        }
        for body in &self.inline_nested_declaration_bodies {
            body.collect_class_body_contexts(contexts);
        }
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.parameters.len() * std::mem::size_of::<FirValueParameter>()
            + self
                .property_delegate
                .as_ref()
                .map_or(0, |_| std::mem::size_of::<FirPropertyDelegatePlan>())
            + self
                .property_storage_type
                .map_or(0, |_| std::mem::size_of::<ResolvedTy>())
            + self.debug_name.as_deref().map_or(0, str::len)
            + self.debug_binding_name.as_deref().map_or(0, str::len)
            + self
                .debug_value_names
                .values()
                .map(|name| std::mem::size_of::<LocalValueId>() + name.len())
                .sum::<usize>()
            + self.expression_debug_lines.len() * std::mem::size_of::<FirExpressionDebugLines>()
            + self.statement_debug_lines.len() * std::mem::size_of::<FirStatementDebugLines>()
            + self.default_values.len() * std::mem::size_of::<FirDefaultValue>()
            + self.context_receiver_types.len() * std::mem::size_of::<ResolvedTy>()
            + self.captures.len() * std::mem::size_of::<FirCapture>()
            + self.implicit_receiver_captures.len()
                * std::mem::size_of::<FirImplicitReceiverCapture>()
            + self
                .implicit_receiver_captures
                .iter()
                .map(|capture| capture.path.len() * std::mem::size_of::<DeclarationId>())
                .sum::<usize>()
            + self.sam_conversions.len() * std::mem::size_of::<FirSamConversion>()
            + self
                .sam_conversions
                .iter()
                .map(|conversion| {
                    conversion.method.len()
                        + conversion.parameters.len() * std::mem::size_of::<ResolvedTy>()
                        + conversion.declared_parameters.len() * std::mem::size_of::<ResolvedTy>()
                })
                .sum::<usize>()
            + self.platform_narrowings.len() * std::mem::size_of::<FirPlatformNarrowing>()
            + self
                .platform_narrowings
                .iter()
                .map(|narrowing| narrowing.message.len())
                .sum::<usize>()
            + self.control_targets.len() * std::mem::size_of::<FirControlTarget>()
            + self.expressions.len() * std::mem::size_of::<FirExpr>()
            + self
                .expressions
                .iter()
                .map(|expression| expression.kind.storage_payload_bytes())
                .sum::<usize>()
            + self.statements.len() * std::mem::size_of::<FirStatement>()
            + self
                .statements
                .iter()
                .map(|statement| match &statement.kind {
                    FirStatementKind::Destructure { entries, .. } => {
                        entries.len() * std::mem::size_of::<FirDestructureEntry>()
                    }
                    FirStatementKind::Local { .. }
                    | FirStatementKind::Expression(_)
                    | FirStatementKind::Loop { .. }
                    | FirStatementKind::InterfaceDelegationInitializer { .. }
                    | FirStatementKind::LocalTypeAlias => 0,
                    FirStatementKind::LocalDeclaration { captures, .. } => {
                        captures.len() * std::mem::size_of::<FirLocalClassCapture>()
                            + captures
                                .iter()
                                .map(|capture| {
                                    capture.name.len() + capture.source.storage_payload_bytes()
                                })
                                .sum::<usize>()
                    }
                    FirStatementKind::ConstructorDelegation(call) => call.storage_payload_bytes(),
                    FirStatementKind::LocalFunction { body, .. } => {
                        std::mem::size_of::<FirBody>() + body.storage_payload_bytes()
                    }
                })
                .sum::<usize>()
            + self.roots.len() * std::mem::size_of::<FirStatementId>()
            + self.inline_nested_declaration_bodies.len() * std::mem::size_of::<FirBody>()
            + self
                .inline_nested_declaration_bodies
                .iter()
                .map(FirBody::storage_payload_bytes)
                .sum::<usize>()
            + self.class_body_contexts.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<ClassBodyContext>())
            + self
                .class_body_contexts
                .values()
                .map(|context| {
                    context.values.len()
                        * (std::mem::size_of::<String>()
                            + std::mem::size_of::<ClassCaptureBinding>())
                        + context.capture_values.len()
                            * (std::mem::size_of::<String>()
                                + std::mem::size_of::<ClassCaptureBinding>())
                        + context
                            .capture_values
                            .iter()
                            .map(|(name, _)| name.capacity())
                            .sum::<usize>()
                        + context.delegates.len()
                            * (std::mem::size_of::<String>()
                                + std::mem::size_of::<LocalDelegateBinding>())
                        + context.callables.len()
                            * (std::mem::size_of::<BodyLocalCallableDeclarationId>()
                                + std::mem::size_of::<(u32, LocalCallableId)>())
                        + context.receivers.len() * std::mem::size_of::<ClassCaptureBinding>()
                })
                .sum::<usize>()
    }
}

/// Persistent checked bodies required by call-site inlining. The only insertion path checks the
/// resolved declaration header and rejects every ordinary body.
#[derive(Debug, Default)]
pub struct InlineBodyStore {
    bodies: std::collections::BTreeMap<CallableId, FirBody>,
}

/// Checked signature expressions retained from Pass 1 until their owning source is lowered. Unlike
/// an ordinary body, a default is callable signature payload: callers and the generated default ABI
/// need it even when the declaration body is reparsed later.
#[derive(Debug, Default)]
pub struct DefaultArgumentStore {
    bodies: std::collections::BTreeMap<CallableId, FirBody>,
}

/// Callable facts published by signature finalization. Construction is crate-private so syntax
/// alone cannot claim that a declaration is semantically inline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedCallableHeader {
    pub id: CallableId,
    pub declaration: DeclarationId,
    pub name: ResolvedCallableName,
    pub shape: ResolvedCallableShape,
    inline: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedCallableName {
    Function(DeclarationNameId),
    Constructor,
}

/// Backend-neutral callable parameter layout. The resolved signature stores context receivers and
/// declared value parameters; the independently selected extension receiver stays here rather than
/// masquerading as a value argument.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedCallableShape {
    pub context_parameter_count: u32,
    /// Leading context parameters represented as named lexical values. Remaining context
    /// parameters are legacy receiver slots and therefore participate in the implicit-receiver
    /// tower without creating source-visible value bindings.
    pub context_value_count: u32,
    pub extension_receiver: Option<ResolvedTy>,
}

impl ResolvedCallableHeader {
    pub(super) const fn new(
        id: CallableId,
        declaration: DeclarationId,
        name: ResolvedCallableName,
        shape: ResolvedCallableShape,
        inline: bool,
    ) -> Self {
        Self {
            id,
            declaration,
            name,
            shape,
            inline,
        }
    }

    pub const fn is_inline(self) -> bool {
        self.inline
    }
}

impl InlineBodyStore {
    pub fn insert(&mut self, callable: ResolvedCallableHeader, body: FirBody) {
        assert!(
            callable.is_inline(),
            "only semantically inline declarations may enter InlineBodyStore"
        );
        assert_eq!(
            body.owner(),
            BodyOwnerId::from_raw(callable.declaration.raw()),
            "inline FIR must belong to the inserted declaration"
        );
        crate::trace_compiler!(
            "fir",
            "retain signature defaults callable={:?} declaration={:?} count={}",
            callable.id,
            callable.declaration,
            body.default_values().len(),
        );
        assert!(
            self.bodies.insert(callable.id, body).is_none(),
            "an inline callable body may be inserted only once"
        );
    }

    pub fn get(&self, callable: CallableId) -> Option<&FirBody> {
        self.bodies.get(&callable)
    }

    pub(crate) fn attach_nested_declaration_body(&mut self, callable: CallableId, body: FirBody) {
        self.bodies
            .get_mut(&callable)
            .expect("an inline root must be retained before its nested declarations")
            .attach_inline_nested_declaration_body(body);
    }

    pub(crate) fn retained_bodies_for_source<'a>(
        &'a self,
        index: &'a super::signature::ResolvedModuleIndex,
        source: SourceFileId,
    ) -> impl Iterator<Item = &'a FirBody> + 'a {
        self.bodies.values().filter(move |body| {
            index
                .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                .is_some_and(|anchor| anchor.source == source)
        })
    }

    /// Clone the retained bodies for one source into a consuming lowering unit. The originals remain
    /// available for call-site inlining throughout Pass 2; only inline FIR is allowed to pay this
    /// retention/copy cost.
    pub fn bodies_for_source(
        &self,
        index: &super::signature::ResolvedModuleIndex,
        source: SourceFileId,
    ) -> Vec<(CallableId, FirBody)> {
        self.bodies
            .iter()
            .filter_map(|(callable, body)| {
                index
                    .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                    .is_some_and(|anchor| anchor.source == source)
                    .then(|| (*callable, body.clone()))
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.bodies
            .values()
            .map(|body| std::mem::size_of::<CallableId>() + body.storage_payload_bytes())
            .sum()
    }
}

impl DefaultArgumentStore {
    pub fn insert(&mut self, callable: ResolvedCallableHeader, body: FirBody) {
        assert!(
            body.is_default_fragment(),
            "only checked defaults enter the store"
        );
        assert!(
            !body.default_values().is_empty(),
            "a default fragment is nonempty"
        );
        assert_eq!(
            body.owner(),
            BodyOwnerId::from_raw(callable.declaration.raw()),
            "checked defaults must belong to their surviving callable"
        );
        assert!(
            self.bodies.insert(callable.id, body).is_none(),
            "a callable's checked defaults may be inserted only once"
        );
    }

    pub fn take_for_source(
        &mut self,
        index: &super::signature::ResolvedModuleIndex,
        source: SourceFileId,
    ) -> Vec<(CallableId, FirBody)> {
        let selected = self
            .bodies
            .iter()
            .filter_map(|(callable, body)| {
                index
                    .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                    .is_some_and(|anchor| anchor.source == source)
                    .then_some(*callable)
            })
            .collect::<Vec<_>>();
        selected
            .into_iter()
            .filter_map(|callable| self.bodies.remove(&callable).map(|body| (callable, body)))
            .collect()
    }

    pub(crate) fn retained_bodies_for_source<'a>(
        &'a self,
        index: &'a super::signature::ResolvedModuleIndex,
        source: SourceFileId,
    ) -> impl Iterator<Item = &'a FirBody> + 'a {
        self.bodies.values().filter(move |body| {
            index
                .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                .is_some_and(|anchor| anchor.source == source)
        })
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.bodies
            .values()
            .map(|body| std::mem::size_of::<CallableId>() + body.storage_payload_bytes())
            .sum()
    }
}

/// Ordinary bodies cross the frontend boundary by value. Implementations lower and emit during
/// this call; the frontend keeps no body collection alongside the persistent module.
pub trait CheckedBodySink {
    /// Consume a body after the checked-FIR boundary has finalized all nested capture forwarding.
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody);

    /// Finalization belongs to this boundary, rather than to individual body-kind dispatchers:
    /// constructors, scripts, enum entries, and default fragments have no callable header and must
    /// still obey exactly the same capture-ownership invariant as ordinary functions.
    fn accept(&mut self, owner: BodyOwnerId, mut body: FirBody) {
        assert_eq!(
            body.owner(),
            owner,
            "checked FIR owner must match the consumed body unit"
        );
        body.finalize_capture_forwarding();
        self.accept_finalized(owner, body);
    }
}

/// Route a checked body according to its resolved inline flag. Inline bodies are prepared before
/// callers; all other bodies are immediately consumed by the lowering/backend sink.
pub fn dispatch_checked_body(
    callable: ResolvedCallableHeader,
    work: BodyWorkItem,
    mut body: FirBody,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
) {
    assert_eq!(
        callable.declaration, work.declaration,
        "resolved callable must identify the scheduled declaration"
    );
    assert_eq!(
        body.owner(),
        work.owner,
        "checked FIR owner must match its scheduled body unit"
    );
    if callable.is_inline() {
        body.finalize_capture_forwarding();
        inline_bodies.insert(callable, body);
    } else {
        ordinary_sink.accept(work.owner, body);
    }
}
