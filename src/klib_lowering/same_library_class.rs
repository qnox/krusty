//! A non-public class used by a body of the library that declares it.
//!
//! A file-local signature names a declaration only inside the KLIB that serialized it. The same
//! file and slot in another library are a different declaration and stay refused. The class is
//! not published, so lowering materializes it in the unit that lowers the body, and the
//! constructor call builds that class instead of an external dependency constructor.
//!
//! Fields are stored from their initializer expressions, in member order, through the same
//! initialization steps a public class uses. Supertypes written on the class are the supertypes
//! subtyping reads: `Any`, because the constructor's delegation to it was authenticated, or an
//! interface this library's producer graph serializes. Any other hierarchy is refused before the
//! class is admitted.
//!
//! A class private to its enclosing class is linked the same way. Its signatures are public, but
//! no provider publishes it and no other library names it.

use super::class_initialization::{record_initializer, InitializationStep};
use super::class_storage::{member_field, stored_field};
use super::constructor_calls::{require_constructor_kind, ConstructorDeclaration};
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::{local_initializer_header, local_member_header, FunctionHeader};
use super::klib_types::{semantic_type_in, KlibTypeParameters};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    ClassId, IrClass, IrConstructorAccess, IrConstructorTarget, IrCtorArg,
    IrCtorParameterProvenance, IrField,
};
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies, LocatedClass, LocatedMember};
use crate::libraries::{KlibBodyClassParameters, KlibDependencyCallable, TypeKind};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrFunction,
    KlibIrMember, KlibIrMemberAccess, KlibIrStatement, KlibIrType, KlibIrTypeParameter,
    KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::semantic::{
    class_is_value, class_kind, declaration_modality, declaration_visibility, KotlinModality,
};
use crate::types::{Ty, TyKind, TypeName, TypeVariance, Visibility};

/// The module and file-local signature that identify one declaration inside one library.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct LocalIdentity {
    pub(super) module: String,
    pub(super) signature: KlibIrSignature,
}

/// The constructor a same-library file-local construction invokes.
pub(super) struct LocalConstructor {
    pub(super) declaration: ConstructorDeclaration,
    pub(super) access: IrConstructorAccess,
}

/// One file-local class materialized in the unit that lowers its library's bodies.
pub(super) struct MaterializedLocalClass {
    pub(super) class: ClassId,
    pub(super) classifier: TypeName,
    pub(super) constructor: KlibIrSignature,
    pub(super) declaration: ConstructorDeclaration,
    pub(super) access: IrConstructorAccess,
    pub(super) formals: Vec<Ty>,
    /// The supertypes written on [`Self::class`]. Subtyping reads this same list.
    pub(super) supertypes: Vec<Ty>,
    pub(super) self_type: Ty,
    pub(super) class_type_parameters: Vec<String>,
}

/// A field the class initializer and its members store through, in declaration order.
#[derive(Clone, Copy)]
pub(super) struct LocalStoredField<'a> {
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) ty: Ty,
    pub(super) final_: bool,
}

/// The initializer frame queued for a file-local class.
pub(super) struct LocalInitialize<'a> {
    pub(super) class: ClassId,
    /// The serialized declaration of `class`.
    pub(super) declaring: &'a KlibIrSymbol,
    pub(super) receiver_symbol: &'a KlibIrSymbol,
    pub(super) receiver_ty: Ty,
    pub(super) fields: Vec<LocalStoredField<'a>>,
    pub(super) steps: Vec<InitializationStep<'a>>,
    pub(super) outer: Option<OuterInstance<'a>>,
}

/// The field frame of one member of a file-local class.
pub(super) struct LocalMemberFields<'a> {
    pub(super) class: ClassId,
    /// The serialized declaration of `class`.
    pub(super) declaring: &'a KlibIrSymbol,
    pub(super) receiver: Ty,
    pub(super) fields: Vec<LocalStoredField<'a>>,
    pub(super) outer: Option<OuterInstance<'a>>,
}

/// Work a file-local class adds to the body queue.
pub(super) enum LocalBody<'a> {
    Initialize(LocalInitialize<'a>),
    Member(LocalMemberFields<'a>),
}

/// Where a lowering reads the library that owns the body, and how it checks a root delegation.
/// The prepared class borrows only the library; the module name and the root check do not.
pub(super) struct LocalClassSource<'bodies, 'call> {
    pub(super) bodies: &'bodies KlibDeclarationBodies,
    pub(super) declarations: &'bodies dyn crate::symbol_source::SymbolSource,
    pub(super) module: &'call str,
    pub(super) validate_root:
        &'call dyn Fn(&KlibPublicIdSignature) -> Result<(), KlibBodyDeclineReason>,
}

/// A class ready to install, or the constructor of one already installed for this library.
/// The prepared class is boxed so the ready constructor does not carry its frame.
pub(super) enum LocalLink<'a> {
    Ready(LocalConstructor),
    Prepared(Box<PreparedLocalClass<'a>>),
}

/// One member function, declared before its body is lowered so an initializer can call it.
pub(super) struct PreparedMember<'a> {
    pub(super) signature: KlibIrSignature,
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) statements: &'a [KlibIrStatement],
    pub(super) header: FunctionHeader,
    /// Interface members this function overrides, authenticated against the admitted supertypes.
    pub(super) overrides: Vec<MemberOverride>,
    pub(super) visibility: Visibility,
}

/// An overridden member, joined by its checked dependency identity.
///
/// `declared_*` is the declaration's signature with its own type parameters. `applied_*` is that
/// signature after substituting the implementing type's supertype arguments. Neither is the
/// implementation member's signature.
pub(super) struct MemberOverride {
    pub(super) identity: KlibDependencyCallable,
    pub(super) owner: TypeName,
    pub(super) declared_parameters: Vec<Ty>,
    pub(super) declared_result: Ty,
    pub(super) applied_parameters: Vec<Ty>,
    pub(super) applied_result: Ty,
    pub(super) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(super) is_interface: bool,
}

/// `parameters` and `result` after substituting each declaring type parameter in `formals` with
/// the supertype argument in the same position. A parameter the arguments do not mention stays.
/// The two lists are one substitution: a different count is not a signature.
pub(super) fn substitute_applied_signature(
    parameters: &[Ty],
    result: Ty,
    formals: &[Ty],
    arguments: &[Ty],
) -> Result<(Vec<Ty>, Ty), KlibBodyDeclineReason> {
    if formals.len() != arguments.len() {
        return Err(mismatch(
            "an override's declaring type has another number of type arguments",
        ));
    }
    if formals.is_empty() {
        return Ok((parameters.to_vec(), result));
    }
    let mut bindings = HashMap::default();
    for (formal, argument) in formals.iter().copied().zip(arguments.iter().copied()) {
        let Some(name) = formal.ty_param_name() else {
            return Err(mismatch(
                "a declaring type parameter is not a type parameter",
            ));
        };
        if bindings
            .insert(crate::types::TyParamName::new(name), argument)
            .is_some()
        {
            return Err(mismatch("a declaring type repeats a type parameter"));
        }
    }
    Ok((
        parameters
            .iter()
            .copied()
            .map(|parameter| crate::types::ty_subst_applied_arguments(parameter, &bindings))
            .collect(),
        crate::types::ty_subst_applied_arguments(result, &bindings),
    ))
}

