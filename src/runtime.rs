//! Target runtime and ABI services used after frontend checking.

use crate::libraries::{LibraryCallable, SemanticPlatform};
use crate::types::Ty;

/// Platform-provided accessor used by counted range/progression loop lowering. The name and descriptor
/// are backend tokens; common lowering only emits them back to the same backend.
#[derive(Clone, Debug)]
pub struct PlatformAccessor {
    pub name: String,
    pub descriptor: String,
}

/// The platform/library shape of a range-like type that can be iterated as a counted loop. This keeps
/// library class names, mangled value-class accessors, and backend descriptors out of common lowering.
#[derive(Clone, Debug)]
pub struct CountedLoopInfo {
    pub elem: Ty,
    pub first: PlatformAccessor,
    pub last: PlatformAccessor,
    /// `None` for unit-step ranges; `Some` for progressions whose step is read from the value.
    pub step: Option<(PlatformAccessor, Ty)>,
}

/// Platform runtime class plus constructor descriptor used when common lowering must synthesize a
/// library runtime object. Both fields are backend tokens owned by the provider.
#[derive(Clone, Debug)]
pub struct PlatformCtor {
    pub internal: String,
    pub ctor_desc: String,
}

/// Platform-owned static field access. Used for target-specific singleton/companion realizations.
#[derive(Clone, Debug)]
pub struct PlatformField {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

/// Platform construction plan for a Kotlin range value expression. `elem` is the semantic element
/// type operands coerce to; `through` constructs `a..b`; `until` realizes `a..<b` when supported.
#[derive(Clone, Debug)]
pub struct RangeConstruction {
    pub elem: Ty,
    pub result: Ty,
    pub through: PlatformRangeCtor,
    pub until: Option<LibraryCallable>,
    pub through_static: Option<LibraryCallable>,
}

/// Platform-owned range constructor tokens. `trailing_nulls` covers synthetic marker arguments such as
/// JVM unsigned range constructors without exposing those marker classes to common lowering.
#[derive(Clone, Debug)]
pub struct PlatformRangeCtor {
    pub internal: String,
    pub ctor_desc: String,
    pub trailing_nulls: usize,
}

/// Platform-owned runtime helper. The common lowerer can request a semantic helper and emit the
/// returned opaque callable without spelling target runtime classes or descriptors.
#[derive(Clone, Copy, Debug)]
pub enum RuntimeOp {
    UnsignedBox,
    UnsignedUnbox,
    UnsignedCompare,
    UnsignedDivide,
    UnsignedRemainder,
    UnsignedToString,
    UIntToLong,
    PrimitiveCompare,
    HashCode,
    ArrayToString,
    ArrayCopyOf,
    StartCoroutine,
    ThrowOnFailure,
    CoroutineSuspended,
}

#[derive(Clone, Copy, Debug)]
pub enum RuntimeCtor {
    IllegalStateException,
    AssertionError,
}

/// Backend runtime/ABI services used by lowering and emit.
pub trait TargetRuntime {
    /// Runtime implementation class constructed for a property reference on this platform.
    fn property_reference_impl(&self, _arity: usize, _mutable: bool) -> Option<PlatformCtor> {
        None
    }

    /// Platform reflection signature stored in a synthesized property-reference object.
    fn property_reference_signature(&self, _getter_name: &str, _ret: Ty) -> Option<String> {
        None
    }

    /// Platform field/type descriptor for a lowered IR type.
    fn type_descriptor(&self, _ty: Ty) -> Option<String> {
        None
    }

    /// Platform field/type descriptor for a type already stored in IR representation. Most targets can
    /// treat this like [`TargetRuntime::type_descriptor`], but the JVM maps IR spellings such as
    /// `Obj("kotlin/Int")` back to primitive descriptor carriers in some ABI positions.
    fn ir_type_descriptor(&self, ty: Ty) -> Option<String> {
        self.type_descriptor(ty)
    }

