//! Exact declaration-owned class membership and formal bindings shared by member calls.

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{classifier_name, semantic_type, KlibTypeParameters};
use crate::fx_hash::HashSet;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    KlibAccessorRole, KlibBodyCallable, KlibBodyClassContract, KlibBodyClassParameters,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrTypeParameter, KlibIrVariance,
};
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
    /// The outer instance an inner class's instance holds.
    pub(super) outer: Option<OuterClass<'b>>,
}

/// The class enclosing an inner class: its `this`, which the inner class's bodies read, and its
/// instance type, with the type parameters the inner class captures.
#[derive(Clone, Copy)]
pub(super) struct OuterClass<'b> {
    pub(super) this: &'b crate::metadata::klib_ir::KlibIrSymbol,
    pub(super) ty: Ty,
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

/// Provider-owned class identity, independently of whether this compilation loaded its IR.
pub(super) fn authenticate_contract<'a>(
    selected: KlibBodyCallable<'a>,
) -> Result<&'a KlibBodyClassContract, KlibBodyDeclineReason> {
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
    Ok(class)
}

pub(super) fn authenticate<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
) -> Result<ClassMemberHeader<'a, 'b>, KlibBodyDeclineReason> {
    let class = authenticate_contract(selected)?;
    let class_is_singleton = selected.singleton_classifier() == Some(class.classifier);
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
        let (types, formals, outer) =
            bind_class_parameters(class, bodies, member.arena, member.class)?;
        return Ok(ClassMemberHeader {
            contract: class,
            arena: member.arena,
            function: member.function,
            class: member.class,
            types,
            formals,
            outer,
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
            outer: None,
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
    let (mut types, formals, outer) =
        bind_class_parameters(class, bodies, member.arena, member.class)?;
    super::private_signature_classes::bind(
        &mut types,
        bodies,
        &class.module,
        member.arena,
        member.function,
    )?;
    Ok(ClassMemberHeader {
        contract: class,
        arena: member.arena,
        function: member.function,
        class: member.class,
        types,
        formals,
        outer,
    })
}

/// The raw header of the private primary constructor of `class`, an object or an enum class,
/// which no provider records: the class's own declaration creates every instance of it. The
/// constructor must be a direct member of that exact class header and private; an object's takes
/// no value.
pub(super) fn authenticate_declared_instances_constructor<'a, 'b>(
    class: &'a KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
) -> Result<Option<ClassMemberHeader<'a, 'b>>, KlibBodyDeclineReason> {
    let Some(primary) = class
        .primary_constructor
        .as_ref()
        .filter(|_| class.declares_instances())
    else {
        return Ok(None);
    };
    if class.signature.member_id().is_some()
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch("a class's identity disagrees with its classifier"));
    }
    let identity = crate::libraries::KlibDependencyCallable {
        module: class.module.clone(),
        declaration: crate::libraries::KlibDeclarationSignature::Public(primary.clone()),
    };
    // A class of a module compiled from source alongside has no serialized declaration: its own
    // file lays it out, never a support unit.
    let Some(member) = bodies
        .class_member_header(&identity, &class.signature, Some(KlibAccessorRole::Getter))
        .map_err(mismatch)?
    else {
        return Ok(None);
    };
    let object = class.kind == crate::libraries::TypeKind::Object;
    if member.class.base.symbol.kind != KlibIrSymbolKind::Class
        || !member.function.constructor
        || crate::metadata::semantic::declaration_visibility(member.function.base.flags)
            != crate::types::Visibility::Private
        || (object && !member.function.regular_parameters.is_empty())
    {
        return Err(mismatch(
            "a primary constructor is not its class's private constructor, taking no value for an object",
        ));
    }
    let (types, formals, outer) = bind_class_parameters(class, bodies, member.arena, member.class)?;
    Ok(Some(ClassMemberHeader {
        contract: class,
        arena: member.arena,
        function: member.function,
        class: member.class,
        types,
        formals,
        outer,
    }))
}

type BoundClass<'b> = (KlibTypeParameters, Vec<String>, Option<OuterClass<'b>>);