/// The override edge of an object member to the declaration `identity`, which the producer
/// published as the member's overridden symbol.
///
/// `declarations` is the provider's registry. `None` when it realizes no declaration of that exact
/// identity. A getter answers only a property member and a function only a function member; a
/// setter is never an object literal's override.
pub(super) fn declared_override(
    identity: KlibDependencyCallable,
    declarations: &dyn crate::symbol_source::SymbolSource,
    supertypes: &[Ty],
    property: bool,
    applied_supertype: &dyn Fn(Ty, TypeName) -> Option<Ty>,
) -> Result<Option<MemberOverride>, KlibBodyDeclineReason> {
    let Some((_, realization)) = declarations.external_callable_by_dependency(&identity) else {
        return Ok(None);
    };
    let getter = realization
        .klib_body_accessor
        .as_ref()
        .map(|accessor| matches!(accessor.role, crate::libraries::KlibAccessorRole::Getter));
    match getter {
        Some(false) => {
            return Err(mismatch("an object literal overrides a property setter"));
        }
        Some(true) if !property => return Err(mismatch("a function overrides a property")),
        None if property => return Err(mismatch("a property overrides a function")),
        Some(true) | None => {}
    }
    realized_member_override(
        identity,
        &realization,
        property,
        supertypes,
        applied_supertype,
    )
    .map(Some)
}

/// The override `realization` declares, viewed through one of `supertypes`.
///
/// The owner is the realization's classifier. The applied signature substitutes that classifier's
/// own type parameters with the arguments of the supertype whose hierarchy reaches it. A
/// supertype is never selected by its position. `property` declines a getter whose declaration
/// still has value parameters.
pub(super) fn realized_member_override(
    identity: KlibDependencyCallable,
    realization: &crate::libraries::ExternalCallableRealization,
    property: bool,
    supertypes: &[Ty],
    applied_supertype: &dyn Fn(Ty, TypeName) -> Option<Ty>,
) -> Result<MemberOverride, KlibBodyDeclineReason> {
    let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
        realization.declaration_owner
    else {
        return Err(mismatch(
            "an override's declaration has no classifier owner",
        ));
    };
    if realization
        .klib_body_class_contract
        .as_ref()
        .is_some_and(|contract| contract.classifier != owner)
    {
        return Err(mismatch("an override's class contract names another owner"));
    }
    let (declared_parameters, declared_result) = declared_member_signature(realization);
    if property && !declared_parameters.is_empty() {
        return Err(mismatch(
            "an object literal overrides a property with parameters",
        ));
    }
    let applied = supertype_application(supertypes, owner, applied_supertype)?;
    let formals = declaring_formals(realization, applied.type_args())?;
    let (applied_parameters, applied_result) = substitute_applied_signature(
        &declared_parameters,
        declared_result,
        &formals,
        applied.type_args(),
    )?;
    Ok(MemberOverride {
        identity,
        owner,
        declared_parameters,
        declared_result,
        applied_parameters,
        applied_result,
        semantic_role: realization.callable.semantic_role,
        is_interface: realization
            .klib_body_class_contract
            .as_ref()
            .is_some_and(|contract| contract.kind == TypeKind::Interface),
    })
}

/// The declaration's own parameters and result, before the implementing type's arguments.
fn declared_member_signature(
    realization: &crate::libraries::ExternalCallableRealization,
) -> (Vec<Ty>, Ty) {
    let callable = &realization.callable;
    let parameters = callable
        .declared_params
        .clone()
        .map(|parameters| parameters.to_vec())
        .or_else(|| {
            callable.generic_sig.as_ref().map(|signature| {
                if realization.kind == crate::libraries::ExternalCallableKind::Extension {
                    signature
                        .parameters_with_receiver(callable.context_count)
                        .to_vec()
                } else {
                    signature.params.clone()
                }
            })
        })
        .unwrap_or_else(|| callable.params.clone());
    let result = callable
        .declared_ret
        .or_else(|| callable.generic_sig.as_ref().map(|signature| signature.ret))
        .unwrap_or(callable.ret);
    (parameters, result)
}

/// The classifier's own type parameters, when `arguments` is the application that views them.
fn declaring_formals(
    realization: &crate::libraries::ExternalCallableRealization,
    arguments: &[Ty],
) -> Result<Vec<Ty>, KlibBodyDeclineReason> {
    let Some(contract) = realization.klib_body_class_contract.as_ref() else {
        if arguments.is_empty() {
            return Ok(Vec::new());
        }
        return Err(mismatch(
            "an override's declaring class has no type-parameter contract",
        ));
    };
    let formals = &contract.parameters.formals;
    let own = contract.parameters.own_count;
    if own > formals.len() || arguments.len() != own {
        return Err(mismatch(
            "an override's declaring type has another number of type arguments",
        ));
    }
    Ok(formals[..own].to_vec())
}

/// The one supertype of the object whose hierarchy is `owner`, with that classifier's arguments.
fn supertype_application(
    supertypes: &[Ty],
    owner: TypeName,
    applied_supertype: &dyn Fn(Ty, TypeName) -> Option<Ty>,
) -> Result<Ty, KlibBodyDeclineReason> {
    let mut found = None;
    for supertype in supertypes.iter().copied() {
        let candidate = supertype.non_null();
        let view = if candidate.kotlin_class_internal() == Some(owner) {
            Some(candidate)
        } else {
            applied_supertype(candidate, owner)
        };
        let Some(view) = view else {
            continue;
        };
        if view.non_null().kotlin_class_internal() != Some(owner) {
            return Err(mismatch("an applied supertype names another classifier"));
        }
        if found.replace(view.non_null()).is_some() {
            return Err(mismatch("an object repeats one overridden supertype"));
        }
    }
    found.ok_or_else(|| mismatch("an override's declaring type is not a supertype of the object"))
}

#[derive(Clone)]
struct AdmittedInterface {
    name: TypeName,
    signature: KlibIrSignature,
    arguments: Vec<Ty>,
}

/// A class's declared supertypes: every one in order, the interfaces among them, its
/// superclass other than `Any`, and, when that is a library class, its application.
struct AuthenticatedSupertypes {
    supertypes: Vec<Ty>,
    interfaces: Vec<AdmittedInterface>,
    superclass: Option<TypeName>,
    library_superclass: Option<AdmittedInterface>,
    inherited: InheritedSupertypes,
}

/// The supertypes a class reaches only through its library supertypes, each applied as the class
/// applies it. The linker follows an override edge naming a member a direct supertype inherits
/// without overriding it to that member's declaration in one of these: `EnumEntriesList.get`
/// overrides `EnumEntries.get`, which `EnumEntries` inherits from `List`.
#[derive(Default)]
struct InheritedSupertypes {
    interfaces: Vec<AdmittedInterface>,
    classes: Vec<AdmittedInterface>,
}

struct InterfaceSignature {
    parameters: Vec<Ty>,
    result: Ty,
}