    /// Physical value type used for an IR value on this target.
    fn ir_value_type(&self, ty: Ty) -> Ty {
        ty
    }

    /// Platform method descriptor for lowered IR parameter and return types.
    fn method_descriptor(&self, _params: &[Ty], _ret: Ty) -> Option<String> {
        None
    }

    /// Runtime superclass used for synthesized function references on this platform.
    fn function_reference_impl_type(&self) -> Option<Ty> {
        None
    }

    /// Platform accessor used for built-in enum properties such as `ordinal` and `name`.
    fn enum_member_accessor(&self, _name: &str) -> Option<PlatformAccessor> {
        None
    }

    /// Platform static field for an object singleton value.
    fn object_instance_field(&self, _internal: &str) -> Option<PlatformField> {
        None
    }

    /// Platform static field for a class companion singleton value.
    fn companion_instance_field(
        &self,
        _class_internal: &str,
        _companion_internal: &str,
        _field_name: &str,
    ) -> Option<PlatformField> {
        None
    }

    /// Platform runtime holder type used when a mutable local of `elem` is captured by a closure.
    fn mutable_local_ref_type(&self, _elem: Ty) -> Option<Ty> {
        None
    }

    /// The target's scalar carrier for a semantic value type, when it has one. Signed primitives usually
    /// carry themselves; target-owned value primitives may carry another type (`UInt` as `Int` on JVM).
    /// Common lowering uses this to decide boxing/coercion shape without spelling a backend primitive set.
    fn scalar_value_repr(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// The boxed library value-class/object type for an unsigned integer semantic type, when the target has
    /// such a representation (`UInt` -> `kotlin/UInt` on JVM). Common lowering uses this for box/unbox and
    /// `is UInt` shapes without spelling target class names.
    fn unsigned_integer_box_type(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// If `internal` is a platform range/progression type that can be emitted as a counted loop,
    /// describe its source element type and platform accessors. The default keeps non-platform sources
    /// on the ordinary iterator path.
    fn counted_loop_info(&self, _internal: &str) -> Option<CountedLoopInfo> {
        None
    }

    /// Platform construction shape for a Kotlin range expression with operands `lo` and `hi`.
    /// The provider owns range runtime class names, constructor descriptors, synthetic marker slots,
    /// and helper facades.
    fn range_construction(&self, _lo: Ty, _hi: Ty) -> Option<RangeConstruction> {
        None
    }

    /// Physical call descriptor for invoking a suspend callable whose current descriptor is still the
    /// logical source signature. The provider owns continuation descriptors and return erasure.
    fn suspend_cps_descriptor(&self, _logical_descriptor: &str) -> Option<String> {
        None
    }

    /// Platform callable for a runtime helper. Common lowering selects the semantic helper; the
    /// provider owns target runtime classes, method names, and descriptors.
    fn runtime_callable(&self, _op: RuntimeOp, _ty: Ty) -> Option<LibraryCallable> {
        None
    }

    /// Platform constructor for a runtime support object. Common lowering selects the semantic
    /// constructor; the provider owns target runtime classes and descriptors.
    fn runtime_ctor(&self, _ctor: RuntimeCtor) -> Option<PlatformCtor> {
        None
    }

    /// Whether a selected library callable has the semantics of Kotlin's defaulted reified
    /// `assertFailsWith<T> { ... }` helper. Such helpers cannot be called directly when their platform
    /// realization is private inline-only bytecode; common lowering can still realize the semantic shape
    /// as `try/catch` IR when the target identifies it.
    fn is_reified_assert_fails_with_default(&self, _callable: &LibraryCallable) -> bool {
        false
    }
}

pub trait CompilerPlatform: SemanticPlatform + TargetRuntime {}

impl<T> CompilerPlatform for T where T: SemanticPlatform + TargetRuntime {}

impl TargetRuntime for crate::libraries::EmptySymbolSource {}
