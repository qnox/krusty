//! Exact declaration-owned class membership and formal bindings shared by member calls.

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{classifier_name, semantic_type, KlibTypeParameters};
use crate::fx_hash::HashSet;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    KlibAccessorRole, KlibBodyCallable, KlibBodyClassContract, KlibBodyClassParameters,
};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrVariance};
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::metadata::klib_ir::{
    tree::{KlibIrNullability, KlibIrType},
    KlibIrSignature,
};
use crate::types::{Ty, TyKind, TypeVariance};

pub(super) struct ClassMemberHeader<'a, 'b> {
    pub(super) contract: &'a KlibBodyClassContract,
    pub(super) arena: &'b KlibIrArena,
    pub(super) function: &'b KlibIrFunction,
    pub(super) class: &'b KlibIrClass,
    pub(super) types: KlibTypeParameters,
    pub(super) formals: Vec<String>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// Semantic type conversion interns class paths, so prove the raw identity, including its mask,
/// before using a member's dispatch receiver as a semantic class application.
pub(super) fn authenticate_receiver(
    class: &KlibBodyClassContract,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<(), KlibBodyDeclineReason> {
    let dispatch = function
        .dispatch_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a class member has no serialized dispatch receiver"))?;
    if !matches!(arena.ty(dispatch.ty), KlibIrType::Simple { classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull, .. }
        if classifier.kind == KlibIrSymbolKind::Class
            && classifier.signature == KlibIrSignature::Public(class.signature.clone()))
    {
        return Err(mismatch(
            "the serialized member receiver names a different class identity",
        ));
    }
    Ok(())
}

pub(super) fn declared_receiver(class: &KlibBodyClassContract) -> Ty {
    if class.parameters.formals.is_empty() {
        crate::types::builtin_semantic(class.classifier)
            .unwrap_or_else(|| Ty::obj_name(class.classifier))
    } else {
        Ty::obj_args_name(class.classifier, &class.parameters.formals)
    }
}

pub(super) fn authenticate<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
) -> Result<ClassMemberHeader<'a, 'b>, KlibBodyDeclineReason> {
    let class = selected
        .class_contract()
        .ok_or_else(|| mismatch("a selected member has no provider-owned class contract"))?;
    // A singleton edge's class is the singleton it dispatches to. Its declaration owner can
    // remain the class the member was inherited from.
    let class_is_member_owner = selected.declaration_owner()
        == Some(crate::types::SemanticCallableOwner::Classifier(
            class.classifier,
        ));
    let class_is_singleton = selected.singleton_classifier() == Some(class.classifier);
    if class.module != selected.identity().module
        || class.signature.member_id().is_some()
        || !(class_is_member_owner || class_is_singleton)
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch(
            "the class identity disagrees with its selected member",
        ));
    }
    let member = bodies
        .class_member_header(
            selected.identity(),
            &class.signature,
            selected
                .accessor()
                .map_or(KlibAccessorRole::Getter, |accessor| accessor.role)
                .into(),
        )
        .map_err(mismatch)?;
    if let Some(member) = member {
        if member.class.base.symbol.kind != KlibIrSymbolKind::Class {
            return Err(mismatch(
                "the serialized class header has a non-class symbol",
            ));
        }
        let (types, formals) =
            bind_class_parameters(&class.parameters, member.arena, member.class)?;
        return Ok(ClassMemberHeader {
            contract: class,
            arena: member.arena,
            function: member.function,
            class: member.class,
            types,
            formals,
        });
    }
    // A singleton member can be serialized as the function itself. Its dispatch receiver names
    // the class, and a class with no type parameters has nothing further to bind.
    let bare_function = (class.parameters.formals.is_empty() && class_is_singleton)
        .then(|| bodies.function(selected.identity()))
        .flatten();
    if let Some((arena, function)) = bare_function {
        authenticate_receiver(class, arena, function)?;
        return Ok(ClassMemberHeader {
            contract: class,
            arena,
            function,
            class: receiver_named_class(),
            types: KlibTypeParameters::default(),
            formals: Vec::new(),
        });
    }
    Err(mismatch(
        "a selected member has no exact serialized class header",
    ))
}

/// Placeholder class node for a member serialized as its function, whose receiver names the
/// class. The class has no type parameters, so later phases do not read this node.
fn receiver_named_class() -> &'static KlibIrClass {
    use std::sync::OnceLock;
    static CLASS: OnceLock<KlibIrClass> = OnceLock::new();
    CLASS.get_or_init(|| KlibIrClass {
        base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
            symbol: crate::metadata::klib_ir::KlibIrSymbol {
                kind: KlibIrSymbolKind::Class,
                signature: crate::metadata::klib_ir::KlibIrSignature::FileLocal {
                    file: 0,
                    slot: 0,
                },
            },
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        },
        name: String::new(),
        this_receiver: None,
        type_parameters: Vec::new(),
        supertypes: Vec::new(),
        members: Vec::new(),
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    })
}

