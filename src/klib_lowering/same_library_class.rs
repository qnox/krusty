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
//! A non-public interface is installed the same way without a constructor: it declares abstract
//! members only, so no class of this unit implements it unless a later preparation admits one. A
//! type test or a member call of it needs the installed interface to name.
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
use crate::klib_libraries::{KlibDeclarationBodies, LocatedClass, LocatedMember};
use crate::libraries::{KlibBodyClassParameters, KlibDependencyCallable, TypeKind};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprId, KlibIrExprKind,
    KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrStatement, KlibIrType,
    KlibIrTypeParameter, KlibIrVariance,
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
    /// Whether the constructor creates the one instance of an object.
    pub(super) object: bool,
}

/// The constructor that builds a materialized class, which an interface does not have.
pub(super) struct LocalConstruction {
    pub(super) constructor: KlibIrSignature,
    pub(super) declaration: ConstructorDeclaration,
    pub(super) access: IrConstructorAccess,
    /// Whether [`MaterializedLocalClass::class`] is an object.
    pub(super) object: bool,
}

/// One file-local class or interface materialized in the unit that lowers its library's bodies.
pub(super) struct MaterializedLocalClass {
    pub(super) class: ClassId,
    pub(super) classifier: TypeName,
    /// `None` for an interface.
    pub(super) construction: Option<LocalConstruction>,
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
    /// The arguments the constructor passes its superclass's constructor.
    pub(super) superclass_arguments: Vec<KlibIrExprId>,
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
    /// The class hierarchy the checker froze for the selected bodies, when it froze one.
    pub(super) classes: Option<&'bodies dyn crate::assignable::TypeOracle>,
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
    /// Members of the class's non-public interfaces this function overrides, by their file-local
    /// signatures in the class's module. The linker installs those interfaces before the class.
    pub(super) installed_overrides: Vec<KlibIrSignature>,
    pub(super) visibility: Visibility,
    /// The function's own type parameters and their declared bounds, which a call binds.
    pub(super) formals: Vec<(crate::types::TyParamName, Vec<Ty>)>,
    /// The runtime symbol the library binds this member to, when it declares one.
    pub(super) runtime_binding: Option<crate::ir::RuntimeBinding>,
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

struct AdmittedInterface {
    name: TypeName,
    arguments: Vec<Ty>,
}

/// A class's declared supertypes: every one in order, the interfaces among them, its
/// superclass other than `Any`, and, when that is a library class, its application.
struct AuthenticatedSupertypes {
    supertypes: Vec<Ty>,
    interfaces: Vec<AdmittedInterface>,
    /// The non-public interfaces of the same library among [`Self::supertypes`], by signature,
    /// each with its classifier.
    local_interfaces: Vec<(KlibIrSignature, TypeName)>,
    superclass: Option<TypeName>,
    library_superclass: Option<AdmittedInterface>,
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
    /// The non-public interfaces of the same library among [`Self::interfaces`], by signature,
    /// which the linker installs before the class.
    pub(super) local_interfaces: Vec<KlibIrSignature>,
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
    /// How Kotlin names a top-level class private to its file: in its file's package, by its
    /// declared name. Any other file-local class has no name of its own here.
    pub(super) naming: Option<crate::ir::IrLocalClassNameProvenance>,
    /// Whether the class is an object, whose one instance its singleton value reads.
    pub(super) object: bool,
}

/// A same-library superclass constructor a class's constructor delegates to, and the serialized
/// argument it passes for each of its parameters.
pub(super) struct LocalSuperclass<'a> {
    pub(super) constructor: SuperclassConstructor<'a>,
    pub(super) arguments: Vec<KlibIrExprId>,
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
        KlibIrSignature::Accessor(_) | KlibIrSignature::TypeParameter { .. } => return None,
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
        TypeKind::Class | TypeKind::Object | TypeKind::Interface => Ok(Some(name)),
        _ => Err(unsupported(
            "a non-public classifier other than a class, an object or an interface",
        )),
    }
}