fn bind_class_parameters<'b>(
    class: &KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
    arena: &'b KlibIrArena,
    raw_class: &'b KlibIrClass,
) -> Result<BoundClass<'b>, KlibBodyDeclineReason> {
    let parameters = &class.parameters;
    let formals = class_parameter_identities(parameters)?;
    let outer = if class.is_inner {
        Some(
            enclosing_class(class, bodies)?
                .ok_or_else(|| mismatch("an inner class's enclosing class is not serialized"))?,
        )
    } else {
        None
    };
    let scope = serialized_class_scope(class, bodies, arena, raw_class)?;
    if scope.len() != formals.len() {
        return Err(mismatch(
            "the serialized class formals are not parallel to the provider contract",
        ));
    }
    let mut types = KlibTypeParameters::default();
    for (index, (_, parameter)) in scope.iter().enumerate() {
        let variance = match parameters.variances[index] {
            TypeVariance::Invariant => KlibIrVariance::Invariant,
            TypeVariance::In => KlibIrVariance::In,
            TypeVariance::Out => KlibIrVariance::Out,
        };
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter
            || !types.bind(&parameter.base.symbol, parameters.formals[index])
            || parameter.is_reified()
            || parameter.variance() != Some(variance)
        {
            return Err(mismatch("the serialized class formal identity, variance or reification disagrees with its provider"));
        }
    }
    for (index, (arena, parameter)) in scope.iter().enumerate() {
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
        if !types.record_bounds(parameters.formals[index], parameters.bounds[index].clone())
            || !types.record_variance(parameters.formals[index], parameters.variances[index])
        {
            return Err(mismatch("a class repeats a semantic formal identity"));
        }
    }
    super::foreign_type_parameters::bind(&mut types, arena, bodies, &class.module)?;
    let outer = match outer {
        Some((outer_arena, outer)) => {
            let this = outer
                .this_receiver
                .as_ref()
                .ok_or_else(|| mismatch("an outer class has no instance receiver"))?;
            Some(OuterClass {
                this: &this.base.symbol,
                ty: semantic_type(outer_arena, &types, this.ty)?,
            })
        }
        None => None,
    };
    Ok((types, formals, outer))
}

/// The serialized class directly enclosing the nested class `class`, which its module declares.
fn enclosing_class<'b>(
    class: &KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
) -> Result<Option<(&'b KlibIrArena, &'b KlibIrClass)>, KlibBodyDeclineReason> {
    let Some(outer) = class.classifier.nested_owner() else {
        return Ok(None);
    };
    bodies
        .class_header(
            &class.module,
            &crate::metadata::id_signature::KlibPublicIdSignature::classifier(outer),
        )
        .map_err(mismatch)
}

/// The serialized type parameters a class puts in scope, in the provider's formal order: its own,
/// then, for an `inner` class, every one its enclosing class puts in scope. Kotlin serializes only
/// a class's own parameters on it; an inner class's body names its outer classes' parameters by
/// their declarations, so they bind to the formals the class captures.
fn serialized_class_scope<'a>(
    class: &KlibBodyClassContract,
    bodies: &'a KlibDeclarationBodies,
    arena: &'a KlibIrArena,
    raw_class: &'a KlibIrClass,
) -> Result<Vec<(&'a KlibIrArena, &'a KlibIrTypeParameter)>, KlibBodyDeclineReason> {
    let count = class.parameters.formals.len();
    let mut scope = raw_class
        .type_parameters
        .iter()
        .map(|parameter| (arena, parameter))
        .collect::<Vec<_>>();
    if scope.len() != class.parameters.own_count || (!class.is_inner && scope.len() != count) {
        return Err(mismatch(
            "the serialized class formals are not parallel to the provider contract",
        ));
    }
    let mut classifier = class.classifier;
    while scope.len() < count {
        let outer = classifier
            .nested_owner()
            .ok_or_else(|| mismatch("an inner class captures formals of no enclosing class"))?;
        let (outer_arena, outer_class) = bodies
            .class_header(
                &class.module,
                &crate::metadata::id_signature::KlibPublicIdSignature::classifier(outer),
            )
            .map_err(mismatch)?
            .ok_or_else(|| mismatch("an inner class's enclosing class is not serialized"))?;
        scope.extend(
            outer_class
                .type_parameters
                .iter()
                .map(|parameter| (outer_arena, parameter)),
        );
        classifier = outer;
    }
    Ok(scope)
}

/// The provider's class formals are also the declared frame for an already emitted initializer.
/// Raw-header binding and retained declarations use the same normalized identity validation.
pub(super) fn class_parameter_identities(
    parameters: &KlibBodyClassParameters,
) -> Result<Vec<String>, KlibBodyDeclineReason> {
    let count = parameters.formals.len();
    if parameters.own_count > count {
        return Err(mismatch(
            "a class declares more formals than it puts in scope",
        ));
    }
    if parameters.bounds.len() != count || parameters.variances.len() != count {
        return Err(mismatch("the selected class formal facts are not parallel"));
    }
    let mut identities = HashSet::default();
    let mut formals = Vec::with_capacity(count);
    for formal in &parameters.formals {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return Err(mismatch("a selected class formal is not a type parameter"));
        };
        if !identities.insert(identity) {
            return Err(mismatch("a class repeats a semantic formal identity"));
        }
        formals.push(identity.as_str().to_owned());
    }
    Ok(formals)
}

fn normalized_bounds(bounds: &[Ty]) -> Vec<Ty> {
    if bounds.is_empty() {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    } else {
        bounds.to_vec()
    }
}