/// A file-local class authenticated and not yet added to the unit.
pub(super) struct PreparedLocalClass<'a> {
    pub(super) key: LocalIdentity,
    pub(super) classifier: TypeName,
    pub(super) self_type: Ty,
    pub(super) supertypes: Vec<Ty>,
    pub(super) interfaces: Vec<TypeName>,
    pub(super) fields: Vec<IrField>,
    pub(super) stored: Vec<LocalStoredField<'a>>,
    pub(super) ctor_args: Vec<IrCtorArg>,
    pub(super) declaration: ConstructorDeclaration,
    pub(super) access: IrConstructorAccess,
    pub(super) formals: Vec<Ty>,
    pub(super) class_type_parameters: Vec<String>,
    /// The declared bounds of each class type parameter, in its order.
    pub(super) class_type_parameter_bounds: Vec<Vec<Ty>>,
    pub(super) constructor_symbol: &'a KlibIrSymbol,
    /// The serialized declaration of the class.
    pub(super) declaring: &'a KlibIrSymbol,
    pub(super) arena: &'a KlibIrArena,
    pub(super) receiver_symbol: &'a KlibIrSymbol,
    pub(super) receiver_ty: Ty,
    pub(super) init_header: FunctionHeader,
    pub(super) steps: Vec<InitializationStep<'a>>,
    pub(super) members: Vec<PreparedMember<'a>>,
    /// An inner class's outer instance.
    pub(super) outer: Option<OuterInstance<'a>>,
    /// The same-library superclass the constructor delegates to; `Any` otherwise.
    pub(super) superclass: Option<LocalSuperclass<'a>>,
    pub(super) open: bool,
}

/// A same-library superclass constructor a class's constructor delegates to, and the slot of the
/// class constructor's parameter each of its arguments passes.
pub(super) struct LocalSuperclass<'a> {
    pub(super) constructor: SuperclassConstructor<'a>,
    pub(super) operands: Vec<u32>,
}

/// The constructor a same-library class's constructor delegates to.
#[derive(Clone, Copy)]
pub(super) enum SuperclassConstructor<'a> {
    /// A constructor of another class only this library's bodies link.
    Local(&'a KlibIrSignature),
    /// A published constructor of the library class `classifier`.
    Library {
        signature: &'a KlibPublicIdSignature,
        classifier: TypeName,
    },
}

/// The outer instance an inner class holds: the outer class's `this`, which the inner class's
/// bodies read, and the field the constructor stores it in.
#[derive(Clone, Copy)]
pub(super) struct OuterInstance<'a> {
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) field: u32,
}

/// The classifier identity of a class only `module`'s bodies link. A file-local class is named
/// by its file and slot, not its source spelling. A class private to its enclosing class keeps
/// the qualified name its public signature serializes, as every type that mentions it does.
pub(super) fn classifier_name(module: &str, signature: &KlibIrSignature) -> Option<TypeName> {
    let (file, slot) = match signature {
        KlibIrSignature::FileLocal { file, slot } => (file, slot),
        KlibIrSignature::Public(public) => {
            return super::klib_types::classifier_name(public).ok();
        }
        KlibIrSignature::Accessor(_) => return None,
    };
    Some(
        TypeName::ROOT
            .nested_child("klib-local")
            .nested_child(module)
            .nested_child(&file.to_string())
            .nested_child(&slot.to_string()),
    )
}

/// The classifier `module` itself declares at `signature`, when that declaration is a class.
pub(super) fn classifier(
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &KlibIrSignature,
) -> Result<Option<TypeName>, KlibBodyDeclineReason> {
    let Some(name) = classifier_name(module, signature) else {
        return Ok(None);
    };
    let Some((_, class)) = bodies.module_local_class(module, signature) else {
        return Ok(None);
    };
    if class.base.symbol.kind != KlibIrSymbolKind::Class
        || class.base.symbol.signature != *signature
    {
        return Ok(None);
    }
    if class_is_value(class.base.flags) || class.value_class_representation.is_some() {
        return Err(unsupported("a non-public value class"));
    }
    match class_kind(class.base.flags) {
        TypeKind::Class => Ok(Some(name)),
        _ => Err(unsupported("a non-public classifier other than a class")),
    }
}

/// Whether a same-library class materialized in `classes` makes `sub` a subtype of `sup`.
/// A class from another library is absent here, so it stays outside this relation.
pub(super) fn is_subtype(
    classes: &HashMap<LocalIdentity, MaterializedLocalClass>,
    sub: Ty,
    sup: Ty,
    public_subtype: &dyn Fn(Ty, Ty) -> bool,
) -> bool {
    public_subtype(sub, sup) || proves_local(classes, sub, sup, public_subtype, 0)
}

fn proves_local(
    classes: &HashMap<LocalIdentity, MaterializedLocalClass>,
    sub: Ty,
    sup: Ty,
    public_subtype: &dyn Fn(Ty, Ty) -> bool,
    depth: u8,
) -> bool {
    if depth == 8 {
        return false;
    }
    if let TyKind::Nullable(inner) = sub.kind() {
        return match sup.kind() {
            TyKind::Nullable(target) => {
                proves_local(classes, inner, sup, public_subtype, depth + 1)
                    || proves_local(classes, inner, target, public_subtype, depth + 1)
            }
            _ => false,
        };
    }
    let Some(supertypes) = applied_local_supertypes(classes, sub) else {
        return false;
    };
    for applied in supertypes {
        if applied == sub {
            continue;
        }
        if applied == sup
            || public_subtype(applied, sup)
            || proves_local(classes, applied, sup, public_subtype, depth + 1)
        {
            return true;
        }
    }
    false
}

/// The supertypes the materialized class `sub` names declares, at the type arguments `sub`
/// applies to the class. `None` when `sub` is not a materialized local class.
fn applied_local_supertypes(
    classes: &HashMap<LocalIdentity, MaterializedLocalClass>,
    sub: Ty,
) -> Option<Vec<Ty>> {
    let TyKind::Obj(name, arguments) = sub.kind() else {
        return None;
    };
    let local = classes.values().find(|class| class.classifier == name)?;
    if arguments.len() != local.formals.len() {
        return None;
    }
    let mut bindings = HashMap::default();
    for (formal, argument) in local.formals.iter().zip(arguments.iter()) {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return None;
        };
        bindings.insert(identity.as_str().to_owned(), *argument);
    }
    Some(
        local
            .supertypes
            .iter()
            .map(|declared| crate::types::ty_subst_applied_arguments(*declared, &bindings))
            .collect(),
    )
}

/// `owner` as the non-null `sub` applies it: `sub` itself, or the supertype a materialized local
/// class's hierarchy applies, ending at one the calling compilation's class model applies.
pub(super) fn applied_supertype(
    classes: &HashMap<LocalIdentity, MaterializedLocalClass>,
    sub: Ty,
    owner: TypeName,
    public: &dyn Fn(Ty, TypeName) -> Option<Ty>,
) -> Option<Ty> {
    applied_local_supertype(classes, sub, owner, public, 0)
}

fn applied_local_supertype(
    classes: &HashMap<LocalIdentity, MaterializedLocalClass>,
    sub: Ty,
    owner: TypeName,
    public: &dyn Fn(Ty, TypeName) -> Option<Ty>,
    depth: u8,
) -> Option<Ty> {
    if depth == 8 {
        return None;
    }
    if sub.kotlin_class_internal() == Some(owner) {
        return Some(sub);
    }
    let Some(supertypes) = applied_local_supertypes(classes, sub) else {
        return public(sub, owner);
    };
    supertypes
        .into_iter()
        .find_map(|applied| applied_local_supertype(classes, applied, owner, public, depth + 1))
}

