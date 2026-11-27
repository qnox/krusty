//! A decoded KLIB body as checked common IR.
//!
//! Each modelled KLIB IR form lowers to exactly the common IR checked FIR lowering produces for the
//! equivalent source, with the same per-expression facts (logical types, return depths, binding
//! stability, `when` facts), so a backend compiles a dependency body as it compiles a module one.
//! The match over KLIB expression forms is exhaustive: every form either has a lowering here or
//! declines by its name. The forms are lowered by responsibility: blocks and the source block
//! shape of a body ([`blocks`]), the blocks of calls a prepared body inlined
//! ([`compiler_blocks`]), calls of built-in operators and dependency functions
//! ([`calls`]), constructor calls ([`constructions`]), `if`/`when` chains and `&&`/`||` ([`conditionals`]), loops and their jumps
//! ([`loops`]), string templates ([`string_templates`]) and type operators
//! ([`type_operators`]).

use crate::fx_hash::{HashMap, HashSet};

mod arrays;
mod atomic_array_elements;
mod blocks;
mod callable_references;
mod calls;
mod class_initializers;
mod class_literals;
mod closures;
mod compiler_blocks;
mod conditionals;
mod constructions;
mod elvis_blocks;
mod enum_members;
mod instance_storage;
mod loops;
mod object_literals;
mod range_membership;
mod safe_calls;
mod sam_conversions;
use safe_calls::NullProvenRead;
mod string_templates;
mod try_catch;
mod type_operators;

pub(super) use blocks::{lower_parameter_defaults, publish_parameter_defaults};
pub(super) use enum_members::ImplicitEnumMember;

use super::decline::KlibBodyDeclineReason;
use super::function_lowering::FunctionHeader;
use super::ir_builtins::IrBuiltinOperators;
use super::klib_types::KlibTypeParameters;
use super::local_values::LocalValues;
use crate::ir::{
    complete_bottom_value, Callee, ExprId, FunId, IrBindingStability, IrConst, IrExpr, IrFile,
    IrTypeOp,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrTypeId,
    KlibIrTypeOperator, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbol};
use crate::types::{Ty, TypeName};

type Lowered<T> = Result<T, KlibBodyDeclineReason>;

/// The unit function a call of a dependency declaration invokes, with its declared parameters
/// and result.
pub(super) struct LinkedCallee {
    pub(super) callee: Callee,
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
    /// Exact classifier of an external singleton dispatch selected by an earlier support unit.
    pub(super) singleton_dispatch: Option<TypeName>,
    /// The semantic identities of the callee's own type parameters, which the receiver,
    /// parameters and result name and a call's type arguments bind, in declaration order.
    pub(super) type_parameters: Vec<crate::types::TyParamName>,
    /// The complete declared upper bounds of each of [`Self::type_parameters`], as the selected
    /// declaration authenticates them (an empty slot is the implicit `Any?`). `None` when the
    /// selection carries no generic signature to read them from.
    pub(super) type_parameter_bounds: Option<Vec<Vec<Ty>>>,
    /// The semantic identities of the type parameters of a member's generic class, which the
    /// receiver (the class applied to them), parameters and result name and a call binds from its
    /// dispatch receiver's type, in declaration order.
    pub(super) class_type_parameters: Vec<String>,
    /// Whether each of [`Self::params`] declares a default a call may leave it to, for a function
    /// this unit lowers; empty for every other callee, which no call may omit an argument of.
    pub(super) parameter_defaults: Vec<bool>,
    /// The facts a target dispatches the call by, as for a source call of the same declaration.
    pub(super) edge: super::CallEdge,
}

pub(super) struct LinkedCallableReference {
    pub(super) target: LinkedReferenceTarget,
    pub(super) kind: crate::libraries::ExternalCallableKind,
    pub(super) extension_receiver_parameter: Option<usize>,
    pub(super) call: LinkedCallee,
}

/// The declaration a callable reference names.
pub(super) enum LinkedReferenceTarget {
    /// A dependency declaration its provider records.
    External(crate::fir::ExternalCallableId),
    /// A private member of a class the unit lowers, which no provider records: the unit emits it
    /// as `function`, a member of `owner` named `name`.
    PrivateMember {
        owner: TypeName,
        name: Box<str>,
        function: FunId,
    },
}

impl LinkedCallableReference {
    pub(super) fn new(
        declaration: crate::fir::ExternalCallableId,
        callable: crate::libraries::KlibBodyCallable<'_>,
        call: LinkedCallee,
    ) -> Self {
        Self {
            target: LinkedReferenceTarget::External(declaration),
            kind: callable.kind(),
            extension_receiver_parameter: callable.parameter_identities().iter().position(|role| {
                matches!(
                    role,
                    crate::fir::ResolvedParameterIdentity::ExtensionReceiver
                )
            }),
            call,
        }
    }
}

/// The getter bounding a counted loop over indices, with the type its declaration answers.
pub(super) struct IndicesBound {
    pub(super) accessor: crate::metadata::id_signature::KlibAccessorIdSignature,
    pub(super) ty: Ty,
}

/// The declared shape of a member a range literal's membership calls.
pub(super) struct RangeMembershipHeader {
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
}

/// The exact provider-selected `suspendCoroutineUninterceptedOrReturn<T>` call shape.
pub(super) struct UninterceptedSuspensionCall {
    pub(super) parameter: Ty,
    pub(super) result: Ty,
    pub(super) formal: crate::types::TyParamName,
}

/// Joins the calls a body makes of public dependency declarations to the functions they invoke.
pub(super) trait CallLinker {
    /// Read the checked declaration selected for this serialized SAM target, without lookup.
    fn sam_declaration(
        &self,
        _classifier: &KlibPublicIdSignature,
    ) -> Lowered<crate::ir::CheckedKlibSamDeclaration> {
        unsupported("a SAM conversion without a checked declaration")
    }

    /// Whether this exact provider-assigned dependency callable is declared `suspend`. A selected
    /// suspend call in a KLIB body keeps its source result type in common IR; the common coroutine
    /// preparation later supplies the target continuation ABI.
    fn is_suspend_callable(&self, _target: crate::fir::ExternalCallableId) -> bool {
        false
    }

