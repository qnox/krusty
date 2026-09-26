//! The synthesized classes a callable or property REFERENCE becomes.
//!
//! These are target realization records, not declaration facts: a JVM pass selects the accessor a
//! reference calls, its physical name and descriptor, the owner it names for reflection, and the
//! value-class adapters around it, then records the answer here for emission to execute. Nothing
//! in them is recovered from a property's spelling, and nothing later re-derives them.

use super::*;

/// A synthesized function-reference subclass of `kotlin/jvm/internal/FunctionReferenceImpl`. See
/// `emit_func_ref_class`. `param_tys`/`ret_ty` are the LOGICAL `invoke` signature (for `VirtualUnbound`,
/// `param_tys[0]` is the receiver); the SAM interface erases them to `Object`, so `invoke` casts.
#[derive(Clone, Debug)]
pub struct FuncRef {
    /// Adapted callable references use Kotlin's `AdaptedFunctionReference` carrier so equality and
    /// hashing include the checked adaptation arity/flags instead of lambda identity.
    pub adapted: bool,
    pub bound: bool,
    /// Leading adapter parameters stored as subclass fields rather than in the runtime callable
    /// reference's semantic receiver slot. This is fixed by JVM realization from the common
    /// reference's explicit ordinary captures; emission never infers it from capture cardinality.
    pub field_capture_count: u32,
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
