//! Callable and property REFERENCES: the checked reference value common lowering produces, and
//! the synthesized classes a backend realizes it as.
//!
//! [`IrCallableReference`] is the language-level fact. The carrier records below are target
//! realization records, not declaration facts: a JVM pass selects the accessor a reference calls,
//! its physical name and descriptor, the owner it names for reflection, and the value-class
//! adapters around it, then records the answer here for emission to execute. Nothing in them is
//! recovered from a property's spelling, and nothing later re-derives them.

use super::*;
use crate::types::TyKind;

/// Language-level identity of a callable-reference value whose invocation has already been lowered
/// to a common-IR adapter. The target is used for Kotlin reflection/equality only; a backend must
/// never recover it from the adapter's generated name.
#[derive(Clone, Debug)]
pub enum IrCallableReferenceTarget {
    Module(crate::fir::CallableId),
    Constructor {
        classifier: TypeName,
        declaration: IrConstructorReferenceDeclaration,
    },
    /// A local function, lifted to `function`, which a reflective carrier names physically.
    Local {
        owner: Option<TypeName>,
        name: Box<str>,
        function: FunId,
    },
    /// A DEPENDENCY declaration, by the provider-owned identity that names it. Kept opaque, as
    /// every other dependency edge is: the owner spelling, the published name and whatever the
    /// declaration is realized as are the target's to read from its provider. `receiver` is the
    /// checked receiver type the reference was selected on (`None` for a top-level function), whose
    /// classifier a reflective carrier names as the member's owner.
    External {
        declaration: crate::fir::ExternalCallableId,
        receiver: Option<Ty>,
    },
    /// The conversion of a function value to another function type: a synthesized declaration
    /// without a source identity, numbered `ordinal` among the conversions of its callable.
    FunctionValueConversion {
        ordinal: u32,
    },
    /// A checked reference to a function value's `invoke` declaration. The function value is the
    /// bound receiver; `adapter` performs the already-checked forwarding call. Backends realize
    /// the reference carrier without rediscovering this target from the adapter's shape or name.
    FunctionInvoke,
    /// A language-defined classifier callable. `operation` is the declaration the reference
    /// reflects; `classifier` is that declaration's owner.
    Classifier {
        classifier: TypeName,
        operation: IrClassifierCallable,
    },
    /// A property read as a function value (`P::n` passed as `(P) -> Int`). The adapter reads the
    /// property; a use that is not spliced into an inline call is the property reference itself,
    /// whose carrier a backend realizes from this checked identity.
    Property(Box<IrPropertyFunctionReference>),
}

/// The checked property reference a function-typed property reference value denotes. Its receiver
/// operand, when it is bound, is the reference's `bound_receiver`; `bound_extension_receiver` says
/// which receiver of the property that operand is.
#[derive(Clone, Debug)]
pub struct IrPropertyFunctionReference {
    pub target: crate::fir::FirPropertyReferenceTarget,
    pub binding: crate::fir::FirCallableReferenceBinding,
    pub bound_extension_receiver: bool,
    pub mutable: bool,
    /// The program uses this value as the `KProperty` itself, not only as a function to invoke.
    /// Common lowering keeps such a reference checked; a target that realizes it as a function
    /// value (`crate::backend::property_reference_values`) sets this.
    pub reflective: bool,
    /// The adapter writing the property of a reflective mutable reference, in a unit that writes
    /// through one: the reference's
    /// captures and receiver operands as the read adapter takes them, then the written value.
    pub writer: Option<FunId>,
    pub substitutions: Vec<IrCheckedSubstitution>,
}

/// The property declaration a function-typed property reference reads: the identity two such
/// references are equal by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrReferencedProperty {
    Module(crate::fir::PropertyId),
    External(crate::fir::ExternalPropertyId),
}