/// The calling compilation's class model `public`, extended with the same-library classes
/// materialized in `classes`: the direct supertypes of a materialized class are the ones written on
/// it, so every subtype rule, function-type variance included, reads it as any other class. A class
/// from another library is absent here, so it stays outside this relation. Without the calling
/// compilation's model, only the materialized classes have a hierarchy.
pub(super) struct MaterializedClassModel<'a> {
    pub(super) classes: &'a HashMap<LocalIdentity, MaterializedLocalClass>,
    pub(super) public: Option<&'a dyn crate::assignable::TypeOracle>,
}

impl MaterializedClassModel<'_> {
    /// Whether `sub` is a subtype of `sup`. Without the calling compilation's model, only a
    /// relation rooted at a materialized class is proven, from the supertypes written on it.
    pub(super) fn is_subtype(&self, parameters: &KlibTypeParameters, sub: Ty, sup: Ty) -> bool {
        let model: &dyn crate::assignable::TypeOracle = self;
        let known = self.public.is_some() || self.names_materialized(sub);
        super::type_evidence::ordinary_subtype(known.then_some(model), parameters, sub, sup)
    }

    fn names_materialized(&self, ty: Ty) -> bool {
        match ty.kind() {
            TyKind::Nullable(inner) => self.names_materialized(inner),
            TyKind::Obj(name, _) => self.materialized(name).is_some(),
            _ => false,
        }
    }

    fn materialized(&self, classifier: TypeName) -> Option<&MaterializedLocalClass> {
        self.classes
            .values()
            .find(|class| class.classifier == classifier)
    }
}

impl crate::assignable::TypeOracle for MaterializedClassModel<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        applied_local_supertypes(self.classes, ty).unwrap_or_else(|| {
            self.public
                .map_or_else(Vec::new, |public| public.direct_supertypes(ty))
        })
    }

    fn same_class_name(&self, left: TypeName, right: TypeName) -> bool {
        self.public
            .map_or(left == right, |public| public.same_class_name(left, right))
    }

    fn type_param_variance(&self, owner: TypeName, index: usize) -> TypeVariance {
        // A materialized class records no declaration-site variance, so its arguments are only
        // ever proven invariant.
        match self.materialized(owner) {
            Some(_) => TypeVariance::Invariant,
            None => self.public.map_or(TypeVariance::Invariant, |public| {
                public.type_param_variance(owner, index)
            }),
        }
    }

    fn type_param_upper_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        match self.materialized(owner) {
            Some(class) => class
                .formals
                .get(index)
                .and_then(|formal| formal.ty_param_bound())
                .into_iter()
                .collect(),
            None => self.public.map_or_else(
                || vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                |public| public.type_param_upper_bounds(owner, index),
            ),
        }
    }

    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        self.public
            .map_or(lower, |public| public.platform_flexible_upper_bound(lower))
    }

    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        self.public
            .map_or_else(Vec::new, |public| public.type_variable_bounds(name))
    }
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
    (function, formals): (crate::ir::FunId, &[(crate::types::TyParamName, Vec<Ty>)]),
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
        type_parameters: formals.iter().map(|(name, _)| *name).collect(),
        type_parameter_bounds: Some(formals.iter().map(|(_, bounds)| bounds.clone()).collect()),
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
        let Some(construction) = &known.construction else {
            return Err(mismatch("a constructor names an interface"));
        };
        if construction.constructor != *signature {
            return Err(unsupported("a non-public class with another constructor"));
        }
        return Ok(LocalLink::Ready(LocalConstructor {
            declaration: construction.declaration.clone(),
            access: construction.access,
            object: construction.object,
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

/// An interface ready to install, or one already installed for this library.
pub(super) enum InterfaceLink<'a> {
    Ready,
    Prepared(Box<PreparedLocalInterface<'a>>),
}

/// A file-local interface: its supertypes, its formals and its abstract members, declared
/// before any body that names it is lowered.
pub(super) struct PreparedLocalInterface<'a> {
    pub(super) key: LocalIdentity,
    pub(super) classifier: TypeName,
    pub(super) self_type: Ty,
    pub(super) supertypes: Vec<Ty>,
    pub(super) interfaces: Vec<TypeName>,
    pub(super) formals: Vec<Ty>,
    pub(super) class_type_parameters: Vec<String>,
    /// The declared bounds of each interface type parameter, in its order.
    pub(super) class_type_parameter_bounds: Vec<Vec<Ty>>,
    pub(super) members: Vec<PreparedMember<'a>>,
}

/// The file-local interface `signature` of the body module. One already installed for this
/// library answers immediately; a new one is prepared and left for the linker to install.
pub(super) fn link_interface<'bodies>(
    source: LocalClassSource<'bodies, '_>,
    signature: &KlibIrSignature,
    cache: &HashMap<LocalIdentity, MaterializedLocalClass>,
) -> Result<InterfaceLink<'bodies>, KlibBodyDeclineReason> {
    let key = LocalIdentity {
        module: source.module.to_owned(),
        signature: signature.clone(),
    };
    if cache
        .get(&key)
        .is_some_and(|known| known.construction.is_none())
    {
        return Ok(InterfaceLink::Ready);
    }
    let Some((arena, class)) = source.bodies.module_local_class(source.module, signature) else {
        return Err(non_public_type());
    };
    if class_kind(class.base.flags) != TypeKind::Interface {
        return Err(mismatch("an interface is named by a class declaration"));
    }
    Ok(InterfaceLink::Prepared(Box::new(prepare_interface(
        source, arena, class, key,
    )?)))
}