/// The call shape of a member already declared for `class`.
pub(super) fn linked_member(
    class: &MaterializedLocalClass,
    function: crate::ir::FunId,
    ir: &crate::ir::IrFile,
    defaults: Vec<bool>,
) -> super::body_lowering::LinkedCallee {
    let declared = &ir.functions[function as usize];
    super::body_lowering::LinkedCallee {
        callee: crate::ir::Callee::Local(function),
        receiver: Some(class.self_type),
        params: declared.params.clone(),
        ret: declared.ret,
        context_count: 0,
        singleton_dispatch: None,
        type_parameters: Vec::new(),
        type_parameter_bounds: Some(Vec::new()),
        class_type_parameters: class.class_type_parameters.clone(),
        parameter_defaults: defaults,
        edge: super::CallEdge::default(),
    }
}

/// The file-local constructor `signature` of the body module. A class already materialized for
/// this library answers immediately; a new one is prepared and left for the linker to install.
pub(super) fn link<'bodies>(
    source: LocalClassSource<'bodies, '_>,
    signature: &KlibIrSignature,
    cache: &HashMap<LocalIdentity, MaterializedLocalClass>,
) -> Result<LocalLink<'bodies>, KlibBodyDeclineReason> {
    let Some((arena, function)) = source
        .bodies
        .module_local_function(source.module, signature)
    else {
        return Err(unsupported("a call of a file-private constructor"));
    };
    require_constructor_kind(function)?;
    if function.extension_receiver.is_some()
        || !function.context_parameters.is_empty()
        || function.companion_extension_class.is_some()
        || !function.overridden.is_empty()
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor receiver or context",
        ));
    }
    if !function.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "constructor-owned type parameters",
        ));
    }
    let class_signature = constructor_class(arena, function)?;
    let key = LocalIdentity {
        module: source.module.to_owned(),
        signature: class_signature,
    };
    if let Some(known) = cache.get(&key) {
        if known.constructor != *signature {
            return Err(unsupported("a non-public class with another constructor"));
        }
        return Ok(LocalLink::Ready(LocalConstructor {
            declaration: known.declaration.clone(),
            access: known.access,
        }));
    }
    let Some((class_arena, class)) = source
        .bodies
        .module_local_class(source.module, &key.signature)
    else {
        return Err(non_public_type());
    };
    if !std::ptr::eq(arena, class_arena) {
        return Err(mismatch("a constructor is not a member of its class"));
    }
    // Only an inner class's constructor takes the outer instance as its dispatch receiver.
    if function.dispatch_receiver.is_some()
        != crate::metadata::semantic::class_is_inner(class.base.flags)
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor receiver or context",
        ));
    }
    Ok(LocalLink::Prepared(Box::new(prepare(
        source, signature, arena, class, function, key,
    )?)))
}

fn constructor_class(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<KlibIrSignature, KlibBodyDeclineReason> {
    let KlibIrType::Simple {
        classifier,
        arguments,
        ..
    } = arena.ty(function.return_type)
    else {
        return Err(mismatch("a constructor result names another class"));
    };
    if classifier.kind != KlibIrSymbolKind::Class {
        return Err(mismatch("a constructor result names another class"));
    }
    let _ = arguments;
    match &classifier.signature {
        KlibIrSignature::FileLocal { .. } | KlibIrSignature::Public(_) => {
            Ok(classifier.signature.clone())
        }
        KlibIrSignature::Accessor(_) => Err(non_public_type()),
    }
}

fn prepare<'a>(
    source: LocalClassSource<'a, '_>,
    signature: &KlibIrSignature,
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    function: &'a KlibIrFunction,
    key: LocalIdentity,
) -> Result<PreparedLocalClass<'a>, KlibBodyDeclineReason> {
    let class_name = classifier(source.bodies, source.module, &class.base.symbol.signature)?
        .ok_or_else(non_public_type)?;
    let resolve = |signature: &KlibIrSignature| classifier(source.bodies, source.module, signature);
    // An inner class also reads its outer class's type parameters, which its type applies after
    // its own.
    let outer = match &function.dispatch_receiver {
        Some(_) => Some(
            arena
                .enclosing_class(class)
                .ok_or_else(|| mismatch("an inner class is not a member of a class"))?,
        ),
        None => None,
    };
    let formals = class_formals(arena, class, outer, &resolve)?;
    prepare_class(
        source,
        signature,
        ClassDeclaration {
            arena,
            class,
            function,
            key,
            class_name,
            outer,
        },
        formals,
    )
}

/// One class declaration and the constructor that builds it, with the identity it is cached by.
struct ClassDeclaration<'a> {
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    function: &'a KlibIrFunction,
    key: LocalIdentity,
    class_name: TypeName,
    /// The class an inner class's instance holds an instance of.
    outer: Option<&'a KlibIrClass>,
}