impl IrPropertyFunctionReference {
    /// The declaration this reference reads. `None` for a classifier read, and for a dependency
    /// reference whose getter is not the dependency property's.
    pub fn referenced_property(&self) -> Option<IrReferencedProperty> {
        match &self.target {
            crate::fir::FirPropertyReferenceTarget::Module(property)
            | crate::fir::FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
                Some(IrReferencedProperty::Module(*property))
            }
            crate::fir::FirPropertyReferenceTarget::External { getter, .. } => {
                match getter.as_ref() {
                    crate::fir::FirPropertyTarget::External { property, .. } => {
                        Some(IrReferencedProperty::External(*property))
                    }
                    _ => None,
                }
            }
            crate::fir::FirPropertyReferenceTarget::Classifier { .. } => None,
        }
    }

    /// The getter is an inline accessor whose body each read splices, so no accessor method
    /// performs the read.
    pub fn getter_is_spliced(&self) -> bool {
        matches!(
            &self.target,
            crate::fir::FirPropertyReferenceTarget::SpecializedModule {
                getter_inline_splice: Some(_),
                ..
            }
        )
    }

    /// The checked property reference this value is, with its bound receiver operand and the
    /// reference's adaptation in place.
    pub fn checked_operation(
        &self,
        bound_receiver: Option<ExprId>,
        adaptation: Option<Box<crate::fir::FirReferenceAdaptation>>,
        function_type: Ty,
    ) -> IrCheckedOperation {
        let (dispatch_receiver, extension_receiver) = if self.bound_extension_receiver {
            (None, bound_receiver)
        } else {
            (bound_receiver, None)
        };
        IrCheckedOperation::PropertyReference {
            target: self.target.clone(),
            delegated: false,
            binding: self.binding,
            dispatch_receiver,
            extension_receiver,
            mutable: self.mutable,
            substitutions: self.substitutions.clone(),
            adaptation,
            function_type: Some(function_type),
        }
    }
}

/// A value-class context a function reference binds, which the physical target takes as the
/// class's underlying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundContextUnbox {
    pub value_class: TypeName,
    /// The declared context type is nullable: `null` stays `null` rather than being unboxed.
    pub nullable: bool,
}

/// The selected constructor declaration, retained independently of its invocation adapter.
#[derive(Clone, Copy, Debug)]
pub enum IrConstructorReferenceDeclaration {
    /// A declaration in the checked source module, shared by that module's files and clones.
    Module(crate::fir::DeclarationId),
    /// A dependency declaration, including one referenced from an imported inline body.
    External(crate::fir::ExternalCallableId),
}

/// A language-defined classifier callable a reference reflects. The physical name and descriptor
/// belong to the backend that realizes the carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrClassifierCallable {
    /// The classifier's implicit `valueOf(String)` member.
    EnumValueOf,
    /// A `fun interface` constructor (`::Action`). Equality compares the interface class, so the
    /// carrier is not a lambda even when the reference is used as a function value.
    SamConstructor,
}

/// A checked callable-reference value after common invocation lowering. `adapter` is an exact
/// common-IR function identity. `captures` are ordinary closure values and `bound_receiver` is the
/// language-level receiver that participates in callable-reference equality. Keeping those facts
/// separate prevents a backend from guessing Kotlin semantics from the number of captured values.
/// The declaration signature remains semantic; target backends independently choose their callable
/// carrier, storage, calling convention, and reflection descriptor.
#[derive(Clone, Debug)]
pub struct IrCallableReference {
    pub target: IrCallableReferenceTarget,
    pub adapter: FunId,
    pub captures: Vec<ExprId>,
    /// How many of the leading `captures` are values the reference site bound for the declaration's
    /// leading context parameters (`CallableReferencesToContextual`). They are ordinary captures
    /// to every consumer; a target whose runtime keeps Kotlin's `boundContextArguments` stores
    /// them there instead of in a field of its own.
    pub bound_contexts: u32,
    pub bound_receiver: Option<ExprId>,
    pub function_type: Ty,
    pub declaration_parameters: Box<[Ty]>,
    pub declaration_result: Ty,
    pub declaration_suspend: bool,
    pub adaptation: Option<Box<crate::fir::FirReferenceAdaptation>>,
    /// Classifier an unbound inherited instance member was written on. Realization uses this
    /// identity directly. Every other reference, including a member extension, leaves it empty.
    pub reflection_owner: Option<crate::types::TypeName>,
}