    /// The same authenticated primitive call shape that dependency planning treats as a leaf.
    /// Template linkers retain their ordinary checked external-call boundary by default.
    fn common_primitive(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<super::primitive_calls::CommonPrimitiveCall>> {
        Ok(None)
    }

    /// The enum class whose implicit `values`, `valueOf` or `entries` getter a call of
    /// `callee` names, which the linker has materialized, and that member. Template linkers keep
    /// the call's ordinary declaration boundary.
    fn implicit_enum_member(
        &mut self,
        _ir: &mut IrFile,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<(TypeName, ImplicitEnumMember)>> {
        Ok(None)
    }

    /// The provider-selected compiler suspension intrinsic, if this template linker owns its
    /// identity and has authenticated its serialized header.
    fn unintercepted_suspension(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<UninterceptedSuspensionCall>> {
        Ok(None)
    }

    /// The continuation interception this linker's selected declaration of `callee` is:
    /// Kotlin/Wasm's `interceptedIntrinsic`, which a body expands to its coroutine class's own
    /// interception.
    fn intercepted_continuation(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<super::intercepted_continuation::InterceptedContinuationExpansion>> {
        Ok(None)
    }

    /// The continuation intrinsic this linker's selected declaration of `callee` is: Native's
    /// `getContinuation`/`returnIfSuspended`, which a body realizes as its suspension point.
    fn continuation_intrinsic(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<super::continuation_intrinsics::ContinuationIntrinsicCall>> {
        Ok(None)
    }

    /// The range construction a range literal's membership compares against, read as
    /// [`Self::common_primitive`] reads its primitive but whatever range class the target lays
    /// out: the membership builds no range.
    fn range_literal_bounds(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<super::primitive_calls::CommonPrimitiveCall>> {
        Ok(None)
    }

    /// Whether a checked source edge or retained external export needs this exact declaration.
    /// Such calls must remain callable edges; only a compiler builtin without either obligation
    /// may be folded into a common-IR operation.
    fn requires_declaration(&self, callee: &KlibPublicIdSignature) -> Lowered<bool>;

    /// Whether the serialized declaration of `callee` carries a body. A function this unit
    /// declares gets its body only when its queued entry is lowered, so the declaration, not the
    /// lowered function, proves a member concrete. A linker that cannot read it declines.
    fn declares_implementation(&self, _callee: &KlibPublicIdSignature) -> Lowered<bool> {
        unsupported("a `super` call whose member's implementation this linker cannot read")
    }

    /// The serialized header of the member `callee` an `in` over a range literal calls, read
    /// only to prove it is the range's own membership operator: its dispatch receiver, value
    /// parameters and result. `None` keeps the membership an ordinary checked call, as template
    /// linkers do; a linker whose dependency preparation omits the membership's declaration
    /// edge declines a header it cannot read instead.
    fn range_membership_header(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<Option<RangeMembershipHeader>> {
        Ok(None)
    }

    /// The range literal kotlinc's `ForLoopsLowering` counts through when a `for` loop iterates
    /// the progression a call of `callee` builds: a builtin scalar's `rangeTo`/`rangeUntil`, or
    /// the `until`/`downTo` builder. `None` keeps the loop an iterator-protocol loop over the
    /// call, as template linkers do; a linker whose dependency preparation omits the loop's
    /// protocol edges declines a call it does not prove instead.
    fn range_literal(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<Option<crate::fir::FirRangeOperation>> {
        Ok(None)
    }

    /// The receiver kind of the `indices` getter `getter` is, when its provider declaration is
    /// kotlinc's counted-loop bound (`IndicesHandler`). `None` keeps the iterator protocol, which
    /// a linker whose dependency preparation omits the protocol edges declines instead.
    fn indices_receiver(
        &self,
        _getter: &crate::metadata::id_signature::KlibAccessorIdSignature,
    ) -> Lowered<Option<crate::libraries::IndicesReceiver>> {
        Ok(None)
    }

    /// The getter of the receiver property that bounds a counted loop over the indices `getter`
    /// is: a collection's `size` or a text's `length`. `None` when the bound is no member read
    /// (an array's intrinsic size), or the linker does not model it.
    fn indices_bound(
        &self,
        _getter: &crate::metadata::id_signature::KlibAccessorIdSignature,
    ) -> Lowered<Option<IndicesBound>> {
        Ok(None)
    }

    /// Whether `callee` is kotlinc's progression `reversed`, by its provider declaration. A
    /// linker that does not model it keeps the iterator protocol.
    fn reverses_progression(
        &self,
        _callee: &crate::metadata::id_signature::KlibPublicIdSignature,
    ) -> Lowered<bool> {
        Ok(false)
    }

    /// The atomic field access `callee` is, when its exact declaration is the target stdlib's
    /// authenticated intrinsic over a property reference. Template linkers keep an ordinary call.
    fn atomic_field(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<Option<crate::libraries::AtomicOperation>> {
        Ok(None)
    }

    /// The atomic array element access `callee` is, when its exact declaration is the target
    /// stdlib's authenticated intrinsic over an array. Template linkers keep an ordinary call.
    fn atomic_array_element(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<Option<crate::libraries::AtomicOperation>> {
        Ok(None)
    }

    /// Where the code of the body being lowered stands, for a SAM conversion in it. A template
    /// is expanded into the file being compiled, whose placement is its own.
    fn sam_placement(&self) -> Lowered<crate::ir::IrSamPlacement> {
        Ok(crate::ir::IrSamPlacement::ThisFile)
    }

    /// The function a call of the public declaration `callee` invokes.
    fn link(&mut self, ir: &mut IrFile, callee: &KlibPublicIdSignature) -> Lowered<LinkedCallee>;

    /// The member `callee` a `super` call runs without dispatch. It links as any call of the
    /// member does, except that a body the unit emits for the call itself is the one it names.
    fn link_super(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Lowered<LinkedCallee> {
        self.link(ir, callee)
    }

    fn link_reference(
        &mut self,
        _ir: &mut IrFile,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<LinkedCallableReference> {
        unsupported("a callable reference without an exact provider declaration")
    }

    /// The dependency constructor a constructor call of the public declaration `callee` invokes:
    /// one allocating its class, or a subclass delegating to it.
    fn link_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
        role: ConstructorRole,
    ) -> Lowered<LinkedConstructor>;

    /// The classifier a file-local signature names in the library of the body being lowered.
    /// `Ok(None)` refuses the type, including a class that exists only in another library.
    fn local_class(
        &self,
        _signature: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<Option<crate::types::TypeName>> {
        Ok(None)
    }

    /// The declaration that names the local classifiers of the body being lowered: the package
    /// of a top-level function, or the classifier a member belongs to. `None` when the linker
    /// lowers no declared body or that body belongs to a classifier with no member name.
    fn site_container(&self) -> Option<crate::ir::IrSiteClassContainer> {
        None
    }

    /// Whether `callee`, a public or accessor signature, is a constructor or member of a class private to the
    /// library of the body being lowered. Such a call links as a file-local one does.
    fn links_privately(&self, _callee: &crate::metadata::klib_ir::KlibIrSignature) -> bool {
        false
    }

    /// The classifier of the object a non-public object read names, installed in the unit with
    /// its one instance. Template linkers keep the refusal.
    fn link_local_object(
        &mut self,
        _ir: &mut IrFile,
        _signature: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<crate::types::TypeName> {
        unsupported("a file-private object value")
    }

    /// Whether `signature` names a non-public interface of the library of the body being lowered.
    fn is_local_interface(&self, _signature: &crate::metadata::klib_ir::KlibIrSignature) -> bool {
        false
    }

    /// The non-public interface a type test or a cast of this library's body names, installed in
    /// the unit with its abstract members. Template linkers keep the refusal.
    fn link_local_interface(
        &mut self,
        _ir: &mut IrFile,
        _signature: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<()> {
        unsupported("a type of a file-private interface")
    }

    /// The constructor a file-local construction invokes. Template linkers keep the refusal.
    fn link_local_constructor(
        &mut self,
        _ir: &mut IrFile,
        _callee: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<super::same_library_class::LocalConstructor> {
        unsupported("a call of a file-private constructor")
    }

    /// The member a call of the file-local `callee` inherits when `callee` is a fake override of
    /// a class private to its file, which no declaration defines: the call is one of that member,
    /// dispatched on its receiver. `None` for any other callee; template linkers find none.
    fn inherited_member(
        &self,
        _callee: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Option<crate::metadata::klib_ir::KlibIrSignature> {
        None
    }

    /// The function a file-local call invokes: a top-level function private to the lowered
    /// body's file, or a member of a file-local class. Template linkers keep the refusal.
    /// The getter of an inherited property that a call of the fake override `callee` of a local
    /// class stands for. `None` when `callee` is no such fake override.
    fn inherited_getter(
        &self,
        _callee: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<Option<crate::metadata::klib_ir::KlibIrSignature>> {
        Ok(None)
    }

    fn link_local_function(
        &mut self,
        _ir: &mut IrFile,
        _callee: &crate::metadata::klib_ir::KlibIrSignature,
    ) -> Lowered<LinkedCallee> {
        unsupported("a call of a file-private declaration")
    }

    /// What a call of the property accessor `callee` reaches: the dependency property it reads or
    /// writes, or the accessor function the unit emits.
    fn link_accessor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibAccessorIdSignature,
    ) -> Lowered<LinkedAccessorCall>;

    /// The static storage of the top-level property whose backing field is `field`, which a body
    /// of that property's own library reads or writes. `None` leaves the access to the body's
    /// instance storage frame. Template linkers materialize no storage.
    fn static_field(
        &mut self,
        _ir: &mut IrFile,
        _field: &KlibIrSymbol,
    ) -> Lowered<Option<StaticField>> {
        Ok(None)
    }

    /// Proves that `classifier` is a dependency object, whose value a body may read, and
    /// materializes it in `ir` when its instance is the linker's to create.
    fn singleton(&mut self, ir: &mut IrFile, classifier: TypeName) -> Lowered<()>;

    /// Whether the target owns the representation of `classifier`, which a class of this unit
    /// extends without the unit materializing it. A linker that knows no such class says no.
    fn target_represents(&self, _classifier: TypeName) -> bool {
        false
    }

    /// The dependency enum class whose constant `entry` a body reads, with that constant's name.
    /// A linker that materializes no class declines the read.
    fn enum_entry(
        &mut self,
        _ir: &mut IrFile,
        _entry: &KlibPublicIdSignature,
    ) -> Lowered<(TypeName, String)> {
        unsupported("an enum entry")
    }

    /// `sub` as the class `owner` it is a subtype of, with the type arguments its hierarchy applies
    /// to `owner`.
    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty>;

    /// Whether `sub` is a subtype of `sup` in the calling compilation's semantic model, which a
    /// producer-checked body uses a value of the one where the other is expected for.
    fn is_subtype(&self, parameters: &KlibTypeParameters, sub: Ty, sup: Ty) -> bool;

    /// Whether `left` and `right` are one type in the calling compilation's class model, apart
    /// from projections that restate their parameter's declaration-site variance.
    fn same_type(&self, left: Ty, right: Ty) -> bool;

    /// The declared upper bounds of the classifier `owner`'s type parameter at `index` in the
    /// calling compilation's class model: what a star or `out` projection of that slot captures
    /// besides its own read type.
    fn classifier_parameter_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty>;

    /// Constructor generic bounds consume explicit hierarchy/full-formal evidence without
    /// changing the independent operand-coercion capability.
    fn constructor_bound(&self, parameters: &KlibTypeParameters, argument: Ty, bound: Ty) -> bool;

    /// The dependency declaration a prepared object's member overrides, when `symbol` names one.
    ///
    /// The record is the shared override: the declaring classifier, and the declared and applied
    /// signatures. `supertypes` are the object's. `property` selects a getter edge rather than a
    /// function edge. `None` leaves the member without an edge.
    fn dependency_override(
        &self,
        symbol: &KlibIrSymbol,
        supertypes: &[Ty],
        property: bool,
    ) -> Lowered<Option<super::same_library_class::MemberOverride>> {
        let _ = (symbol, supertypes, property);
        Ok(None)
    }

    /// The provider callable `identity` publishes an override edge as.
    fn external_override(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> Lowered<crate::fir::ExternalCallableId> {
        let _ = identity;
        unsupported("an object literal override")
    }

    /// Whether the selected classifier `name` is an interface.
    fn is_interface(&self, name: TypeName) -> Lowered<bool> {
        let _ = name;
        unsupported("an object literal supertype")
    }
}

/// What a constructor call does with the constructor it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ConstructorRole {
    /// Allocates an instance of the constructor's class.
    Allocation,
    /// Initializes a subclass instance another constructor allocated.
    Delegation,
}

/// The dependency constructor a constructor call invokes: the provider's identity, the class it
/// constructs and its declared value parameters, and, when the linking unit materializes that
/// class, the constructor of the materialized class the call selects.
pub(super) struct LinkedConstructor {
    pub(super) target: crate::fir::ExternalCallableId,
    pub(super) declaration: super::constructor_calls::ConstructorDeclaration,
    pub(super) materialized: Option<crate::ir::IrConstructorTarget>,
}

/// The dependency property a call of one of its accessors reads or writes, as a source access of
/// the property is checked: the provider's property identity, whether the accessor is its setter,
/// and the accessor's dispatch receiver, parameters (context parameters, extension receiver, and a
/// setter's value) and result.
pub(super) struct LinkedAccessor {
    pub(super) property: crate::fir::ExternalPropertyId,
    pub(super) write: bool,
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
    /// As [`LinkedCallee::class_type_parameters`].
    pub(super) class_type_parameters: Vec<String>,
    /// The exact provider-selected external accessor called by this property operation, when the
    /// target is a dependency property.
    pub(super) callable: Option<crate::fir::ExternalCallableId>,
    /// This accessor's own formal identities and bounds, distinct from declaring-class formals.
    pub(super) type_parameters: Vec<crate::types::TyParamName>,
    pub(super) type_parameter_bounds: Vec<Vec<Ty>>,
    pub(super) reified_type_parameters: Vec<u32>,
}

/// The value of a serialized constant typed `ty`, and the type the lowered constant has: `null`
/// is the null value, typed `Nothing?`; any other constant has its value's own type.
pub(super) fn constant_value(constant: &KlibIrConstant, ty: Ty) -> Lowered<(IrConst, Ty)> {
    let (value, constant_ty) = match constant {
        KlibIrConstant::Null => {
            if ty != Ty::nullable(Ty::Nothing) {
                return Err(mismatch("a `null` is not typed `Nothing?`"));
            }
            return Ok((IrConst::Null, Ty::Null));
        }
        KlibIrConstant::Boolean(value) => (IrConst::Boolean(*value), Ty::Boolean),
        KlibIrConstant::Char(value) => (IrConst::Char(*value), Ty::Char),
        KlibIrConstant::Byte(value) => (IrConst::Byte(*value), Ty::Byte),
        KlibIrConstant::Short(value) => (IrConst::Short(*value), Ty::Short),
        KlibIrConstant::Int(value) => (IrConst::Int(*value), Ty::Int),
        KlibIrConstant::Long(value) => (IrConst::Long(*value), Ty::Long),
        KlibIrConstant::Float(value) => (IrConst::Float(*value), Ty::Float),
        KlibIrConstant::Double(value) => (IrConst::Double(*value), Ty::Double),
        KlibIrConstant::String(value) => (IrConst::String(value.as_str().into()), Ty::String),
    };
    if constant_ty != ty {
        // An unsigned constant, for one, is serialized as its signed value typed `UInt`.
        return unsupported("a constant of another type than its value's");
    }
    Ok((value, ty))
}

/// The static storage a top-level property's backing field is realized as in a unit.
#[derive(Clone, Copy)]
pub(super) struct StaticField {
    /// The property's entry in the unit's `IrFile::statics`.
    pub(super) index: u32,
    pub(super) ty: Ty,
    /// Whether a body may store to it: the field is not final.
    pub(super) writable: bool,
}

/// What an accessor call reaches.
pub(super) enum LinkedAccessorCall {
    /// The provider's property, read or written through its accessor.
    Property(LinkedAccessor),
    /// The accessor function this unit emits, called as any function of the unit is: a member of
    /// a class the unit materializes, or a top-level accessor, generic ones among them.
    Function(LinkedCallee),
}

/// The function a body is lowered into: its serialized symbol, the only target its `return`s may
/// name, and the unit function that holds it.
#[derive(Clone, Copy)]
pub(super) struct LoweredFunction<'a> {
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) function: FunId,
}

/// Lowers the body of one function into `ir`.
pub(super) struct BodyLowering<'a> {
    arena: &'a KlibIrArena,
    function: LoweredFunction<'a>,
    header: &'a FunctionHeader,
    builtins: &'a IrBuiltinOperators,
    ir: &'a mut IrFile,
    linker: &'a mut dyn CallLinker,
    locals: LocalValues,
    /// Compiler temporaries that name another immutable value, by the initializer each read of
    /// them lowers as.
    compiler_aliases: HashMap<KlibIrSymbol, KlibIrExprId>,
    /// Reads proven non-null where they are lowered: safe-call receiver temporaries inside the
    /// selector their `null` test guards, and fields a compiler-generated `when` tests.
    null_proven: HashSet<NullProvenRead>,
    /// All enclosing return-target declarations, including tail-only blocks without a frame.
    returnable_targets: Vec<KlibIrSymbol>,
    /// Enclosing serialized returnable blocks, whose exact symbols own their local exits.
    returnable_frames: Vec<compiler_blocks::ReturnableFrame>,
    /// The loops enclosing the expression being lowered, innermost last: each serialized loop
    /// identity with the label its `break`s and `continue`s name.
    loops: Vec<(u32, String)>,
    /// Loops whose lowered bodies contain a `break` that names that loop.
    breakable_loops: HashSet<u32>,
    /// The coercions of an erased body's values to their erasure, which a conversion back to the
    /// declared type cancels.
    erasure_coercions: HashSet<ExprId>,
    instance_storage: Option<instance_storage::InstanceStorageFrame<'a>>,
    /// Enclosing values a member of an object literal reads through its constructor fields.
    captures: HashMap<KlibIrSymbol, object_literals::CaptureField>,
    /// The members of the object literals being lowered, by serialized signature.
    object_members:
        HashMap<crate::metadata::klib_ir::KlibIrSignature, object_literals::ObjectMember>,
    /// The member function whose `return`s are being lowered, when that is not the template.
    member_return: Option<KlibIrSymbol>,
    /// The result type of [`Self::member_return`].
    member_result: Option<Ty>,
    /// The declarations enclosing this closure whose `return`s its body may name, innermost last,
    /// with their result types: a non-local return, valid only where the closure is inlined.
    enclosing_returns: Vec<(KlibIrSymbol, Ty)>,
}

impl<'a> BodyLowering<'a> {
    pub(super) fn new(
        arena: &'a KlibIrArena,
        function: LoweredFunction<'a>,
        header: &'a FunctionHeader,
        builtins: &'a IrBuiltinOperators,
        ir: &'a mut IrFile,
        linker: &'a mut dyn CallLinker,
    ) -> Self {
        Self {
            arena,
            function,
            header,
            builtins,
            ir,
            linker,
            locals: LocalValues::after(header.parameter_count()),
            compiler_aliases: HashMap::default(),
            null_proven: HashSet::default(),
            returnable_targets: Vec::new(),
            returnable_frames: Vec::new(),
            loops: Vec::new(),
            breakable_loops: HashSet::default(),
            erasure_coercions: HashSet::default(),
            instance_storage: None,
            captures: HashMap::default(),
            member_return: None,
            member_result: None,
            object_members: HashMap::default(),
            enclosing_returns: Vec::new(),
        }
    }

    /// This body as a member of the materialized class `class`, which `declaring` declares and
    /// which reads its receiver's
    /// `fields`, the backing fields of `properties`. A value class member reads its one value
    /// and writes nothing.
    pub(super) fn reading_through(
        mut self,
        class: crate::ir::ClassId,
        declaring: &'a KlibIrSymbol,
        fields: &[&'a crate::metadata::klib_ir::tree::KlibIrField],
        properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
        value: bool,
    ) -> Self {
        let declared = &self.ir.classes[class as usize].fields;
        self.instance_storage = Some(instance_storage::InstanceStorageFrame::member(
            class,
            declaring,
            fields
                .iter()
                .zip(declared)
                .map(|(field, declared)| (&field.base.symbol, declared.ty, declared.is_final())),
            properties,
            self.header.value_receiver(),
            value,
        ));
        self
    }

    fn statement(&mut self, statement: &KlibIrStatement) -> Lowered<ExprId> {
        match statement {
            KlibIrStatement::Expression(expression) => self.discarded(*expression),
            KlibIrStatement::Variable(variable) => self.variable(*variable),
            KlibIrStatement::Function(_) => unsupported("a local function"),
            KlibIrStatement::Class(_) => unsupported("a local class"),
            KlibIrStatement::LocalDelegatedProperty(_) => unsupported("a local delegated property"),
            KlibIrStatement::TypeAlias(_) => unsupported("a local type alias"),
        }
    }

    /// `id` evaluated for its effect alone: a statement of a block, a block's trailing value or a
    /// branch of a `Unit` `when`. A KLIB coerces a value of another type to `Unit` there; the
    /// source states no conversion, and checked FIR lowering lowers the value itself.
    fn discarded(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let expression = self.arena.expr(id);
        if let KlibIrExprKind::TypeOperator {
            operator: KlibIrTypeOperator::ImplicitCoercionToUnit,
            operand,
            argument,
        } = &expression.kind
        {
            if self.semantic(*operand)? != Ty::Unit
                || self.ty(expression, "coercion to `Unit`")? != Ty::Unit
            {
                return Err(mismatch("a coercion to `Unit` is typed apart from `Unit`"));
            }
            return self.expression(*argument);
        }
        self.expression(id)
    }

    fn expression(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let arena = self.arena;
        let expression = arena.expr(id);
        let (lowered, ty) = match &expression.kind {
            KlibIrExprKind::Const(constant) => {
                let ty = self.ty(expression, "constant")?;
                self.constant(constant, ty)?
            }
            KlibIrExprKind::GetValue { symbol, origin: _ } => {
                let ty = self.ty(expression, "value read")?;
                let read = self.read(symbol)?;
                (read, self.read_type(symbol, ty))
            }
            KlibIrExprKind::Call {
                access,
                super_qualifier,
            } => {
                let ty = self.ty(expression, "call")?;
                if let Some(qualifier) = super_qualifier {
                    (self.super_call(qualifier, access, ty)?, ty)
                } else {
                    let lowered = match super::range_membership::range_literal_membership(
                        arena, expression,
                    ) {
                        Some(membership) => self.range_literal_membership(membership, ty, false)?,
                        None => None,
                    };
                    match lowered {
                        Some(lowered) => (lowered, ty),
                        None => (self.call(access, ty)?, ty),
                    }
                }
            }
            KlibIrExprKind::Return { target, value } => {
                let ty = self.ty(expression, "`return`")?;
                (self.return_value(target, *value)?, ty)
            }
            KlibIrExprKind::When { branches, origin } => {
                let ty = self.ty(expression, "`when`")?;
                (self.conditional(branches, origin.as_deref(), ty)?, ty)
            }
            KlibIrExprKind::SetValue {
                symbol,
                value,
                origin: _,
            } => {
                let ty = self.ty(expression, "assignment")?;
                (self.assignment(symbol, *value, ty)?, ty)
            }
            KlibIrExprKind::ConstructorCall {
                access,
                constructor_type_arguments,
            } => {
                let ty = self.ty(expression, "constructor call")?;
                (
                    self.constructor_call(access, *constructor_type_arguments, ty)?,
                    ty,
                )
            }
            KlibIrExprKind::DelegatingConstructorCall(_) => {
                return unsupported("a delegating constructor call")
            }
            KlibIrExprKind::EnumConstructorCall(_) => {
                return unsupported("an enum constructor call")
            }
            KlibIrExprKind::FunctionReference { .. } => return unsupported("a function reference"),
            KlibIrExprKind::RichFunctionReference {
                invoke,
                overridden,
                origin,
                ..
            } => {
                if let Some(reference) =
                    super::callable_references::declaration_reference(self.arena, expression)
                {
                    let ty = self.ty(expression, "callable reference")?;
                    let lowered = self.callable_reference(reference, ty)?;
                    return Ok(self.typed(lowered, ty));
                }
                if !super::closure_forms::unbound_rich_reference(&expression.kind) {
                    return unsupported("a bound or reflective rich function reference");
                }
                let ty = self.ty(expression, "closure")?;
                let crate::types::TyKind::Fun(signature) = ty.kind() else {
                    // A lambda converted to a functional interface implements the interface's
                    // one abstract method, the declaration the reference overrides.
                    let ty_id = expression
                        .ty
                        .ok_or_else(|| mismatch("a converted lambda has no serialized type"))?;
                    return self.converted_closure(
                        ty_id,
                        *invoke,
                        overridden,
                        ty,
                        origin.as_deref(),
                    );
                };
                // A suspend lambda overrides `SuspendFunctionN.invoke`, any other `FunctionN.invoke`.
                let invocation = match &overridden.signature {
                    crate::metadata::klib_ir::KlibIrSignature::Public(invoke)
                        if overridden.kind
                            == crate::metadata::klib_ir::KlibIrSymbolKind::Function =>
                    {
                        if signature.suspend {
                            super::klib_types::suspend_function_invocation_arity(invoke)
                        } else {
                            super::klib_types::function_invocation_arity(invoke)
                        }
                    }
                    _ => None,
                };
                if invocation != Some(signature.params.len()) {
                    return Err(mismatch(
                        "a rich closure does not override its function invocation",
                    ));
                }
                (self.closure(*invoke, ty, origin.as_deref())?, ty)
            }
            KlibIrExprKind::RichPropertyReference { .. } => {
                let ty = self.ty(expression, "property reference")?;
                return self.field_designation(expression, ty);
            }
            KlibIrExprKind::PropertyReference { .. } => return unsupported("a property reference"),
            KlibIrExprKind::LocalDelegatedPropertyReference { .. } => {
                return unsupported("a local delegated property reference")
            }
            KlibIrExprKind::Block {
                statements,
                origin: None,
            } => {
                // A block's type is its source block's, which checked FIR lowering records itself.
                self.ty(expression, "block")?;
                return self.block(statements);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if origin == "ELVIS" => {
                let ty = self.ty(expression, "Elvis block")?;
                return self.elvis_block(statements, ty);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if matches!(
                origin.as_str(),
                "INLINE_ARGS_CONTAINER"
                    | "ARGUMENTS_REORDERING_FOR_CALL"
                    | "WHEN"
                    | "PLUSEQ"
                    | "MINUSEQ"
                    | "MULTEQ"
                    | "DIVEQ"
                    | "PERCEQ"
                    | "POSTFIX_INCR"
                    | "POSTFIX_DECR"
                    | "PREFIX_INCR"
                    | "PREFIX_DECR"
            ) =>
            {
                let ty = self.ty(expression, "compiler expression block")?;
                return self.temporary_expression_block(statements, ty);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if origin == "SAFE_CALL" => {
                let ty = self.ty(expression, "safe call")?;
                return self.safe_call_block(statements, ty);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if origin == "FOR_LOOP" => {
                let ty = self.ty(expression, "`for` loop")?;
                return self.for_loop(statements, ty);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if origin == "OBJECT_LITERAL" => return self.object_literal(statements),
            KlibIrExprKind::Block { .. } => {
                return unsupported("a block of a compiler-introduced form");
            }
            KlibIrExprKind::Composite { .. } => return unsupported("a composite block"),
            KlibIrExprKind::ReturnableBlock {
                symbol,
                statements,
                origin: None,
            } => {
                let ty = self.ty(expression, "returnable block")?;
                return self.returnable_block(symbol, statements, ty);
            }
            KlibIrExprKind::ReturnableBlock { .. } => {
                return unsupported("a returnable block of a compiler-introduced form")
            }
            KlibIrExprKind::InlinedFunctionBlock { .. } => {
                return unsupported("an inlined function block")
            }
            KlibIrExprKind::TypeOperator {
                operator,
                operand,
                argument,
            } => {
                let ty = self.ty(expression, "type operator")?;
                return self.type_operator(*operator, *operand, *argument, ty);
            }
            KlibIrExprKind::GetField {
                symbol,
                receiver,
                super_qualifier,
                ..
            } => {
                let ty = self.ty(expression, "field read")?;
                return self.initialized_field(
                    symbol,
                    super_qualifier.as_ref(),
                    *receiver,
                    None,
                    ty,
                );
            }
            KlibIrExprKind::SetField {
                symbol,
                receiver,
                value,
                super_qualifier,
                ..
            } => {
                let ty = self.ty(expression, "field write")?;
                return self.initialized_field(
                    symbol,
                    super_qualifier.as_ref(),
                    *receiver,
                    Some(*value),
                    ty,
                );
            }
            KlibIrExprKind::GetObject(symbol) => {
                let ty = self.ty(expression, "object value")?;
                return self.object_value(symbol, ty);
            }
            KlibIrExprKind::GetClass(value) => {
                let ty = self.ty(expression, "class literal")?;
                return self.value_class_literal(*value, ty);
            }
            KlibIrExprKind::ClassReference { class, class_type } => {
                let ty = self.ty(expression, "class literal")?;
                return self.class_reference(class, *class_type, ty);
            }
            KlibIrExprKind::GetEnumValue(symbol) => {
                let ty = self.ty(expression, "enum entry")?;
                return self.enum_entry_value(symbol, ty);
            }
            KlibIrExprKind::Break { loop_id, label: _ } => {
                let ty = self.ty(expression, "`break`")?;
                (self.jump(*loop_id, ty, false)?, ty)
            }
            KlibIrExprKind::Continue { loop_id, label: _ } => {
                let ty = self.ty(expression, "`continue`")?;
                (self.jump(*loop_id, ty, true)?, ty)
            }
            KlibIrExprKind::While(serialized) => {
                // A loop is a statement: checked FIR lowering records no type for it.
                let ty = self.ty(expression, "loop")?;
                return self.loop_statement(serialized, false, ty);
            }
            KlibIrExprKind::DoWhile(serialized) => {
                let ty = self.ty(expression, "loop")?;
                return self.loop_statement(serialized, true, ty);
            }
            KlibIrExprKind::InstanceInitializerCall(_) => {
                return unsupported("an instance initializer call")
            }
            KlibIrExprKind::StringConcat(parts) => {
                let ty = self.ty(expression, "string template")?;
                (self.string_template(parts, ty)?, ty)
            }
            KlibIrExprKind::Throw(value) => {
                let ty = self.ty(expression, "`throw`")?;
                (self.throw(*value, ty)?, ty)
            }
            KlibIrExprKind::Try {
                result,
                catches,
                finally,
            } => {
                let ty = self.ty(expression, "`try`")?;
                (self.try_expression(*result, catches, *finally, ty)?, ty)
            }
            KlibIrExprKind::Vararg {
                element_type,
                elements,
            } => {
                let ty = self.ty(expression, "a vararg array")?;
                (self.vararg_array(*element_type, elements, ty)?, ty)
            }
            KlibIrExprKind::FunctionExpression { function, origin } => {
                let ty = self.ty(expression, "closure")?;
                (self.closure(*function, ty, origin.as_deref())?, ty)
            }
            KlibIrExprKind::DynamicMember { .. } => return unsupported("a dynamic member access"),
            KlibIrExprKind::DynamicOperator { .. } => return unsupported("a dynamic operator"),
            KlibIrExprKind::Error { .. } => return unsupported("an error expression"),
            KlibIrExprKind::ErrorCall { .. } => return unsupported("an error call"),
            KlibIrExprKind::Missing => return unsupported("an omitted argument"),
        };
        Ok(self.typed(lowered, ty))
    }

    /// Record `ty` as the checked type of the freshly lowered `lowered`, as checked FIR lowering
    /// records the type of every expression it lowers, around the bottom-value completion it
    /// attaches to a `Nothing` producer.
    fn typed(&mut self, lowered: ExprId, ty: Ty) -> ExprId {
        self.ir.set_logical_type(lowered, ty);
        let completed = complete_bottom_value(self.ir, lowered, ty);
        self.ir.set_logical_type(completed, ty);
        completed
    }

    /// The type serialized for `expression`, a `form`.
    fn ty(&self, expression: &KlibIrExpr, form: &'static str) -> Lowered<Ty> {
        let ty = expression
            .ty
            .ok_or(KlibBodyDeclineReason::UntypedExpression(form))?;
        self.semantic(ty)
    }

    /// A type used in this body, including a non-public class of the body's own library.
    fn semantic(&self, id: KlibIrTypeId) -> Lowered<Ty> {
        super::klib_types::semantic_type_in(
            self.arena,
            self.header.type_parameters(),
            id,
            &|signature| self.linker.local_class(signature),
        )
    }

    /// Whether `actual` is an anonymous or local class declared in this body and `expected` is
    /// one of the supertypes that class was lowered with.
    fn local_class_subtype(&self, actual: Ty, expected: Ty) -> bool {
        if actual.is_nullable() && !expected.is_nullable() {
            return false;
        }
        let parameters = self.header.type_parameters();
        self.local_class_supertypes(actual)
            .is_some_and(|mut supertypes| {
                supertypes.any(|supertype| self.linker.is_subtype(parameters, supertype, expected))
            })
    }

    /// The direct supertypes recorded for the local or anonymous class lowered in this body that
    /// the non-null `actual` names; `None` when it names no such class.
    fn local_class_supertypes(&self, actual: Ty) -> Option<impl Iterator<Item = Ty> + '_> {
        let class = self.local_class(actual)?;
        Some(
            class
                .supertypes
                .iter()
                .copied()
                .chain(std::iter::once(Ty::obj_name(class.superclass))),
        )
    }

    /// The generic class `owner` as `sub` applies it through its hierarchy: through the supertypes
    /// recorded for a local or anonymous class lowered in this body, or the calling compilation's
    /// class model otherwise.
    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty> {
        match self.local_class_supertypes(sub) {
            Some(mut supertypes) => {
                supertypes.find_map(|supertype| self.linker.applied_supertype(supertype, owner))
            }
            None => self.linker.applied_supertype(sub, owner),
        }
    }

    /// The anonymous or local class lowered in this body that `ty` names.
    fn local_class(&self, ty: Ty) -> Option<&crate::ir::IrClass> {
        let name = ty.non_null().kotlin_class_internal()?;
        self.ir.classes.iter().find(|class| {
            class.fq_name == name && (class.is_anonymous_object || class.is_local_class)
        })
    }

    /// Whether `supertype` counts as a supertype of `subtype`: one recorded for a local or
    /// anonymous class lowered in this body, or one the calling compilation's class model proves.
    fn is_supertype_of(&self, supertype: Ty, subtype: Ty) -> bool {
        self.local_class_subtype(subtype, supertype)
            || self
                .linker
                .is_subtype(self.header.type_parameters(), subtype, supertype)
    }

    /// Lower `id` as a value used where `expected` is. The serialized body states every
    /// conversion explicitly but the two Kotlin leaves implicit, as checked FIR does: a value is
    /// one of each of its supertypes, with no conversion, and the widening of a non-null value to
    /// a nullable type is marked with the coercion checked FIR lowering places. `null` is a value
    /// of every nullable type. Any other use, such as an erased body's value where its declared
    /// type is expected, is a conversion the body must state.
    fn operand(&mut self, id: KlibIrExprId, expected: Ty) -> Lowered<ExprId> {
        self.operand_capturing(id, expected, None)
    }

    /// [`Self::operand`] for an argument of a call, whose callee declaration `site` names: a
    /// star-projected argument is also a value of the parameter when the call binds the type
    /// parameter the parameter applies there to the capture of that star.
    fn operand_capturing(
        &mut self,
        id: KlibIrExprId,
        expected: Ty,
        site: Option<CaptureSite<'_>>,
    ) -> Lowered<ExprId> {
        let lowered = self.expression(id)?;
        let actual = self.lowered_type(lowered);
        if is_value_of(actual, expected) {
            return Ok(lowered);
        }
        if let Some(widened) = self.nullable_widening(lowered, actual, expected) {
            return Ok(widened);
        }
        if let Some(narrowed) = self.null_proven_field(id, lowered, actual, expected) {
            return Ok(narrowed);
        }
        if site.is_some_and(|site| captures_star_projection(actual, expected, site)) {
            return Ok(lowered);
        }
        // A body a compiler plugin generates may use a value of a type parameter's declared bound
        // as a value of the parameter, as an atomicfu helper initializes a `T` from the `Any?` its
        // field intrinsic reads. kotlinc's backends represent both by the parameter's erasure; the
        // value crosses with the coercion a generic declaration's boundary places.
        if expected.is_ty_param() && expected.ty_param_bound() == Some(actual) {
            let coerced = self.coercion(lowered, expected);
            return Ok(self.typed(coerced, expected));
        }
        self.upcast(lowered, actual, expected)
            .ok_or(KlibBodyDeclineReason::ImplicitConversion)
    }

    /// `value`, of the type `actual`, as a value of its supertype `expected`: the value itself, or
    /// the coercion checked FIR lowering places when nullability or value/reference shape changes.
    /// `None` when `expected` is not a supertype of `actual`.
    fn upcast(&mut self, value: ExprId, actual: Ty, expected: Ty) -> Option<ExprId> {
        if !self.is_supertype_of(expected, actual) {
            return None;
        }
        Some(
            if actual.is_nullable() == expected.is_nullable()
                && actual.is_reference() == expected.is_reference()
            {
                value
            } else {
                let coerced = self.coercion(value, expected);
                self.typed(coerced, expected)
            },
        )
    }

    /// The type recorded for an expression this lowering produced.
    fn lowered_type(&self, expression: ExprId) -> Ty {
        self.ir
            .checked_type(expression)
            .expect("every lowered value is typed")
    }

    /// The constant `constant` the KLIB types `ty`, with the type checked FIR lowering records for
    /// it: `null` is serialized as `Nothing?` and typed as the null literal.
    fn constant(&mut self, constant: &KlibIrConstant, ty: Ty) -> Lowered<(ExprId, Ty)> {
        let (value, constant_ty) = constant_value(constant, ty)?;
        Ok((self.ir.add_expr(IrExpr::Const(value)), constant_ty))
    }

    fn read(&mut self, symbol: &KlibIrSymbol) -> Lowered<ExprId> {
        if let Some(capture) = self.captures.get(symbol).copied() {
            let receiver = self.ir.add_expr(IrExpr::GetValue(0));
            self.ir
                .set_logical_type(receiver, Ty::obj_name(capture.class_name));
            return Ok(self.ir.add_expr(IrExpr::GetField {
                receiver,
                class: capture.class,
                index: capture.index,
            }));
        }
        if let Some(alias) = self.compiler_aliases.get(symbol).copied() {
            return self.expression(alias);
        }
        let (index, stability, erased) = if let Some(local) = self.locals.get(symbol) {
            (local.index, Some(local.stability()), None)
        } else {
            let parameter = self
                .header
                .value(symbol)
                .ok_or(KlibBodyDeclineReason::ForeignValue)?;
            let stability = parameter.stable_read.then_some(IrBindingStability::Stable);
            (parameter.index, stability, parameter.erased)
        };
        let read = self.ir.add_expr(IrExpr::GetValue(index));
        if let Some(stability) = stability {
            self.ir.binding_read_stability.insert(read, stability);
        }
        Ok(match erased {
            Some(erased) => {
                // The read itself is of the declared type; its use is of the erasure.
                let declared = self.header.slot_type(index);
                self.ir.set_logical_type(read, declared);
                let coercion = self.coercion(read, erased);
                self.erasure_coercions.insert(coercion);
                coercion
            }
            None => read,
        })
    }

    /// The type a read of `symbol` the body types `serialized` has: a function-typed parameter of
    /// an erased body is read at its declared type, which its invocations convert from. The
    /// erasure also drops the markers of a function type's context parameters and receiver.
    fn read_type(&self, symbol: &KlibIrSymbol, serialized: Ty) -> Ty {
        if self.locals.get(symbol).is_some() {
            return serialized;
        }
        match self.header.value(symbol) {
            Some(parameter) if parameter.erased.is_none() => {
                let declared = self.header.slot_type(parameter.index);
                let erased = self.header.erased(declared);
                if erased == serialized
                    || super::function_lowering::plain_function_type(erased) == serialized
                {
                    declared
                } else {
                    serialized
                }
            }
            _ => serialized,
        }
    }

    /// `value`, of an erased type, as a value of the declared type `target`. A coercion of a
    /// declared value to its erasure cancels, and a conditional converts each branch, so the
    /// expanded body converts no value the source does not.
    fn declared_value(&mut self, value: ExprId, target: Ty) -> ExprId {
        let actual = self.lowered_type(value);
        if is_value_of(actual, target) || actual == Ty::Nothing {
            return value;
        }
        match self.ir.expr(value).clone() {
            IrExpr::TypeOp { arg, .. } if self.erasure_coercions.contains(&value) => {
                let inner = self.lowered_type(arg);
                if is_value_of(inner, target) {
                    return arg;
                }
                if let Some(widened) = self.nullable_widening(arg, inner, target) {
                    return widened;
                }
            }
            IrExpr::When { branches } => {
                let branches = branches
                    .into_iter()
                    .map(|(condition, result)| (condition, self.declared_value(result, target)))
                    .collect();
                self.ir.exprs[value as usize] = IrExpr::When { branches };
                self.ir.set_logical_type(value, target);
                if self.ir.whens.exhaustive.contains_key(&value) {
                    self.ir.whens.exhaustive.insert(value, target);
                }
                return value;
            }
            _ => {}
        }
        self.coercion(value, target)
    }

    /// `value` converted to `ty` by the coercion checked FIR lowering places at a generic
    /// declaration's boundary, which the target represents.
    fn coercion(&mut self, value: ExprId, ty: Ty) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: ty,
        })
    }

    /// Lower `id` as a value of the declared type `declared` where the body produces its erasure,
    /// as a prepared-inline copy passes an erased value to a function parameter's invocation.
    fn declared_operand(&mut self, id: KlibIrExprId, declared: Ty) -> Lowered<ExprId> {
        let lowered = self.expression(id)?;
        self.declared_lowered(lowered, declared)
    }

    /// The lowered `lowered` as a value of the declared type `declared`; see
    /// [`Self::declared_operand`].
    fn declared_lowered(&mut self, lowered: ExprId, declared: Ty) -> Lowered<ExprId> {
        let actual = self.lowered_type(lowered);
        if is_value_of(actual, declared) {
            return Ok(lowered);
        }
        let parameters = self.header.type_parameters();
        // A value of a subtype of `declared`, as `suspendCoroutine` passes its `SafeContinuation`
        // to a `(Continuation<T>) -> Unit` block, is one by subsumption, and so is an instance of
        // a local or anonymous class this body lowers, by its recorded supertypes. It crosses as
        // the checked source argument does, with the conversion the checker decides for the same
        // two types.
        if self.is_supertype_of(declared, actual) {
            return Ok(match crate::types::value_conversion(actual, declared) {
                None => lowered,
                Some(_) => self.coercion(lowered, declared),
            });
        }
        // A prepared copy also erases the type parameters of the inline functions it already
        // expanded, so the value may be typed with the erasure's bound: a supertype of `declared`.
        if actual != self.header.erased(declared)
            && !self.linker.is_subtype(parameters, declared, actual)
        {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        Ok(self.declared_value(lowered, declared))
    }

    /// A local `val` or `var` with its initializer, lowered as checked FIR lowers the source
    /// declaration: a named variable in the next value slot, with no type of its own.
    fn variable(&mut self, id: KlibIrVariableId) -> Lowered<ExprId> {
        let variable = self.arena.variable(id);
        // A compiler temporary, such as the cast `other` of a data class's `equals`, is an unnamed
        // immutable local the source never names.
        if variable.base.origin == "IR_TEMPORARY_VARIABLE" {
            return self.unnamed_variable(id);
        }
        let ty = self.semantic(variable.ty)?;
        let Some(initializer) = variable.initializer else {
            return self.deferred_variable(id, ty);
        };
        let init = self.operand(initializer, ty)?;
        // Initializers can declare locals of their own. Allocate the enclosing declaration only
        // after those locals, as checked FIR does, and keep its symbol invisible until then.
        let local = self.locals.slot(variable, ty)?;
        self.locals.declare(&variable.base.symbol, local)?;
        self.locals
            .record_name(&variable.base.symbol, &variable.name);
        Ok(self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(init),
            named: true,
        }))
    }

    /// A local declared without an initializer and assigned later (`val x: Int` followed by
    /// `x = …` on every path kotlinc's definite-assignment analysis proved). Checked FIR lowers the
    /// source declaration as a reassignable binding holding a target-neutral zero and records the
    /// declared type as deferred, so each backend chooses that type's physical zero.
    fn deferred_variable(&mut self, id: KlibIrVariableId, ty: Ty) -> Lowered<ExprId> {
        let variable = self.arena.variable(id);
        let local = self.locals.deferred(variable, ty)?;
        self.locals.declare(&variable.base.symbol, local)?;
        self.locals
            .record_name(&variable.base.symbol, &variable.name);
        let zero = self.ir.add_expr(IrExpr::Const(deferred_zero(ty)));
        self.ir.set_logical_type(zero, ty);
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(zero),
            named: true,
        });
        self.ir.deferred_local_types.insert(declaration, ty);
        Ok(declaration)
    }

    /// An assignment to a local `var`, typed `Unit` as the source assignment is.
    fn assignment(
        &mut self,
        symbol: &KlibIrSymbol,
        value: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some(local) = self.locals.get(symbol) else {
            return Err(match self.header.value(symbol) {
                Some(_) => mismatch("a body assigns a parameter"),
                None => KlibBodyDeclineReason::ForeignValue,
            });
        };
        if !local.mutable {
            return unsupported("an assignment that initializes a `val`");
        }
        if ty != Ty::Unit {
            return Err(mismatch("an assignment is not typed `Unit`"));
        }
        let value = self.operand(value, local.ty)?;
        Ok(self.ir.add_expr(IrExpr::SetValue {
            var: local.index,
            value,
        }))
    }

    fn return_value(&mut self, target: &KlibIrSymbol, value: KlibIrExprId) -> Lowered<ExprId> {
        if let Some(returned) = self.returnable_frame_exit(target, value)? {
            return Ok(returned);
        }
        let member = self
            .member_return
            .as_ref()
            .is_some_and(|member| member == target);
        if !member && target != self.function.symbol {
            return self.non_local_return(target, value);
        }
        let expected = self.member_result.unwrap_or(self.header.body_result());
        let mut value = if !member && self.header.erases_result() {
            // An erased body leaves the smart cast of its result to a type parameter out with the
            // parameter (`checkNotNull` returns its `value: T?` as `T` after its `null` test), and
            // Kotlin's inliner casts the inlined result to the call's result type implicitly.
            let lowered = self.expression(value)?;
            self.implicit_cast_of(lowered, expected, expected)?
        } else {
            self.operand(value, expected)?
        };
        if !member && self.header.body_result() != self.header.result() {
            value = self.declared_value(value, self.header.result());
        }
        let returned = self.ir.add_expr(IrExpr::Return(Some(value)));
        // The function's own return leaves the innermost callable.
        self.ir.checked_return_depths.insert(returned, 0);
        Ok(returned)
    }

    /// A closure's `return` from the enclosing declaration `target`, which checked FIR lowering
    /// records as a return as many lexical frames out as the closures it crosses. Inline expansion
    /// moves it a frame nearer at each one.
    fn non_local_return(&mut self, target: &KlibIrSymbol, value: KlibIrExprId) -> Lowered<ExprId> {
        let Some((frames, &(_, result))) = self
            .enclosing_returns
            .iter()
            .rev()
            .enumerate()
            .find(|(_, (symbol, _))| symbol == target)
        else {
            return Err(KlibBodyDeclineReason::ForeignReturnTarget);
        };
        let value = self.operand(value, result)?;
        let returned = self.ir.add_expr(IrExpr::Return(Some(value)));
        let depth = u32::try_from(frames + 1).expect("a closure nesting fits u32");
        self.ir.checked_return_depths.insert(returned, depth);
        Ok(returned)
    }

    /// `throw` of the value `value`, which checked FIR lowering lowers as the source operand.
    fn throw(&mut self, value: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::Nothing {
            return Err(mismatch("a `throw` is not typed `Nothing`"));
        }
        let operand = self.expression(value)?;
        Ok(self.ir.add_expr(IrExpr::Throw { operand }))
    }
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

fn unsupported<T>(form: &'static str) -> Lowered<T> {
    Err(KlibBodyDeclineReason::UnsupportedOperation(form))
}

/// A value typed `actual` is one of `expected` without a conversion: the same type, or the null
/// literal where a nullable type is expected, which checked FIR lowering converts nothing for.
fn is_value_of(actual: Ty, expected: Ty) -> bool {
    actual == expected || (actual == Ty::Null && expected.is_nullable())
}

/// The callee declaration a call argument is passed to, as the callee declares it before the
/// call binds its type parameters.
#[derive(Clone, Copy)]
struct CaptureSite<'a> {
    /// The declared type of the parameter the argument is passed to.
    declared: Ty,
    /// Every declared parameter of the callee.
    parameters: &'a [Ty],
    /// The type parameters of the callee itself.
    formals: &'a [crate::types::TyParamName],
}

/// A value typed `actual` is one of `expected` through the capture of a star projection: the
/// call binds a type parameter of its callee to the star, which the serialized call states as
/// the type parameter's bound (`Key<*>` passed to `get<E : Element>(key: Key<E>)` is read at
/// `Key<Element>`).
///
/// Every slot where the types differ must be a star projection of `actual` against a type
/// parameter of the callee that the declaration applies in that slot alone, and `expected` must
/// state that parameter's bound there. A parameter the declaration fixes (`Key<Element>`) or a
/// type parameter the call does not bind is no capture and is read as written.
fn captures_star_projection(actual: Ty, expected: Ty, site: CaptureSite<'_>) -> bool {
    let (
        Some((class, actual_args)),
        Some((expected_class, expected_args)),
        Some((owner, declared_args)),
    ) = (
        actual.obj_parts(),
        expected.obj_parts(),
        site.declared.obj_parts(),
    )
    else {
        return false;
    };
    if class != expected_class
        || class != owner
        || actual_args.len() != expected_args.len()
        || actual_args.len() != declared_args.len()
    {
        return false;
    }
    let mut captured = false;
    for (slot, ((have, want), declared)) in actual_args
        .iter()
        .zip(expected_args)
        .zip(declared_args)
        .enumerate()
    {
        if have == want {
            continue;
        }
        let crate::types::TyKind::TyParam(name, bound) = declared.kind() else {
            return false;
        };
        let sole = !declared_args
            .iter()
            .enumerate()
            .any(|(other, argument)| other != slot && calls::names(*argument, name))
            && site
                .parameters
                .iter()
                .filter(|parameter| calls::names(**parameter, name))
                .count()
                == 1;
        if !have.is_star_projection() || !site.formals.contains(&name) || !sole || *want != bound {
            return false;
        }
        captured = true;
    }
    captured
}

/// The target-neutral zero a deferred local of type `ty` holds before its first assignment, as the
/// source parser supplies it: the primitive's zero, and `null` for every other type (a backend
/// reads the deferred type to choose the physical zero it stores).
fn deferred_zero(ty: Ty) -> IrConst {
    match ty {
        Ty::Boolean => IrConst::Boolean(false),
        Ty::Byte => IrConst::Byte(0),
        Ty::Short => IrConst::Short(0),
        Ty::Int => IrConst::Int(0),
        Ty::Long => IrConst::Long(0),
        Ty::Float => IrConst::Float(0.0),
        Ty::Double => IrConst::Double(0.0),
        Ty::Char => IrConst::Char(0),
        _ => IrConst::Null,
    }
}

#[cfg(test)]
mod capture_conversion_tests {
    use super::{captures_star_projection, CaptureSite};
    use crate::types::{Ty, TyParamName};

    fn key(argument: Ty) -> Ty {
        Ty::obj_args("demo/Key", &[argument])
    }

    fn star() -> Ty {
        Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))
    }

    fn formal() -> (TyParamName, Ty) {
        let name = TyParamName::from("E");
        (name, Ty::ty_param("E", Ty::obj("demo/Element")))
    }

    #[test]
    fn a_star_is_captured_by_a_type_parameter_of_the_callee_at_its_bound() {
        let (name, e) = formal();
        let declared = key(e);
        let site = CaptureSite {
            declared,
            parameters: &[declared],
            formals: &[name],
        };
        assert!(captures_star_projection(
            key(star()),
            key(Ty::obj("demo/Element")),
            site
        ));
    }

    #[test]
    fn a_star_is_not_captured_where_the_declaration_fixes_or_does_not_bind_the_slot() {
        let (name, e) = formal();
        let element = Ty::obj("demo/Element");
        // The declared parameter is fixed to `Key<Element>`.
        let fixed = key(element);
        assert!(!captures_star_projection(
            key(star()),
            key(element),
            CaptureSite {
                declared: fixed,
                parameters: &[fixed],
                formals: &[name]
            }
        ));
        // The parameter applies a type parameter the callee does not declare.
        let rigid = key(e);
        assert!(!captures_star_projection(
            key(star()),
            key(element),
            CaptureSite {
                declared: rigid,
                parameters: &[rigid],
                formals: &[]
            }
        ));
        // Another parameter constrains the same type parameter too.
        let declared = key(e);
        assert!(!captures_star_projection(
            key(star()),
            key(element),
            CaptureSite {
                declared,
                parameters: &[declared, e],
                formals: &[name]
            }
        ));
    }

    #[test]
    fn a_capture_states_the_bound_and_only_a_star_is_captured() {
        let (name, e) = formal();
        let declared = key(e);
        let site = CaptureSite {
            declared,
            parameters: &[declared],
            formals: &[name],
        };
        let element = Ty::obj("demo/Element");
        // Another type argument than the bound.
        assert!(!captures_star_projection(
            key(star()),
            key(Ty::obj("demo/Other")),
            site
        ));
        // A concrete argument is not a capture.
        assert!(!captures_star_projection(
            key(Ty::obj("demo/Other")),
            key(element),
            site
        ));
        // Another class or another nullability.
        assert!(!captures_star_projection(
            Ty::obj_args("demo/Lock", &[star()]),
            key(element),
            site
        ));
        assert!(!captures_star_projection(
            key(star()),
            Ty::nullable(key(element)),
            site
        ));
    }
}