fn prepare_class<'a>(
    source: LocalClassSource<'a, '_>,
    signature: &KlibIrSignature,
    declaration: ClassDeclaration<'a>,
    formals: ClassFormals,
) -> Result<PreparedLocalClass<'a>, KlibBodyDeclineReason> {
    let ClassDeclaration {
        arena,
        class,
        function,
        key,
        class_name,
        outer,
    } = declaration;
    if declaration_modality(class.base.flags).is_abstract() {
        return Err(unsupported("a non-public abstract class"));
    }
    if !class.sealed_subclasses.is_empty() {
        return Err(unsupported("a non-public sealed class"));
    }
    if class
        .members
        .iter()
        .any(|member| matches!(member, KlibIrMember::EnumEntry(_)))
    {
        return Err(unsupported("a non-public enum"));
    }
    let constructors = class
        .members
        .iter()
        .filter_map(|member| match member {
            KlibIrMember::Function(id) => {
                let member = arena.function(*id);
                member.constructor.then_some(member)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if constructors.len() != 1 || constructors[0].base.symbol.signature != *signature {
        return Err(unsupported("a non-public class with another constructor"));
    }
    let delegation =
        superclass_delegation(arena, &class.base.symbol, function, source.validate_root)?;
    let resolve = |signature: &KlibIrSignature| classifier(source.bodies, source.module, signature);
    let ClassFormals {
        types,
        formals,
        variances,
        bounds,
    } = formals;
    let AuthenticatedSupertypes {
        supertypes,
        interfaces,
        superclass,
        library_superclass,
        inherited,
    } = authenticate_supertypes(source.bodies, source.module, arena, class, &types, &resolve)?;
    let self_type = Ty::obj_args_name(class_name, &formals);
    let result = semantic_type_in(arena, &types, function.return_type, &resolve)?;
    if result != self_type {
        return Err(mismatch(
            "a constructor result is not its declared class application",
        ));
    }
    let Some(receiver) = &class.this_receiver else {
        return Err(mismatch("a class initializer has no instance receiver"));
    };
    if semantic_type_in(arena, &types, receiver.ty, &resolve)? != self_type {
        return Err(mismatch(
            "a class instance receiver has another declared type",
        ));
    }
    let scope = MemberScope {
        types: &types,
        resolve: &resolve,
        self_type,
        bodies: source.bodies,
        declarations: source.declarations,
        module: source.module,
        interfaces: &interfaces,
        library_superclass: library_superclass.as_ref(),
        inherited: &inherited,
    };
    let StoredMembers {
        mut fields,
        stored,
        steps,
        members,
    } = members_and_storage(arena, class, &scope)?;
    let ConstructorParameters {
        mut params,
        mut defaults,
        arguments: mut ctor_args,
    } = constructor_parameters(arena, function, &types, &resolve)?;
    // An inner class stores the outer instance its constructor receives first in a field after
    // its own, before the superclass constructor runs, and its members read the outer class's
    // `this` from there.
    let outer_instance = match (outer, &function.dispatch_receiver) {
        (Some(outer), Some(receiver)) => {
            let ty = semantic_type_in(arena, &types, receiver.ty, &resolve)?;
            let this = outer
                .this_receiver
                .as_ref()
                .ok_or_else(|| mismatch("an outer class has no instance receiver"))?;
            if semantic_type_in(arena, &types, this.ty, &resolve)? != ty {
                return Err(mismatch(
                    "an inner class constructor receives another outer instance",
                ));
            }
            let field = u32::try_from(fields.len()).expect("a field list fits u32");
            fields.push(IrField::new("this$0".to_owned(), ty).with_is_final(true));
            params.insert(0, ty);
            defaults.insert(0, false);
            ctor_args.insert(
                0,
                IrCtorArg {
                    name: None,
                    context_kind: crate::types::ContextParameterKind::None,
                    ty,
                    declared_ty: None,
                    is_field: true,
                    field_index: Some(field),
                    has_default: false,
                    is_vararg: false,
                    type_param: None,
                    check: None,
                    anonymous_super_forward: None,
                    capture: None,
                    provenance: IrCtorParameterProvenance::EnclosingInstance,
                    capture_identity: None,
                },
            );
            Some(OuterInstance {
                symbol: &this.base.symbol,
                field,
            })
        }
        (None, None) => None,
        _ => return Err(mismatch("an inner class constructor has no outer instance")),
    };
    let superclass = local_superclass(
        source,
        LocalDelegation {
            arena,
            function,
            delegation,
            superclass,
            outer: outer_instance,
        },
    )?;
    let class_type_parameters = formals
        .iter()
        .map(|formal| match formal.kind() {
            TyKind::TyParam(identity, _) => Ok(identity.as_str().to_owned()),
            _ => Err(mismatch("a non-public class type parameter has no symbol")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let init_header =
        local_initializer_header(function, receiver, self_type, arena, &types, &resolve)?;
    let class_type_parameter_bounds = bounds.clone();
    Ok(PreparedLocalClass {
        key,
        classifier: class_name,
        self_type,
        supertypes,
        interfaces: interfaces.iter().map(|interface| interface.name).collect(),
        fields,
        stored,
        ctor_args,
        declaration: {
            let declaration = ConstructorDeclaration::local(
                class_name,
                params,
                KlibBodyClassParameters {
                    formals: formals.clone().into_boxed_slice(),
                    bounds: bounds.into_boxed_slice(),
                    variances: variances.into_boxed_slice(),
                    own_count: class.type_parameters.len(),
                },
                defaults.into_iter().collect(),
            );
            if outer_instance.is_some() {
                declaration.with_enclosing_instance()
            } else {
                declaration
            }
        },
        access: match declaration_visibility(function.base.flags) {
            Visibility::Private => IrConstructorAccess::Private,
            _ => IrConstructorAccess::Unrestricted,
        },
        formals,
        class_type_parameters,
        class_type_parameter_bounds,
        constructor_symbol: &function.base.symbol,
        declaring: &class.base.symbol,
        arena,
        receiver_symbol: &receiver.base.symbol,
        receiver_ty: self_type,
        init_header,
        steps,
        members,
        outer: outer_instance,
        superclass,
        open: declaration_modality(class.base.flags) != KotlinModality::Final,
    })
}

/// A class's constructor delegation to the constructor of its same-library superclass, and what
/// it checks that delegation against.
struct LocalDelegation<'a, 'o> {
    arena: &'a KlibIrArena,
    function: &'a KlibIrFunction,
    delegation: Option<&'a KlibIrMemberAccess>,
    superclass: Option<TypeName>,
    outer: Option<OuterInstance<'o>>,
}

/// The same-library superclass a class's constructor delegates to, with the constructor slot of
/// each argument it passes. The delegation passes the constructor's own parameters, an inner
/// class its outer instance among them; any other argument keeps the class unsupported.
fn local_superclass<'a>(
    source: LocalClassSource<'a, '_>,
    delegation: LocalDelegation<'a, '_>,
) -> Result<Option<LocalSuperclass<'a>>, KlibBodyDeclineReason> {
    let LocalDelegation {
        arena,
        function,
        delegation,
        superclass,
        outer,
    } = delegation;
    let (access, superclass) = match (delegation, superclass) {
        (None, None) => return Ok(None),
        (Some(access), Some(superclass)) => (access, superclass),
        _ => {
            return Err(mismatch(
                "a constructor delegates apart from its class's superclass",
            ))
        }
    };
    let constructor = match source
        .bodies
        .module_local_function(source.module, &access.symbol.signature)
    {
        Some((super_arena, super_function)) => {
            let super_class = constructor_class(super_arena, super_function)?;
            if classifier(source.bodies, source.module, &super_class)? != Some(superclass) {
                return Err(mismatch(
                    "a constructor delegates to another class than its superclass",
                ));
            }
            SuperclassConstructor::Local(&access.symbol.signature)
        }
        None => match &access.symbol.signature {
            // A library superclass's constructor is published; the linker proves its class.
            KlibIrSignature::Public(signature) => SuperclassConstructor::Library {
                signature,
                classifier: superclass,
            },
            _ => {
                return Err(mismatch(
                    "a superclass constructor has no linkable signature",
                ))
            }
        },
    };
    let prefix = u32::from(outer.is_some());
    let slot = |symbol: &KlibIrSymbol| -> Option<u32> {
        if let Some(outer) = outer {
            let receiver = function.dispatch_receiver.as_ref();
            if symbol == outer.symbol || receiver.is_some_and(|r| &r.base.symbol == symbol) {
                return Some(1);
            }
        }
        function
            .regular_parameters
            .iter()
            .position(|parameter| &parameter.base.symbol == symbol)
            .map(|index| 1 + prefix + u32::try_from(index).expect("a parameter index fits u32"))
    };
    let arguments = match &access.arguments {
        KlibIrArguments::Flat(values) => values.clone(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver: None,
            values,
        } => dispatch_receiver
            .iter()
            .map(|receiver| Some(*receiver))
            .chain(values.iter().copied())
            .collect(),
        KlibIrArguments::Split { .. } => {
            return Err(mismatch(
                "a superclass delegation passes an extension receiver",
            ))
        }
    };
    let operands = arguments
        .into_iter()
        .map(|argument| {
            let argument = argument.ok_or(unsupported(
                "a superclass delegation leaving a parameter to its default",
            ))?;
            match &arena.expr(argument).kind {
                KlibIrExprKind::GetValue { symbol, .. } => slot(symbol).ok_or(unsupported(
                    "a superclass constructor argument other than a constructor parameter",
                )),
                _ => Err(unsupported(
                    "a superclass constructor argument other than a constructor parameter",
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(LocalSuperclass {
        constructor,
        operands,
    }))
}

fn authenticate_supertypes(
    bodies: &KlibDeclarationBodies,
    module: &str,
    arena: &KlibIrArena,
    class: &KlibIrClass,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<AuthenticatedSupertypes, KlibBodyDeclineReason> {
    let mut supertypes = Vec::new();
    let mut interfaces = Vec::new();
    let mut superclass = None;
    let mut library_superclass = None;
    for supertype in &class.supertypes {
        let ty = semantic_type_in(arena, types, *supertype, resolve)?;
        if ty == Ty::obj_name(crate::types::wk::any()) {
            supertypes.push(ty);
            continue;
        }
        let KlibIrType::Simple { classifier, .. } = arena.ty(*supertype) else {
            return Err(unsupported("a superclass other than Any"));
        };
        // A same-library class is the superclass, which the class's constructor delegates to.
        if let Some(local) = resolve(&classifier.signature)? {
            if superclass.replace(local).is_some() {
                return Err(mismatch("a class lists two superclasses"));
            }
            supertypes.push(ty);
            continue;
        }
        let KlibIrSignature::Public(signature) = &classifier.signature else {
            return Err(unsupported("a non-public supertype"));
        };
        match bodies.public_class(module, signature) {
            LocatedClass::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a supertype this library does not authenticate",
                ))
            }
            LocatedClass::Several => return Err(mismatch("two libraries serialize one supertype")),
            LocatedClass::One(_, found) => {
                // A library class is the superclass, which the unit materializes from its own
                // initialization when the class's constructor delegates to it.
                if class_kind(found.base.flags) == TypeKind::Class {
                    let TyKind::Obj(name, _) = ty.kind() else {
                        return Err(mismatch("a superclass is not a class"));
                    };
                    if superclass.replace(name).is_some() {
                        return Err(mismatch("a class lists two superclasses"));
                    }
                    library_superclass = Some(AdmittedInterface {
                        name,
                        signature: classifier.signature.clone(),
                        arguments: ty.type_args().to_vec(),
                    });
                    supertypes.push(ty);
                    continue;
                }
                if class_kind(found.base.flags) != TypeKind::Interface {
                    return Err(unsupported("a superclass other than Any"));
                }
                let TyKind::Obj(name, _) = ty.kind() else {
                    return Err(mismatch("an interface supertype is not a class"));
                };
                interfaces.push(AdmittedInterface {
                    name,
                    signature: classifier.signature.clone(),
                    arguments: ty.type_args().to_vec(),
                });
                supertypes.push(ty);
            }
        }
    }
    let inherited = inherited_supertypes(
        bodies,
        module,
        interfaces.iter().chain(library_superclass.as_ref()),
        resolve,
    )?;
    Ok(AuthenticatedSupertypes {
        supertypes,
        interfaces,
        superclass,
        library_superclass,
        inherited,
    })
}

/// The supertypes the library supertypes in `direct` reach in turn, other than `Any` and those in
/// `direct`, each applied through the application of the supertype that names it.
fn inherited_supertypes<'a>(
    bodies: &KlibDeclarationBodies,
    module: &str,
    direct: impl Iterator<Item = &'a AdmittedInterface>,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<InheritedSupertypes, KlibBodyDeclineReason> {
    let mut reached: Vec<AdmittedInterface> = direct.cloned().collect();
    let direct_count = reached.len();
    let mut inherited = InheritedSupertypes::default();
    let mut next = 0;
    while let Some(current) = reached.get(next).cloned() {
        next += 1;
        let KlibIrSignature::Public(signature) = &current.signature else {
            return Err(unsupported("a non-public supertype"));
        };
        let (arena, class) = match bodies.public_class(module, signature) {
            LocatedClass::One(arena, class) => (arena, class),
            LocatedClass::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a supertype this library does not authenticate",
                ))
            }
            LocatedClass::Several => return Err(mismatch("two libraries serialize one supertype")),
        };
        if next > direct_count {
            match class_kind(class.base.flags) {
                TypeKind::Interface => inherited.interfaces.push(current.clone()),
                TypeKind::Class => inherited.classes.push(current.clone()),
                _ => {
                    return Err(unsupported(
                        "a supertype other than a class or an interface",
                    ))
                }
            }
        }
        let mut formals = Vec::new();
        let types = bind_interface_parameters(class, |parameter| {
            let formal = Ty::ty_param(
                crate::types::metadata_type_parameter(&parameter.name),
                Ty::nullable(Ty::obj("kotlin/Any")),
            );
            formals.push(formal);
            Ok(formal)
        })?;
        let declared = class
            .supertypes
            .iter()
            .map(|&supertype| semantic_type_in(arena, &types, supertype, resolve))
            .collect::<Result<Vec<_>, _>>()?;
        let (applied, _) = substitute_applied_signature(
            &declared,
            Ty::obj_name(crate::types::wk::any()),
            &formals,
            &current.arguments,
        )?;
        for (&supertype, ty) in class.supertypes.iter().zip(applied) {
            if ty == Ty::obj_name(crate::types::wk::any()) {
                continue;
            }
            let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
                return Err(unsupported(
                    "a supertype other than a class or an interface",
                ));
            };
            let TyKind::Obj(name, _) = ty.kind() else {
                return Err(mismatch("a supertype is not a class"));
            };
            let supertype = AdmittedInterface {
                name,
                signature: classifier.signature.clone(),
                arguments: ty.type_args().to_vec(),
            };
            match reached
                .iter()
                .find(|known| known.signature == supertype.signature)
            {
                Some(known) if known.arguments == supertype.arguments => {}
                Some(_) => {
                    return Err(mismatch(
                        "a class inherits one supertype with two applications",
                    ))
                }
                None => reached.push(supertype),
            }
        }
    }
    Ok(inherited)
}