fn prepare_interface<'a>(
    source: LocalClassSource<'a, '_>,
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    key: LocalIdentity,
) -> Result<PreparedLocalInterface<'a>, KlibBodyDeclineReason> {
    let resolve = |signature: &KlibIrSignature| classifier(source.bodies, source.module, signature);
    let classifier_name = resolve(&class.base.symbol.signature)?.ok_or_else(non_public_type)?;
    if !class.sealed_subclasses.is_empty() {
        return Err(unsupported("a non-public sealed interface"));
    }
    let ClassFormals {
        types,
        formals,
        variances: _,
        bounds,
        // An interface is never inner, so no outer instance's type parameters are its own.
    } = class_formals(arena, class, None, &resolve)?;
    let AuthenticatedSupertypes {
        supertypes,
        interfaces,
        local_interfaces,
        superclass,
        library_superclass,
    } = authenticate_supertypes(source.bodies, source.module, arena, class, &types, &resolve)?;
    // Only an interface can be a supertype of an interface: a superclass of either kind is a
    // declaration this installation did not authenticate.
    if superclass.is_some() || library_superclass.is_some() {
        return Err(mismatch("an interface lists a superclass"));
    }
    // An installed interface's members take numbers of their own, which one extending it would
    // have to share.
    if !local_interfaces.is_empty() {
        return Err(unsupported(
            "a non-public interface extending a non-public interface",
        ));
    }
    let self_type = Ty::obj_args_name(classifier_name, &formals);
    let Some(receiver) = &class.this_receiver else {
        return Err(mismatch("an interface has no instance receiver"));
    };
    if semantic_type_in(arena, &types, receiver.ty, &resolve)? != self_type {
        return Err(mismatch(
            "an interface instance receiver has another declared type",
        ));
    }
    let scope = MemberScope {
        types: &types,
        resolve: &resolve,
        self_type,
        bodies: source.bodies,
        declarations: source.declarations,
        classes: source.classes,
        module: source.module,
        interfaces: &interfaces,
        local_interfaces: &[],
        library_superclass: None,
    };
    let members = abstract_members(arena, class, &scope)?;
    let class_type_parameters = formals
        .iter()
        .map(|formal| match formal.kind() {
            TyKind::TyParam(identity, _) => Ok(identity.as_str().to_owned()),
            _ => Err(mismatch(
                "a non-public interface type parameter has no symbol",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedLocalInterface {
        key,
        classifier: classifier_name,
        self_type,
        supertypes,
        interfaces: interfaces.iter().map(|interface| interface.name).collect(),
        formals,
        class_type_parameters,
        class_type_parameter_bounds: bounds,
        members,
    })
}

/// The abstract members of an interface, in declaration order. A member with a body, a stored
/// property and an initializer are refused: this installs no implementation.
fn abstract_members<'a>(
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    scope: &MemberScope<'_>,
) -> Result<Vec<PreparedMember<'a>>, KlibBodyDeclineReason> {
    let mut members = Vec::new();
    for declaration in &class.members {
        if member_field(declaration).is_some() {
            return Err(unsupported("a stored property of a non-public interface"));
        }
        match declaration {
            KlibIrMember::Function(id) => {
                let function = arena.function(*id);
                if function.constructor {
                    return Err(mismatch("an interface declares a constructor"));
                }
                members.push(prepare_abstract_member(arena, function, scope)?);
            }
            KlibIrMember::Property(property) => {
                for id in [property.getter, property.setter].into_iter().flatten() {
                    members.push(prepare_abstract_member(arena, arena.function(id), scope)?);
                }
            }
            KlibIrMember::Field(_) | KlibIrMember::AnonymousInitializer { .. } => {
                return Err(unsupported("an initializer of a non-public interface"))
            }
            // A nested class is its own declaration, prepared when a body names it.
            KlibIrMember::Class(_) => {}
            KlibIrMember::EnumEntry(_) => return Err(unsupported("a non-public enum")),
            KlibIrMember::TypeAlias(_) => {
                return Err(unsupported("a type alias of a non-public interface"))
            }
        }
    }
    Ok(members)
}