/// The raw header of `identity`, a private member of `class` that no provider records. Only
/// members of its class call a private member, so the calling member's authenticated class is
/// the authority: the callee must be a direct member of that exact class header (an accessor in
/// the slot of its own shape), its receiver must name that exact class, and it must be private.
pub(super) fn authenticate_private_member<'a, 'b>(
    class: &'a KlibBodyClassContract,
    identity: &crate::libraries::KlibDependencyCallable,
    bodies: &'b KlibDeclarationBodies,
) -> Result<ClassMemberHeader<'a, 'b>, KlibBodyDeclineReason> {
    if class.module != identity.module
        || class.signature.member_id().is_some()
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch(
            "the calling class identity disagrees with its private member",
        ));
    }
    let member = bodies
        .class_member_header(identity, &class.signature, None)
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("a private member is not a direct member of its calling class"))?;
    if member.class.base.symbol.kind != KlibIrSymbolKind::Class {
        return Err(mismatch(
            "the serialized class header has a non-class symbol",
        ));
    }
    if crate::metadata::semantic::declaration_visibility(member.function.base.flags)
        != crate::types::Visibility::Private
    {
        return Err(mismatch(
            "a class member without a provider record is not private",
        ));
    }
    authenticate_receiver(class, member.arena, member.function)?;
    let (types, formals) = bind_class_parameters(&class.parameters, member.arena, member.class)?;
    Ok(ClassMemberHeader {
        contract: class,
        arena: member.arena,
        function: member.function,
        class: member.class,
        types,
        formals,
    })
}

/// The raw header of the private primary constructor of the object `class`, which no provider
/// records: an object's instance is created only by its own declaration. The constructor must be
/// a direct member of that exact class header, private, and take no value.
pub(super) fn authenticate_object_constructor<'a, 'b>(
    class: &'a KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
) -> Result<Option<ClassMemberHeader<'a, 'b>>, KlibBodyDeclineReason> {
    let Some(primary) = class
        .primary_constructor
        .as_ref()
        .filter(|_| class.kind == crate::libraries::TypeKind::Object)
    else {
        return Ok(None);
    };
    if class.signature.member_id().is_some()
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch(
            "an object's identity disagrees with its classifier",
        ));
    }
    let identity = crate::libraries::KlibDependencyCallable {
        module: class.module.clone(),
        declaration: crate::libraries::KlibDeclarationSignature::Public(primary.clone()),
    };
    // An object of a module compiled from source alongside has no serialized declaration: its
    // own file lays it out, never a support unit.
    let Some(member) = bodies
        .class_member_header(&identity, &class.signature, Some(KlibAccessorRole::Getter))
        .map_err(mismatch)?
    else {
        return Ok(None);
    };
    if member.class.base.symbol.kind != KlibIrSymbolKind::Class
        || !member.function.constructor
        || crate::metadata::semantic::declaration_visibility(member.function.base.flags)
            != crate::types::Visibility::Private
        || !member.function.regular_parameters.is_empty()
    {
        return Err(mismatch(
            "an object's primary constructor is not its private constructor taking no value",
        ));
    }
    let (types, formals) = bind_class_parameters(&class.parameters, member.arena, member.class)?;
    Ok(Some(ClassMemberHeader {
        contract: class,
        arena: member.arena,
        function: member.function,
        class: member.class,
        types,
        formals,
    }))
}

fn bind_class_parameters(
    parameters: &KlibBodyClassParameters,
    arena: &KlibIrArena,
    raw_class: &KlibIrClass,
) -> Result<(KlibTypeParameters, Vec<String>), KlibBodyDeclineReason> {
    let count = parameters.formals.len();
    if parameters.own_count != count {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "captured class type parameters",
        ));
    }
    if raw_class.type_parameters.len() != count
        || parameters.bounds.len() != count
        || parameters.variances.len() != count
    {
        return Err(mismatch(
            "the serialized class formals are not parallel to the provider contract",
        ));
    }
    let mut types = KlibTypeParameters::default();
    let mut identities = HashSet::default();
    let mut formals = Vec::with_capacity(count);
    for (index, parameter) in raw_class.type_parameters.iter().enumerate() {
        let TyKind::TyParam(identity, _) = parameters.formals[index].kind() else {
            return Err(mismatch("a selected class formal is not a type parameter"));
        };
        let variance = match parameters.variances[index] {
            TypeVariance::Invariant => KlibIrVariance::Invariant,
            TypeVariance::In => KlibIrVariance::In,
            TypeVariance::Out => KlibIrVariance::Out,
        };
        if !identities.insert(identity)
            || parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter
            || !types.bind(&parameter.base.symbol, parameters.formals[index])
            || parameter.is_reified()
            || parameter.variance() != Some(variance)
        {
            return Err(mismatch("the serialized class formal identity, variance or reification disagrees with its provider"));
        }
        formals.push(identity.as_str().to_owned());
    }
    for (index, parameter) in raw_class.type_parameters.iter().enumerate() {
        let bounds = parameter
            .supertypes
            .iter()
            .map(|ty| semantic_type(arena, &types, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        if normalized_bounds(&bounds) != normalized_bounds(&parameters.bounds[index]) {
            return Err(mismatch(
                "the serialized class upper bounds disagree with its provider",
            ));
        }
        if !types.record_bounds(parameters.formals[index], parameters.bounds[index].clone()) {
            return Err(mismatch("a class repeats a semantic formal identity"));
        }
    }
    Ok((types, formals))
}

fn normalized_bounds(bounds: &[Ty]) -> Vec<Ty> {
    if bounds.is_empty() {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    } else {
        bounds.to_vec()
    }
}