struct StoredMembers<'a> {
    fields: Vec<IrField>,
    stored: Vec<LocalStoredField<'a>>,
    steps: Vec<InitializationStep<'a>>,
    members: Vec<PreparedMember<'a>>,
}

struct MemberScope<'a> {
    types: &'a KlibTypeParameters,
    resolve: &'a dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
    self_type: Ty,
    bodies: &'a KlibDeclarationBodies,
    declarations: &'a dyn crate::symbol_source::SymbolSource,
    module: &'a str,
    interfaces: &'a [AdmittedInterface],
    library_superclass: Option<&'a AdmittedInterface>,
    inherited: &'a InheritedSupertypes,
}

fn members_and_storage<'a>(
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    scope: &MemberScope<'_>,
) -> Result<StoredMembers<'a>, KlibBodyDeclineReason> {
    let mut fields = Vec::new();
    let mut stored = Vec::new();
    let mut identities = HashSet::default();
    let mut steps = Vec::new();
    let mut members = Vec::new();
    for declaration in &class.members {
        let field = if let Some(field) = member_field(declaration) {
            let index = u32::try_from(fields.len()).expect("a field list fits u32");
            let authenticated = stored_field(field, &mut identities, || {
                semantic_type_in(arena, scope.types, field.ty, scope.resolve)
            })?;
            fields.push(
                IrField::new(authenticated.name, authenticated.ty)
                    .with_is_final(authenticated.final_)
                    .with_is_private(authenticated.private),
            );
            stored.push(LocalStoredField {
                symbol: &field.base.symbol,
                ty: authenticated.ty,
                final_: authenticated.final_,
            });
            Some((index, field))
        } else {
            None
        };
        record_initializer(declaration, field, &mut steps)?;
        match declaration {
            KlibIrMember::Function(id) => {
                let function = arena.function(*id);
                if !function.constructor {
                    members.push(prepare_member(arena, function, scope)?);
                }
            }
            KlibIrMember::Property(property) => {
                for id in [property.getter, property.setter].into_iter().flatten() {
                    members.push(prepare_member(arena, arena.function(id), scope)?);
                }
            }
            KlibIrMember::Field(_) | KlibIrMember::AnonymousInitializer { .. } => {}
            // A nested class is its own declaration, prepared when a body constructs it or
            // calls one of its members. It shares no storage or initialization with this class.
            KlibIrMember::Class(_) => {}
            KlibIrMember::EnumEntry(_) => return Err(unsupported("a non-public enum")),
            KlibIrMember::TypeAlias(_) => {
                return Err(unsupported("a type alias of a non-public class"))
            }
        }
    }
    Ok(StoredMembers {
        fields,
        stored,
        steps,
        members,
    })
}