fn prepare_abstract_member<'a>(
    arena: &'a KlibIrArena,
    function: &'a KlibIrFunction,
    scope: &MemberScope<'_>,
) -> Result<PreparedMember<'a>, KlibBodyDeclineReason> {
    if function.body.is_some() {
        return Err(unsupported(
            "a member of a non-public interface with a body",
        ));
    }
    let header = local_member_header(function, arena, scope.types, scope.self_type, scope.resolve)?;
    let MemberOverrides {
        published: overrides,
        installed: installed_overrides,
    } = member_overrides(function, scope)?;
    Ok(PreparedMember {
        signature: function.base.symbol.signature.clone(),
        symbol: &function.base.symbol,
        statements: &[],
        formals: header.own_formals(function),
        header,
        overrides,
        installed_overrides,
        visibility: declaration_visibility(function.base.flags),
        runtime_binding: None,
    })
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
        KlibIrSignature::Accessor(_) | KlibIrSignature::TypeParameter { .. } => {
            Err(non_public_type())
        }
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

/// Authenticates that `object` keeps the one process-wide instance its installed class is
/// realized as. Each annotation is joined to its declared classifier; one of unknown identity
/// cannot prove the storage role. A per-thread object needs its own identity and state on every
/// thread, which installed objects do not carry, so it stays refused.
fn object_storage(object: &KlibIrClass) -> Result<(), KlibBodyDeclineReason> {
    let per_thread = crate::types::type_name("kotlin/native/concurrent/ThreadLocal");
    for annotation in &object.base.annotations {
        match super::klib_types::annotation_classifier(annotation) {
            None => {
                return Err(unsupported(
                    "a non-public object annotation of unknown identity",
                ))
            }
            Some(classifier) if classifier == per_thread => {
                return Err(unsupported("a non-public per-thread object"))
            }
            Some(_) => {}
        }
    }
    Ok(())
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
    let naming = file_private_naming(source.bodies, &key);
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
    // An object's private constructor creates its one instance, which no call constructs again.
    let object = class_kind(class.base.flags) == TypeKind::Object;
    if object
        && (declaration_visibility(function.base.flags) != Visibility::Private
            || !function.regular_parameters.is_empty()
            || !class.type_parameters.is_empty())
    {
        return Err(mismatch(
            "an object is not created by its private constructor alone",
        ));
    }
    if object {
        object_storage(class)?;
    }
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
        local_interfaces,
        superclass,
        library_superclass,
    } = authenticate_supertypes(source.bodies, source.module, arena, class, &types, &resolve)?;
    let (local_interfaces, local_interface_names): (Vec<_>, Vec<_>) =
        local_interfaces.into_iter().unzip();
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
        classes: source.classes,
        module: source.module,
        interfaces: &interfaces,
        local_interfaces: &local_interfaces,
        library_superclass: library_superclass.as_ref(),
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
            delegation,
            superclass,
        },
    )?;
    let class_type_parameters = formals
        .iter()
        .map(|formal| match formal.kind() {
            TyKind::TyParam(identity, _) => Ok(identity.as_str().to_owned()),
            _ => Err(mismatch("a non-public class type parameter has no symbol")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let init_header = local_initializer_header(
        function,
        receiver,
        self_type,
        outer_instance.map(|_| params[0]),
        arena,
        &types,
        &resolve,
    )?;
    let class_type_parameter_bounds = bounds.clone();
    Ok(PreparedLocalClass {
        key,
        classifier: class_name,
        self_type,
        supertypes,
        interfaces: interfaces
            .iter()
            .map(|interface| interface.name)
            .chain(local_interface_names)
            .collect(),
        local_interfaces,
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
        access: constructor_access(function),
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
        naming,
        object,
    })
}

/// The naming provenance of the class `key` names when it is a top-level class private to its
/// file: rooted in that file's package, with no lexical owner and no ordinal.
fn file_private_naming(
    bodies: &KlibDeclarationBodies,
    key: &LocalIdentity,
) -> Option<crate::ir::IrLocalClassNameProvenance> {
    let (package, class) = bodies.module_file_private_class(&key.module, &key.signature)?;
    let package = package.iter().fold(TypeName::ROOT, |parent, segment| {
        crate::types::type_name_child(parent, segment)
    });
    Some(crate::ir::IrLocalClassNameProvenance {
        source: crate::ir::IrLocalClassNameRoot::Dependency { package },
        lexical_owner: None,
        segments: Box::new([class.name.clone()]),
        ordinal: None,
        parents: Box::new([]),
    })
}

/// A class's constructor delegation to the constructor of its same-library superclass, and what
/// it checks that delegation against.
struct LocalDelegation<'a> {
    delegation: Option<&'a KlibIrMemberAccess>,
    superclass: Option<TypeName>,
}

/// The same-library superclass a class's constructor delegates to, with the serialized argument
/// it passes for each of that constructor's parameters. The class's initializer evaluates them in
/// the constructor's frame, as it evaluates the rest of the constructor's body.
fn local_superclass<'a>(
    source: LocalClassSource<'a, '_>,
    delegation: LocalDelegation<'a>,
) -> Result<Option<LocalSuperclass<'a>>, KlibBodyDeclineReason> {
    let LocalDelegation {
        delegation,
        superclass,
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
    let arguments = arguments
        .into_iter()
        .map(|argument| {
            argument.ok_or(unsupported(
                "a superclass delegation leaving a parameter to its default",
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(LocalSuperclass {
        constructor,
        arguments,
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
    let mut local_interfaces = Vec::new();
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
        // A same-library interface is installed with the class that implements it; a same-library
        // class is the superclass, which the class's constructor delegates to.
        if let Some(local) = resolve(&classifier.signature)? {
            let is_interface = bodies
                .module_local_class(module, &classifier.signature)
                .is_some_and(|(_, local)| class_kind(local.base.flags) == TypeKind::Interface);
            if is_interface {
                local_interfaces.push((classifier.signature.clone(), local));
                supertypes.push(ty);
                continue;
            }
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
            LocatedClass::One(found) => {
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
                    arguments: ty.type_args().to_vec(),
                });
                supertypes.push(ty);
            }
        }
    }
    Ok(AuthenticatedSupertypes {
        supertypes,
        interfaces,
        local_interfaces,
        superclass,
        library_superclass,
    })
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
    classes: Option<&'a dyn crate::assignable::TypeOracle>,
    module: &'a str,
    interfaces: &'a [AdmittedInterface],
    /// The non-public interfaces of the class's own library it implements, by signature.
    local_interfaces: &'a [KlibIrSignature],
    library_superclass: Option<&'a AdmittedInterface>,
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
    let (statements, runtime_binding) = match &function.body {
        Some(KlibIrBody::Block(statements)) => (
            statements.as_slice(),
            crate::klib_libraries::runtime_export_symbol(arena, function)
                .map(|symbol| crate::ir::RuntimeBinding::Export(symbol.to_owned())),
        ),
        Some(KlibIrBody::Synthetic(_)) => {
            return Err(unsupported("a synthetic member of a non-public class"))
        }
        None => match crate::klib_libraries::runtime_entry_symbol(arena, function) {
            Some(symbol) => (
                &[][..],
                Some(crate::ir::RuntimeBinding::Entry(symbol.to_owned())),
            ),
            None => return Err(unsupported("a member of a non-public class without a body")),
        },
    };
    let header = local_member_header(function, arena, scope.types, scope.self_type, scope.resolve)?;
    let MemberOverrides {
        published: overrides,
        installed: installed_overrides,
    } = member_overrides(function, scope)?;
    Ok(PreparedMember {
        signature: function.base.symbol.signature.clone(),
        symbol: &function.base.symbol,
        statements,
        formals: header.own_formals(function),
        header,
        overrides,
        installed_overrides,
        visibility: declaration_visibility(function.base.flags),
        runtime_binding,
    })
}

/// What one member overrides: published declarations by their dependency identity, and members
/// of the class's non-public interfaces by their file-local signatures.
struct MemberOverrides {
    published: Vec<MemberOverride>,
    installed: Vec<KlibIrSignature>,
}

fn member_overrides(
    function: &KlibIrFunction,
    scope: &MemberScope<'_>,
) -> Result<MemberOverrides, KlibBodyDeclineReason> {
    // An override overrides every declaration the ones it names override in turn:
    // `ArrayAsCollection.iterator` overrides `Collection.iterator` and, through it,
    // `Iterable.iterator`, which interface dispatch keys by.
    let mut identities = Vec::<KlibDependencyCallable>::new();
    let mut installed = Vec::new();
    for symbol in &function.overridden {
        if symbol.kind != KlibIrSymbolKind::Function {
            return Err(unsupported(
                "an override of a declaration other than a function",
            ));
        }
        // A member of a non-public interface the class implements is no published declaration:
        // it is the member the installed interface declares. A non-public interface extends no
        // other, so nothing further is overridden through it.
        if let KlibIrSignature::FileLocal { .. } = symbol.signature {
            let declaring = scope
                .bodies
                .module_class_declaring(scope.module, &symbol.signature)
                .map(|(_, class)| &class.base.symbol.signature);
            if !declaring.is_some_and(|interface| scope.local_interfaces.contains(interface)) {
                return Err(unsupported("an override of a non-public declaration"));
            }
            if !installed.contains(&symbol.signature) {
                installed.push(symbol.signature.clone());
            }
            continue;
        }
        // A property accessor overrides the interface's accessor by its accessor signature, as a
        // function overrides by its public one; `function_owner` locates either.
        let Some(declaration) = crate::klib_libraries::member_signature(&symbol.signature) else {
            return Err(unsupported("an override of a non-public declaration"));
        };
        let closure = scope
            .bodies
            .overridden_closure(scope.module, &declaration)
            .map_err(|_| {
                KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override this library does not authenticate",
                )
            })?;
        for identity in closure {
            if !identities.contains(&identity) {
                identities.push(identity);
            }
        }
    }
    let supertypes = applied_supertypes(scope);
    let mut overrides = Vec::with_capacity(identities.len());
    for identity in identities {
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
        let (owner_name, arguments) = if metadata_owner == crate::types::wk::any() {
            (metadata_owner, &[][..])
        } else {
            // A supertype above the class's written ones is applied as the walk from them
            // reaches it.
            let Some((_, applied)) = supertypes
                .iter()
                .find(|(classifier, _)| *classifier == metadata_owner)
            else {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override of a supertype this library does not authenticate",
                ));
            };
            let Some((_, arguments)) = applied.non_null().obj_parts() else {
                return Err(mismatch(
                    "an overridden supertype is not a class application",
                ));
            };
            (metadata_owner, arguments)
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
        if declared.constructor {
            return Err(unsupported("an override of a constructor declaration"));
        }
        let mut formals = Vec::new();
        let mut declared_types = bind_interface_parameters(owner, |parameter| {
            let formal = Ty::ty_param(
                crate::types::metadata_type_parameter(&parameter.name),
                Ty::nullable(Ty::obj("kotlin/Any")),
            );
            formals.push(formal);
            Ok(formal)
        })?;
        // The overridden declaration's own type parameters stay formals of its signature; only
        // its class's are applied.
        for parameter in &declared.type_parameters {
            let formal = Ty::ty_param(
                crate::types::metadata_type_parameter(&parameter.name),
                Ty::nullable(Ty::obj("kotlin/Any")),
            );
            if !declared_types.bind(&parameter.base.symbol, formal) {
                return Err(mismatch(
                    "an overridden declaration repeats a type parameter",
                ));
            }
        }
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
    Ok(MemberOverrides {
        published: overrides,
        installed,
    })
}

/// Every supertype from a class's written interfaces and its library superclass up, at the
/// application the walk from them reaches it with, through the hierarchy the checker froze.
/// Without that evidence only the written supertypes themselves are known.
fn applied_supertypes(scope: &MemberScope<'_>) -> Vec<(TypeName, Ty)> {
    let mut supertypes = Vec::<(TypeName, Ty)>::new();
    for written in scope.interfaces.iter().chain(scope.library_superclass) {
        let root = Ty::obj_args_name(written.name, &written.arguments);
        for (classifier, applied, _) in
            crate::assignable::applied_hierarchy_through(root, |_, applied| {
                scope
                    .classes
                    .map_or_else(Vec::new, |classes| classes.direct_supertypes(applied))
            })
        {
            if !supertypes.iter().any(|(known, _)| *known == classifier) {
                supertypes.push((classifier, applied));
            }
        }
    }
    supertypes
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
    for parameter in parameters {
        let mut parameter_bounds = Vec::with_capacity(parameter.supertypes.len());
        for bound in &parameter.supertypes {
            parameter_bounds.push(semantic_type_in(arena, &types, *bound, resolve)?);
        }
        bounds.push(parameter_bounds);
    }
    for ((&formal, &variance), declared) in formals.iter().zip(&variances).zip(&bounds) {
        if !types.record_bounds(formal, declared.clone())
            || !types.record_variance(formal, variance)
        {
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

/// Who may call the serialized constructor `function`.
pub(super) fn constructor_access(function: &KlibIrFunction) -> IrConstructorAccess {
    match declaration_visibility(function.base.flags) {
        Visibility::Private => IrConstructorAccess::Private,
        _ => IrConstructorAccess::Unrestricted,
    }
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
    // as its source declares it, and a top-level class private to its file is named by its
    // provenance; any other file-local one has no name of its own here.
    class.is_source_declared =
        matches!(prepared.key.signature, KlibIrSignature::Public(_)) || prepared.naming.is_some();
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
    class.is_object = prepared.object;
    class
}

/// The interface shape a prepared file-local interface installs. Its members are declared by the
/// linker; none has a body.
pub(super) fn installed_interface(prepared: &PreparedLocalInterface<'_>) -> IrClass {
    let mut class = IrClass::synthetic(prepared.classifier);
    class.is_interface = true;
    class.has_primary_ctor = false;
    class.supertypes = prepared.supertypes.clone();
    for interface in &prepared.interfaces {
        class.interfaces.push_name(*interface);
    }
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
    class
}