impl IrCallableReference {
    /// The `captures` slots holding values bound for the declaration's context parameters. They
    /// are the leading captures, and two references to the same declaration are equal only when
    /// these values are equal as well, exactly as for the bound receiver.
    pub fn bound_context_slots(&self) -> std::ops::Range<usize> {
        0..self.bound_contexts as usize
    }
}

impl IrFile {
    /// Build the implementation body of a function-like reference after the frontend has selected
    /// its target and adaptation. Kotlin `Unit` is `void` only at a declared-call boundary; a
    /// `FunctionN.invoke` result is a value and therefore returns the `kotlin.Unit` singleton after
    /// discarding the selected callable's result.
    pub(crate) fn callable_reference_adapter_body(
        &mut self,
        value: ExprId,
        selected_result: crate::types::Ty,
        reference_result: crate::types::Ty,
    ) -> ExprId {
        let mut statements = Vec::with_capacity(2);
        // A `Unit` function type discards the selected result, but the invocation still runs.
        // Publish that call as an effect of the returned value: the splice emits only this value,
        // so a bare `Unit` singleton would drop the call.
        let result = if reference_result == crate::types::Ty::Unit {
            let unit = self.add_expr(IrExpr::UnitInstance);
            self.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit),
            })
        } else if selected_result == reference_result {
            value
        } else {
            self.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: reference_result,
            })
        };
        statements.push(self.add_expr(IrExpr::Return(Some(result))));
        let body = self.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        });
        self.callable_reference_adapter_results.insert(body, result);
        body
    }

    /// The checked invocation template of a callable reference consumed as an inline argument.
    /// Callable-reference lowering published the returned expression directly; this projection
    /// combines it with the reference's ordered captures/bound receiver and adapter signature
    /// without inspecting the adapter body or its reflective target kind.
    ///
    /// The arity is a literal lambda's: the function type's parameters, and for a suspend function
    /// type the `Continuation` its adapter takes after them once a backend gives the adapter its
    /// CPS signature, as a suspend literal's arity counts its implementation's continuation.
    pub fn callable_reference_inline_template(
        &self,
        expression: ExprId,
    ) -> Option<(FunId, Vec<ExprId>, ExprId, usize)> {
        let IrExpr::CallableReference(reference) = self.expr(expression) else {
            return None;
        };
        let TyKind::Fun(signature) = reference.function_type.non_null().kind() else {
            return None;
        };
        // The selected declaration's call is then a suspension point of the template, which the
        // coroutine pass places only in a literal's own inline body.
        if reference.declaration_suspend {
            return None;
        }
        let function = self.functions.get(reference.adapter as usize)?;
        let body = function.body?;
        let returned = *self.callable_reference_adapter_results.get(&body)?;
        let mut captures = reference.captures.clone();
        captures.extend(reference.bound_receiver);
        let declared = self
            .suspend_declared_sigs
            .get(&reference.adapter)
            .map_or(&function.params, |(params, _)| params);
        let arity = signature.params.len() + usize::from(signature.suspend);
        (declared.len().checked_sub(captures.len())? == signature.params.len()).then_some((
            reference.adapter,
            captures,
            returned,
            arity,
        ))
    }
}