fn prepare_member<'a>(
    arena: &'a KlibIrArena,
    function: &'a KlibIrFunction,
    scope: &MemberScope<'_>,
) -> Result<PreparedMember<'a>, KlibBodyDeclineReason> {
    let statements = match &function.body {
        Some(KlibIrBody::Block(statements)) => statements.as_slice(),
        Some(KlibIrBody::Synthetic(_)) => {
            return Err(unsupported("a synthetic member of a non-public class"))
        }
        None => return Err(unsupported("a member of a non-public class without a body")),
    };
    Ok(PreparedMember {
        signature: function.base.symbol.signature.clone(),
        symbol: &function.base.symbol,
        statements,
        header: local_member_header(function, arena, scope.types, scope.self_type, scope.resolve)?,
        overrides: member_overrides(function, scope)?,
        visibility: declaration_visibility(function.base.flags),
    })
}

fn member_overrides(
    function: &KlibIrFunction,
    scope: &MemberScope<'_>,
) -> Result<Vec<MemberOverride>, KlibBodyDeclineReason> {
    let mut overrides = Vec::with_capacity(function.overridden.len());
    for symbol in &function.overridden {
        if symbol.kind != KlibIrSymbolKind::Function {
            return Err(unsupported(
                "an override of a declaration other than a function",
            ));
        }
        // A property accessor overrides the interface's accessor by its accessor signature, as a
        // function overrides by its public one; `function_owner` locates either.
        let Some(declaration) = crate::klib_libraries::member_signature(&symbol.signature) else {
            return Err(unsupported("an override of a non-public declaration"));
        };
        let identity = match scope.bodies.linked_origin(scope.module, &declaration) {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override this library does not authenticate",
                ))
            }
            KlibCalleeOrigin::Ambiguous => {
                return Err(mismatch("two libraries serialize one overridden function"))
            }
        };
        let Some((_, realization)) = scope
            .declarations
            .external_callable_by_dependency(&identity)
        else {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "an override this library does not authenticate",
            ));
        };
        let (owner_arena, owner, declared) = match scope.bodies.member_owner(&identity) {
            LocatedMember::One {
                module: owner_module,
                arena,
                class,
                function,
            } => {
                if owner_module != identity.module {
                    return Err(mismatch("an overridden function is not in its class"));
                }
                (arena, class, function)
            }
            LocatedMember::Several => {
                return Err(mismatch("two classes serialize one overridden function"))
            }
            LocatedMember::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override this library does not authenticate",
                ))
            }
        };
        let is_interface = class_kind(owner.base.flags) == TypeKind::Interface;
        let metadata_owner = match realization.declaration_owner {
            Some(crate::types::SemanticCallableOwner::Classifier(owner)) => owner,
            _ => return Err(unsupported("an override without a classifier owner")),
        };
        if realization
            .klib_body_class_contract
            .as_ref()
            .is_none_or(|contract| {
                contract.classifier != metadata_owner
                    || contract.kind != class_kind(owner.base.flags)
                    || contract.module != identity.module
                    || KlibIrSignature::Public(contract.signature.clone())
                        != owner.base.symbol.signature
            })
        {
            return Err(mismatch(
                "an override owner disagrees with its provider contract",
            ));
        }
        let (owner_name, arguments) = if is_interface {
            let Some(interface) = scope
                .interfaces
                .iter()
                .chain(&scope.inherited.interfaces)
                .find(|interface| interface.signature == owner.base.symbol.signature)
            else {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override of a supertype this library does not authenticate",
                ));
            };
            (interface.name, interface.arguments.as_slice())
        } else if metadata_owner == crate::types::wk::any() {
            (metadata_owner, &[][..])
        } else if let Some(superclass) = scope
            .library_superclass
            .into_iter()
            .chain(&scope.inherited.classes)
            .find(|superclass| superclass.signature == owner.base.symbol.signature)
        {
            // A member a library superclass declares, a fake override included, applied as the
            // class applies that superclass.
            (superclass.name, superclass.arguments.as_slice())
        } else {
            return Err(unsupported("an override of a class member"));
        };
        // Locating the member by its signature does not prove membership. The declaring class's
        // raw header must hold it directly: a function as a member, an accessor in its own
        // property group and in the slot of its shape.
        let KlibIrSignature::Public(owner_signature) = &owner.base.symbol.signature else {
            return Err(mismatch(
                "an overridden member's class has no public signature",
            ));
        };
        match scope
            .bodies
            .class_member_header(&identity, owner_signature, None)
        {
            Ok(Some(member)) if std::ptr::eq(member.function, declared) => {}
            Ok(_) => {
                return Err(mismatch(
                    "an overridden member is not a direct member of its class's header",
                ))
            }
            Err(reason) => return Err(mismatch(reason)),
        }
        if !declared.type_parameters.is_empty() || declared.constructor {
            return Err(unsupported(
                "an override of a generic or constructor declaration",
            ));
        }
        let mut formals = Vec::new();
        let declared_types = bind_interface_parameters(owner, |parameter| {
            let formal = Ty::ty_param(
                crate::types::metadata_type_parameter(&parameter.name),
                Ty::nullable(Ty::obj("kotlin/Any")),
            );
            formals.push(formal);
            Ok(formal)
        })?;
        let declared_signature =
            interface_signature(owner_arena, declared, &declared_types, scope.resolve)?;
        if declared_signature.parameters.len() != function.regular_parameters.len() {
            return Err(unsupported(
                "an override whose parameters differ from the interface member",
            ));
        }
        let (applied_parameters, applied_result) = substitute_applied_signature(
            &declared_signature.parameters,
            declared_signature.result,
            &formals,
            arguments,
        )?;
        overrides.push(MemberOverride {
            identity,
            owner: owner_name,
            declared_parameters: declared_signature.parameters,
            declared_result: declared_signature.result,
            applied_parameters,
            applied_result,
            semantic_role: realization.callable.semantic_role,
            is_interface,
        });
    }
    Ok(overrides)
}

fn bind_interface_parameters(
    owner: &KlibIrClass,
    mut formal: impl FnMut(&KlibIrTypeParameter) -> Result<Ty, KlibBodyDeclineReason>,
) -> Result<KlibTypeParameters, KlibBodyDeclineReason> {
    let mut types = KlibTypeParameters::default();
    for parameter in &owner.type_parameters {
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter {
            return Err(mismatch("an interface type parameter has no symbol"));
        }
        if !types.bind(&parameter.base.symbol, formal(parameter)?) {
            return Err(mismatch("an interface repeats a type parameter"));
        }
    }
    Ok(types)
}

