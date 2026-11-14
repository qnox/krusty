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
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies, LocatedClass};
use crate::libraries::{
    KlibBodyClassParameters, KlibDeclarationSignature, KlibDependencyCallable, TypeKind,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrFunction,
    KlibIrMember, KlibIrStatement, KlibIrType, KlibIrTypeParameter, KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::semantic::{
    class_is_value, class_kind, declaration_modality, declaration_visibility,
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
    pub(super) receiver_symbol: &'a KlibIrSymbol,
    pub(super) receiver_ty: Ty,
    pub(super) fields: Vec<LocalStoredField<'a>>,
    pub(super) steps: Vec<InitializationStep<'a>>,
}

/// The field frame of one member of a file-local class.
pub(super) struct LocalMemberFields<'a> {
    pub(super) class: ClassId,
    pub(super) receiver: Ty,
    pub(super) fields: Vec<LocalStoredField<'a>>,
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
    pub(super) declarations: &'call dyn crate::symbol_source::SymbolSource,
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
    /// Whether the function is its property's setter.
    pub(super) setter: bool,
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
    let contract = realization.klib_body_class_contract.as_ref();
    if contract.is_some_and(|contract| {
        contract.classifier != owner
            || realization.callable.owner_is_interface != (contract.kind == TypeKind::Interface)
    }) {
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
        is_interface: realization.callable.owner_is_interface,
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

struct AdmittedInterface {
    name: TypeName,
    signature: KlibIrSignature,
    arguments: Vec<Ty>,
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
    pub(super) arena: &'a KlibIrArena,
    pub(super) receiver_symbol: &'a KlibIrSymbol,
    pub(super) receiver_ty: Ty,
    pub(super) init_header: FunctionHeader,
    pub(super) steps: Vec<InitializationStep<'a>>,
    pub(super) members: Vec<PreparedMember<'a>>,
}

/// The classifier identity of a file-local class in `module`. The file and slot are the identity;
/// the source spelling is not.
pub(super) fn classifier_name(module: &str, signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::FileLocal { file, slot } = signature else {
        return None;
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
    let TyKind::Obj(name, arguments) = sub.kind() else {
        return false;
    };
    let Some(local) = classes.values().find(|class| class.classifier == name) else {
        return false;
    };
    if arguments.len() != local.formals.len() {
        return false;
    }
    let mut bindings = HashMap::default();
    for (formal, argument) in local.formals.iter().zip(arguments.iter()) {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return false;
        };
        bindings.insert(identity.as_str().to_owned(), *argument);
    }
    for declared in &local.supertypes {
        let applied = crate::types::ty_subst_applied_arguments(*declared, &bindings);
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
    if function.dispatch_receiver.is_some()
        || function.extension_receiver.is_some()
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
        KlibIrSignature::FileLocal { .. } => Ok(classifier.signature.clone()),
        KlibIrSignature::Public(_) | KlibIrSignature::Accessor(_) => Err(non_public_type()),
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
    let formals = class_formals(arena, class, &resolve)?;
    prepare_class(
        source,
        signature,
        ClassDeclaration {
            arena,
            class,
            function,
            key,
            class_name,
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
    delegates_to_any(arena, &class.base.symbol, function, source.validate_root)?;
    let resolve = |signature: &KlibIrSignature| classifier(source.bodies, source.module, signature);
    let ClassFormals {
        types,
        formals,
        variances,
        bounds,
    } = formals;
    let (supertypes, interfaces) =
        authenticate_supertypes(source.bodies, source.module, arena, class, &types, &resolve)?;
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
        module: source.module,
        interfaces: &interfaces,
        declarations: source.declarations,
    };
    let StoredMembers {
        fields,
        stored,
        steps,
        members,
    } = members_and_storage(arena, class, &scope)?;
    let ConstructorParameters {
        params,
        defaults,
        arguments: ctor_args,
    } = constructor_parameters(arena, function, &types, &resolve)?;
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
        declaration: ConstructorDeclaration::local(
            class_name,
            params,
            KlibBodyClassParameters {
                formals: formals.clone().into_boxed_slice(),
                bounds: bounds.into_boxed_slice(),
                variances: variances.into_boxed_slice(),
                own_count: formals.len(),
            },
            defaults.into_iter().collect(),
        ),
        access: match declaration_visibility(function.base.flags) {
            Visibility::Private => IrConstructorAccess::Private,
            _ => IrConstructorAccess::Unrestricted,
        },
        formals,
        class_type_parameters,
        class_type_parameter_bounds,
        constructor_symbol: &function.base.symbol,
        arena,
        receiver_symbol: &receiver.base.symbol,
        receiver_ty: self_type,
        init_header,
        steps,
        members,
    })
}

fn authenticate_supertypes(
    bodies: &KlibDeclarationBodies,
    module: &str,
    arena: &KlibIrArena,
    class: &KlibIrClass,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<(Vec<Ty>, Vec<AdmittedInterface>), KlibBodyDeclineReason> {
    let mut supertypes = Vec::new();
    let mut interfaces = Vec::new();
    for supertype in &class.supertypes {
        let ty = semantic_type_in(arena, types, *supertype, resolve)?;
        if ty == Ty::obj_name(crate::types::wk::any()) {
            supertypes.push(ty);
            continue;
        }
        let KlibIrType::Simple { classifier, .. } = arena.ty(*supertype) else {
            return Err(unsupported("a superclass other than Any"));
        };
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
            LocatedClass::One(found) => {
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
    Ok((supertypes, interfaces))
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
    module: &'a str,
    interfaces: &'a [AdmittedInterface],
    declarations: &'a dyn crate::symbol_source::SymbolSource,
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
                    members.push(prepare_member(arena, function, scope, false)?);
                }
            }
            KlibIrMember::Property(property) => {
                if let Some(id) = property.getter {
                    members.push(prepare_member(arena, arena.function(id), scope, false)?);
                }
                if let Some(id) = property.setter {
                    members.push(prepare_member(arena, arena.function(id), scope, true)?);
                }
            }
            KlibIrMember::Field(_) | KlibIrMember::AnonymousInitializer { .. } => {}
            KlibIrMember::Class(_) => {
                return Err(unsupported("a nested class of a non-public class"))
            }
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
    setter: bool,
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
        setter,
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
        let KlibIrSignature::Public(signature) = &symbol.signature else {
            return Err(unsupported("an override of a non-public declaration"));
        };
        let declaration = KlibDeclarationSignature::Public(signature.clone());
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
        let KlibDeclarationSignature::Public(_) = &identity.declaration else {
            return Err(unsupported("an override of a non-public declaration"));
        };
        let Some((_, realization)) = scope
            .declarations
            .external_callable_by_dependency(&identity)
        else {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "an override this library does not authenticate",
            ));
        };
        let Some(canonical) = realization.klib_body_callable() else {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "an override this library does not authenticate",
            ));
        };
        if canonical.identity() != &identity {
            return Err(mismatch(
                "an overridden function has another provider identity",
            ));
        }
        let header = super::class_headers::authenticate(canonical, scope.bodies)?;
        super::class_headers::authenticate_receiver(
            header.contract,
            header.arena,
            header.function,
        )?;
        let contract = header.contract;
        if !canonical.is_member()
            || canonical.declaration_owner()
                != Some(crate::types::SemanticCallableOwner::Classifier(
                    contract.classifier,
                ))
            || contract.module != identity.module
        {
            return Err(mismatch(
                "an overridden function has another declaration owner",
            ));
        }
        let owner_name = contract.classifier;
        let is_interface = class_kind(header.class.base.flags) == TypeKind::Interface;
        if contract.kind != class_kind(header.class.base.flags) {
            return Err(mismatch("an overridden class has another declared kind"));
        }
        let applied_arguments = if is_interface {
            let interface = scope
                .interfaces
                .iter()
                .find(|interface| interface.signature == header.class.base.symbol.signature)
                .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override of a supertype this library does not authenticate",
                ))?;
            if interface.name != owner_name {
                return Err(mismatch("an overridden interface has another owner"));
            }
            Some(interface.arguments.as_slice())
        } else if owner_name == crate::types::wk::any() {
            // The enclosing class constructor was already authenticated to delegate to Any.
            None
        } else {
            return Err(unsupported(
                "an override of an unauthenticated class member",
            ));
        };
        let declared = header.function;
        if !declared.type_parameters.is_empty() || declared.constructor {
            return Err(unsupported(
                "an override of a generic or constructor declaration",
            ));
        }
        let declared_signature =
            interface_signature(header.arena, declared, &header.types, scope.resolve)?;
        if declared_signature.parameters.len() != function.regular_parameters.len() {
            return Err(unsupported(
                "an override whose parameters differ from the interface member",
            ));
        }
        let (applied_parameters, applied_result) = match applied_arguments {
            Some(arguments) => substitute_applied_signature(
                &declared_signature.parameters,
                declared_signature.result,
                &contract.parameters.formals,
                arguments,
            )?,
            None => (
                declared_signature.parameters.clone(),
                declared_signature.result,
            ),
        };
        overrides.push(MemberOverride {
            identity,
            owner: owner_name,
            declared_parameters: declared_signature.parameters,
            declared_result: declared_signature.result,
            applied_parameters,
            applied_result,
            semantic_role: canonical.semantic_role(),
            is_interface,
        });
    }
    Ok(overrides)
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
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<ClassFormals, KlibBodyDeclineReason> {
    let mut types = KlibTypeParameters::default();
    let mut formals = Vec::with_capacity(class.type_parameters.len());
    let mut variances = Vec::with_capacity(class.type_parameters.len());
    for parameter in &class.type_parameters {
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
    let mut bounds = Vec::with_capacity(class.type_parameters.len());
    for parameter in &class.type_parameters {
        let mut parameter_bounds = Vec::with_capacity(parameter.supertypes.len());
        for bound in &parameter.supertypes {
            parameter_bounds.push(semantic_type_in(arena, &types, *bound, resolve)?);
        }
        bounds.push(parameter_bounds);
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

fn delegates_to_any(
    arena: &KlibIrArena,
    class_symbol: &KlibIrSymbol,
    function: &KlibIrFunction,
    validate_root: &dyn Fn(&KlibPublicIdSignature) -> Result<(), KlibBodyDeclineReason>,
) -> Result<(), KlibBodyDeclineReason> {
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
    let no_operands = match &access.arguments {
        KlibIrArguments::Flat(values) => values.is_empty(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver.is_none() && extension_receiver.is_none() && values.is_empty(),
    };
    let root = match &access.symbol.signature {
        KlibIrSignature::Public(signature) => {
            super::ir_builtins::root_constructor(signature, || validate_root(signature))?
        }
        _ => false,
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !no_operands
        || !access.type_arguments.is_empty()
        || !root
        || !matches!(
            &arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == class_symbol
        )
    {
        return Err(mismatch(
            "an ordinary class does not delegate to Any and initialize its own instance",
        ));
    }
    Ok(())
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
    class
}