/// A synthesized function-reference subclass of `kotlin/jvm/internal/FunctionReferenceImpl`. See
/// `emit_func_ref_class`. `param_tys`/`ret_ty` are the LOGICAL `invoke` signature (for `VirtualUnbound`,
/// `param_tys[0]` is the receiver); the SAM interface erases them to `Object`, so `invoke` casts.
#[derive(Clone, Debug)]
pub struct FuncRef {
    /// Adapted callable references use Kotlin's `AdaptedFunctionReference` carrier so equality and
    /// hashing include the checked adaptation arity/flags instead of lambda identity.
    pub adapted: bool,
    /// A `fun interface` constructor reference. The carrier extends
    /// `FunInterfaceConstructorReference` and is constructed with that interface's `Class`.
    pub fun_interface_constructor: bool,
    pub bound: bool,
    /// Leading adapter parameters bound at the reference site for the declaration's context
    /// parameters. They reach the carrier as one `Object[]` the runtime base class keeps as
    /// `boundContextArguments`, which both reflection and equality read. The carrier's own `invoke`
    /// reads them back from it instead of from fields of its own.
    pub bound_contexts: u32,
    /// Leading adapter parameters stored as subclass fields rather than in the runtime callable
    /// reference's semantic receiver slot. This is fixed by JVM realization from the common
    /// reference's explicit ordinary captures; emission never infers it from capture cardinality.
    pub field_capture_count: u32,
    /// The JVM names of those capture fields, in order.
    pub capture_fields: Vec<String>,
    /// Kotlin source-level function arity. Backends add any representation parameters, such as a
    /// suspend continuation, when selecting their callable carrier.
    pub arity: u8,
    /// The reference's function type is suspend. The generated reference carries Kotlin's semantic
    /// suspend-function identity; each backend owns its physical calling convention.
    pub is_suspend: bool,
    /// The selected declaration itself is suspend, as recorded when it was selected. It differs
    /// from `is_suspend` when an ordinary function is converted to a suspend function type.
    pub declaration_suspend: bool,
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
    /// Per value bound for a leading context parameter: the value class whose underlying the
    /// physical target takes. The value is stored boxed in `boundContextArguments`; `invoke`
    /// unboxes it before the target call.
    pub bound_context_unbox: Vec<Option<BoundContextUnbox>>,
    /// The carrier's own specialized `invoke`: a common-IR instance method of this class whose body
    /// calls the referenced declaration directly, as kotlinc's `FunctionReferenceLowering` builds
    /// it. The backend then writes that method and the erased `FunctionN.invoke` bridge to it
    /// instead of a synthesized dispatching `invoke`.
    pub invoke: Option<FunId>,
    /// The reference's Kotlin function type: the generic interface the carrier implements, which
    /// its class `Signature` spells.
    pub function_type: Ty,
    /// The complete reflected JVM signature of a dependency target, as its provider publishes it
    /// (`charAt(I)C` for `String::get`, `padStart(Ljava/lang/String;IC)Ljava/lang/String;` for an
    /// extension). `None` derives the signature from the reflection types above.
    pub reflection_signature: Option<String>,
    /// What the reflected declaration is, which decides how a backend realizes its reflected name.
    pub reflected: ReflectedCallable,
    /// The in-file function a [`ReflectedCallable::Source`] reference reflects, so a backend that
    /// gives that function another JVM name reflects it under that name.
    pub reflected_function: Option<FunId>,
    /// A backend renamed the carrier's own `invoke` (a value-class signature mangles it), so the
    /// erased `FunctionN.invoke` bridges to it even when their descriptors agree.
    pub invoke_renamed: bool,
}

/// The kind of declaration a function-reference carrier reflects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectedCallable {
    /// A declaration of this module, reflected by its source name until a backend realizes it.
    Source,
    /// A class constructor, reflected as the constructor itself.
    Constructor,
    /// A dependency declaration whose provider published its physical name and signature.
    Physical,
    /// A local function, reflected under the function it was lifted to, whatever a backend
    /// finally names that function.
    LocalFunction(FunId),
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
    /// `setExt(Recv, v)`), unlike a member reference's instance `getExt()`. Also `Some` — naming the
    /// OWNER, not a facade — for a private member reached through an `access$…` bridge, whose
    /// accessor is static in the same way. Which of the two this is, is a JVM selection answer,
    /// recorded with the rest of them in `jvm::property_references::PropertyReferenceRealization`.
    pub ext_facade: Option<Option<TypeName>>,
    /// The carrier's own `get`, holding a spliced (inline) getter's body: kotlinc writes the splice
    /// into the carrier because such a getter is never called. `None` for a carrier whose `get`
    /// calls the selected accessor.
    pub get: Option<super::FunId>,
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

impl IrFile {
    /// Whether `expression` is the reflected property a delegated-property operator receives, as
    /// passed: the reference itself, or it adapted to the operator's declared parameter.
    pub fn is_delegated_property_operand(&self, expression: ExprId) -> bool {
        match self.expr(expression) {
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::PropertyReference { delegated, .. } = &**checked_op =>
            {
                *delegated
            }
            IrExpr::LocalPropertyReference(_) => true,
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => self.is_delegated_property_operand(*arg),
            _ => false,
        }
    }
}