fn interface_signature(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<InterfaceSignature, KlibBodyDeclineReason> {
    let parameters = function
        .regular_parameters
        .iter()
        .map(|parameter| semantic_type_in(arena, types, parameter.ty, resolve))
        .collect::<Result<Vec<_>, _>>()?;
    let result = semantic_type_in(arena, types, function.return_type, resolve)?;
    Ok(InterfaceSignature { parameters, result })
}

struct ClassFormals {
    types: KlibTypeParameters,
    formals: Vec<Ty>,
    variances: Vec<TypeVariance>,
    bounds: Vec<Vec<Ty>>,
}

fn class_formals(
    arena: &KlibIrArena,
    class: &KlibIrClass,
    outer: Option<&KlibIrClass>,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<ClassFormals, KlibBodyDeclineReason> {
    let parameters = class
        .type_parameters
        .iter()
        .chain(outer.into_iter().flat_map(|outer| &outer.type_parameters))
        .collect::<Vec<_>>();
    let mut types = KlibTypeParameters::default();
    let mut formals = Vec::with_capacity(parameters.len());
    let mut variances = Vec::with_capacity(parameters.len());
    for &parameter in &parameters {
        if parameter.is_reified() {
            return Err(unsupported(
                "a reified type parameter of a non-public class",
            ));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter {
            return Err(mismatch("a non-public class type parameter has no symbol"));
        }
        let variance = variance(parameter)?;
        let formal = Ty::ty_param(
            crate::types::metadata_type_parameter(&parameter.name),
            Ty::nullable(Ty::obj("kotlin/Any")),
        );
        if !types.bind(&parameter.base.symbol, formal) {
            return Err(mismatch("a non-public class repeats a type parameter"));
        }
        formals.push(formal);
        variances.push(variance);
    }
    let mut bounds = Vec::with_capacity(parameters.len());
    for &parameter in &parameters {
        let mut parameter_bounds = Vec::with_capacity(parameter.supertypes.len());
        for bound in &parameter.supertypes {
            parameter_bounds.push(semantic_type_in(arena, &types, *bound, resolve)?);
        }
        bounds.push(parameter_bounds);
    }
    // Each formal carries its first declared bound, as a provider's formal does: a value of
    // `T : Enum<T>` is an `Enum`, whose members a body calls on it. A bound naming a formal reads
    // it unbounded, as the provider decodes a recursive bound.
    let mut types = KlibTypeParameters::default();
    for ((&parameter, formal), parameter_bounds) in parameters.iter().zip(&mut formals).zip(&bounds)
    {
        if let (Some(&bound), Some(name)) = (parameter_bounds.first(), formal.ty_param_name()) {
            *formal = Ty::ty_param(name, bound);
        }
        if !types.bind(&parameter.base.symbol, *formal) {
            return Err(mismatch("a non-public class repeats a type parameter"));
        }
    }
    Ok(ClassFormals {
        types,
        formals,
        variances,
        bounds,
    })
}

fn variance(parameter: &KlibIrTypeParameter) -> Result<TypeVariance, KlibBodyDeclineReason> {
    match parameter.variance() {
        Some(KlibIrVariance::Invariant) => Ok(TypeVariance::Invariant),
        Some(KlibIrVariance::In) => Ok(TypeVariance::In),
        Some(KlibIrVariance::Out) => Ok(TypeVariance::Out),
        None => Err(mismatch(
            "a non-public class type parameter has no variance",
        )),
    }
}

struct ConstructorParameters {
    params: Vec<Ty>,
    defaults: Vec<bool>,
    arguments: Vec<IrCtorArg>,
}

fn constructor_parameters(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<ConstructorParameters, KlibBodyDeclineReason> {
    let mut params = Vec::with_capacity(function.regular_parameters.len());
    let mut defaults = Vec::with_capacity(function.regular_parameters.len());
    let mut arguments = Vec::with_capacity(function.regular_parameters.len());
    for parameter in &function.regular_parameters {
        if parameter.vararg_element_type.is_some() {
            return Err(unsupported("a vararg constructor of a non-public class"));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::ValueParameter {
            return Err(mismatch("a constructor parameter has no value symbol"));
        }
        let ty = semantic_type_in(arena, types, parameter.ty, resolve)?;
        defaults.push(parameter.default_value.is_some());
        params.push(ty);
        arguments.push(IrCtorArg {
            name: Some(parameter.name.clone()),
            context_kind: crate::types::ContextParameterKind::None,
            ty,
            declared_ty: Some(ty),
            is_field: false,
            field_index: None,
            has_default: parameter.default_value.is_some(),
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: IrCtorParameterProvenance::Value,
            capture_identity: None,
        });
    }
    Ok(ConstructorParameters {
        params,
        defaults,
        arguments,
    })
}

/// The superclass constructor a primary constructor delegates to before initializing its own
/// instance: `None` for `Any`'s, or the delegation to a same-library superclass.
fn superclass_delegation<'a>(
    arena: &'a KlibIrArena,
    class_symbol: &KlibIrSymbol,
    function: &'a KlibIrFunction,
    validate_root: &dyn Fn(&KlibPublicIdSignature) -> Result<(), KlibBodyDeclineReason>,
) -> Result<Option<&'a KlibIrMemberAccess>, KlibBodyDeclineReason> {
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return Err(unsupported("a nontrivial primary constructor body"));
    };
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        return Err(unsupported("a nontrivial primary constructor body"));
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return Err(mismatch(
            "a primary constructor does not delegate before initialization",
        ));
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !matches!(
            &arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == class_symbol
        )
    {
        return Err(mismatch(
            "an ordinary class does not delegate to a superclass and initialize its own instance",
        ));
    }
    let root = match &access.symbol.signature {
        KlibIrSignature::Public(signature) => {
            super::ir_builtins::root_constructor(signature, || validate_root(signature))?
        }
        _ => false,
    };
    if !root {
        return Ok(Some(access));
    }
    let no_operands = match &access.arguments {
        KlibIrArguments::Flat(values) => values.is_empty(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver.is_none() && extension_receiver.is_none() && values.is_empty(),
    };
    if !no_operands || !access.type_arguments.is_empty() {
        return Err(mismatch(
            "an ordinary class does not delegate to Any and initialize its own instance",
        ));
    }
    Ok(None)
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

fn unsupported(form: &'static str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedOperation(form)
}

fn non_public_type() -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedType("a type of a non-public class")
}

/// The construction target recorded for a same-library primary constructor.
pub(super) fn construction_target(access: IrConstructorAccess) -> IrConstructorTarget {
    IrConstructorTarget { ordinal: 0, access }
}

/// The class shape a prepared file-local class installs. Its supertypes are the authenticated
/// list, and its fields are stored by the initializer rather than by parameter spelling.
pub(super) fn installed_class(prepared: &PreparedLocalClass<'_>) -> IrClass {
    let mut class = IrClass::synthetic(prepared.classifier);
    // A class private to its enclosing class is a member classifier its public signature names,
    // as its source declares it; a file-local one has no name of its own here.
    class.is_source_declared = matches!(prepared.key.signature, KlibIrSignature::Public(_));
    class.supertypes = prepared.supertypes.clone();
    for interface in &prepared.interfaces {
        class.interfaces.push_name(*interface);
    }
    // A generic class keeps its declared formals and their bounds.
    for (identity, bounds) in prepared
        .class_type_parameters
        .iter()
        .zip(&prepared.class_type_parameter_bounds)
    {
        class.type_params.push(identity.clone());
        class
            .type_param_bounds
            .extend(bounds.iter().map(|bound| (identity.clone(), *bound)));
    }
    class.fields = prepared.fields.clone();
    class.ctor_args = prepared.ctor_args.clone();
    class.ctor_param_count = 0;
    class.explicit_param_stores = true;
    class.is_open = prepared.open;
    if let Some(outer) = &prepared.outer {
        class.is_inner_class = true;
        class.constructor_prefix_count = 1;
        class.pre_super_param_fields.push((0, outer.field));
    }
    class
}
