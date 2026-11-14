//! An authenticated, bounded ordinary-class initialization capability.
//!
//! Planning and lowering use this same declaration-owned plan. Fields come from actual serialized
//! members; constructor parameters never imply storage. Other class representations remain target
//! constructor leaves, while an admitted class with invalid initialization declines transactionally.

use super::class_headers::{self, ClassMemberHeader};
use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::libraries::{
    KlibBodyCallable, KlibClassStorage, KlibDeclarationSignature, KlibDependencyCallable, TypeKind,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrField, KlibIrFunction,
    KlibIrMember, KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::metadata::semantic::KotlinModality;
use crate::types::Ty;

pub(super) enum InitializationStep<'a> {
    Field { index: u32, value: KlibIrExprId },
    Block(&'a [KlibIrStatement]),
}

/// One member's contribution to initialization, in member order. A field store is the field's own
/// initializer expression; an anonymous initializer is its statement list. Constructor parameter
/// names never create a store.
pub(super) fn record_initializer<'a>(
    member: &'a KlibIrMember,
    field: Option<(u32, &'a KlibIrField)>,
    steps: &mut Vec<InitializationStep<'a>>,
) -> Result<(), KlibBodyDeclineReason> {
    if let KlibIrMember::AnonymousInitializer { base, statements } = member {
        if base.flags != 0 {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a static or flagged class initializer",
            ));
        }
        steps.push(InitializationStep::Block(statements));
    }
    if let Some((index, field)) = field {
        if let Some(value) = field.initializer {
            steps.push(InitializationStep::Field { index, value });
        }
    }
    Ok(())
}

pub(super) struct ClassInitialization<'a, 'b> {
    pub(super) header: ClassMemberHeader<'a, 'b>,
    pub(super) field_declarations: Vec<&'b KlibIrField>,
    pub(super) storage: KlibClassStorage,
    pub(super) steps: Vec<InitializationStep<'b>>,
    /// The members dispatch on an instance of the class reaches: its overrides with bodies, and
    /// the open and abstract members an open class declares. A materialized class always carries
    /// them, so dispatch never reaches an implementation the instance's class overrides.
    pub(super) dispatched: Vec<&'b KlibIrFunction>,
    /// The superclass constructor the constructor delegates to, unless that is `Any`'s.
    pub(super) superclass: Option<SuperclassCall>,
    /// Whether the constructor is the class's primary one. A class without a primary constructor
    /// is initialized by each of its secondary constructors, which all delegate to the superclass.
    pub(super) primary: bool,
    /// Every declared supertype, the superclass first.
    pub(super) supertypes: Vec<Ty>,
}

/// The delegation of a constructor to its superclass's constructor.
pub(super) struct SuperclassCall {
    pub(super) constructor: KlibDependencyCallable,
    /// The delegation's arguments, in the superclass constructor's parameter order.
    pub(super) arguments: Vec<KlibIrExprId>,
    /// The superclass, and the parameters its selected constructor declares.
    pub(super) owner: crate::types::TypeName,
    pub(super) params: Vec<Ty>,
    /// Whether the unit materializes the superclass from its own admitted initialization. A
    /// superclass the target lays out itself is the target's, as `Any` is.
    pub(super) materialized: bool,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// A positive common representation, independently of a target's runtime constructor registry.
pub(super) fn plan<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    let header = class_headers::authenticate(selected, bodies)?;
    if header
        .contract
        .primary_constructor
        .as_ref()
        .is_some_and(|primary| {
            selected.identity().declaration != KlibDeclarationSignature::Public(primary.clone())
        })
    {
        return Ok(None);
    }
    admit(header, TypeKind::Class, bodies, callees)
}

/// The initialization of the object `class`, through its private primary constructor, which no
/// provider records: the object's own declaration is the only creator of its instance.
pub(super) fn object_plan<'a, 'b>(
    class: &'a crate::libraries::KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    match class_headers::authenticate_object_constructor(class, bodies)? {
        Some(header) => admit(header, TypeKind::Object, bodies, callees),
        None => Ok(None),
    }
}

/// The initialization of a class of `kind` by the constructor `header` authenticates.
fn admit<'a, 'b>(
    header: ClassMemberHeader<'a, 'b>,
    kind: TypeKind,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    let contract = header.contract;
    // A class the target lays out itself is the target's to construct, as `Any` is.
    if contract.kind != kind
        || callees.target_represents(contract.classifier)
        || !matches!(
            contract.modality,
            KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
        )
        || contract.is_value
        || contract.is_inner
        || contract.is_expect
        || header.class.value_class_representation.is_some()
    {
        return Ok(None);
    }
    let open = contract.modality != KotlinModality::Final;
    let mut constructors = Vec::new();
    let mut dispatched = Vec::new();
    // A member function or property accessor dispatch reaches: an override with a body, an open
    // class's open member, or an abstract member. A fake override is the inherited declaration;
    // any other bodyless member is the target's, and the class keeps that target boundary.
    let mut dispatch = |function: &'b KlibIrFunction| -> bool {
        let modality = crate::metadata::semantic::declaration_modality(function.base.flags);
        if function.body.is_none() {
            match modality {
                _ if function.base.origin == "FAKE_OVERRIDE" => {}
                KotlinModality::Abstract => dispatched.push(function),
                _ => return false,
            }
        } else if !function.overridden.is_empty() || (open && modality != KotlinModality::Final) {
            dispatched.push(function);
        }
        true
    };
    for member in &header.class.members {
        match member {
            KlibIrMember::Function(function) => {
                let function = header.arena.function(*function);
                if function.constructor {
                    constructors.push(&function.base.symbol);
                } else if !dispatch(function) {
                    return Ok(None);
                }
            }
            KlibIrMember::Property(property) => {
                for accessor in [property.getter, property.setter].into_iter().flatten() {
                    if !dispatch(header.arena.function(accessor)) {
                        return Ok(None);
                    }
                }
            }
            // A nested class, a companion object included, is a class of its own.
            KlibIrMember::Field(_)
            | KlibIrMember::Class(_)
            | KlibIrMember::AnonymousInitializer { .. } => {}
            _ => return Ok(None),
        }
    }
    // A materialized class carries every member dispatch reaches, so a class whose members the
    // unit does not emit with it keeps its target boundary rather than losing its overrides.
    if !dispatched.is_empty() && !contract.emits_members() {
        return Ok(None);
    }
    let primary = contract.primary_constructor.is_some();
    // A class with a primary constructor has only that one here; any secondary constructor
    // delegates to it and is not an initialization of its own. Without one, the selected secondary
    // constructor initializes the class.
    if (primary && constructors.as_slice() != [&header.function.base.symbol])
        || !constructors.contains(&&header.function.base.symbol)
        || header.function.regular_parameters.iter().any(|parameter| {
            parameter.default_value.is_some() || parameter.vararg_element_type.is_some()
        })
    {
        return Ok(None);
    }
    let supertypes = header
        .class
        .supertypes
        .iter()
        .map(|&supertype| semantic_type(header.arena, &header.types, supertype))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(KlibIrBody::Block(statements)) = &header.function.body else {
        // A bodyless or synthetic constructor retains its separately authenticated target boundary.
        return Ok(None);
    };
    let receiver = header
        .class
        .this_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a materialized class has no serialized instance receiver"))?;
    if semantic_type(header.arena, &header.types, receiver.ty)? != contract.self_type() {
        return Err(mismatch(
            "a class instance receiver has another declared type",
        ));
    }
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        // A constructor body with more than its delegation and instance initialization keeps
        // its separately authenticated target boundary, as a bodyless one does.
        return Ok(None);
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &header.arena.expr(*delegate).kind
    else {
        return Err(mismatch(
            "a primary constructor does not delegate before initialization",
        ));
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !access.type_arguments.is_empty()
        || !matches!(&header.arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == &header.class.base.symbol)
    {
        return Err(mismatch(
            "an ordinary class does not delegate to its superclass and initialize its own instance",
        ));
    }
    let KlibIrSignature::Public(signature) = &access.symbol.signature else {
        return Ok(None);
    };
    let arguments = match &access.arguments {
        KlibIrArguments::Flat(values) => values.clone(),
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => values.clone(),
        KlibIrArguments::Split { .. } => {
            return Err(mismatch(
                "a superclass constructor delegation passes a receiver",
            ))
        }
    };
    let root = super::ir_builtins::root_constructor(signature, || {
        callees.validate_root_constructor(&contract.module, signature, bodies)
    })?;
    let (superclass, declared_superclass) = if root {
        if !arguments.is_empty() {
            return Err(mismatch(
                "a delegation to `Any`'s constructor passes arguments",
            ));
        }
        (None, Ty::obj_name(crate::types::wk::any()))
    } else {
        // The superclass constructor the delegation names, by its exact declaration.
        let constructor = match bodies.linked_origin(
            &contract.module,
            &KlibDeclarationSignature::Public(signature.clone()),
        ) {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                return Err(mismatch(
                    "a superclass constructor delegation names no unique declaration",
                ))
            }
        };
        let Some(super::callee_facts::KlibCalleeFact::Selected(selected)) =
            callees.get(&constructor)
        else {
            return Ok(None);
        };
        let Some(arguments) = arguments.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(None);
        };
        let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
            selected[0].declaration_owner()
        else {
            return Err(mismatch("a superclass constructor has no declaring class"));
        };
        let params = selected[0].params().to_vec();
        if callees.target_represents(owner) {
            // The target lays the superclass out itself and builds it from the delegation's
            // arguments, as it does for a source class extending it.
            (
                Some(SuperclassCall {
                    constructor,
                    arguments,
                    owner,
                    params,
                    materialized: false,
                }),
                Ty::obj_name(owner),
            )
        } else {
            let Some(superclass) = plan(selected[0], bodies, callees)? else {
                return Ok(None);
            };
            if superclass.header.contract.modality == KotlinModality::Final {
                return Err(mismatch("a class extends a final class"));
            }
            (
                Some(SuperclassCall {
                    constructor,
                    arguments,
                    owner: superclass.header.contract.classifier,
                    params,
                    materialized: true,
                }),
                Ty::obj_name(superclass.header.contract.classifier),
            )
        }
    };
    if supertypes.first() != Some(&declared_superclass)
        && !(root
            && supertypes
                .iter()
                .all(|supertype| *supertype != declared_superclass))
    {
        return Err(mismatch(
            "a class's first supertype is not the class its constructor delegates to",
        ));
    }
    let mut steps = Vec::new();
    let storage =
        super::class_storage::authenticate(&header, declared_superclass, |member, field| {
            record_initializer(member, field, &mut steps)
        })?;
    let mut supertypes = supertypes;
    if root {
        supertypes.retain(|supertype| *supertype != declared_superclass);
        supertypes.insert(0, declared_superclass);
    }
    Ok(Some(ClassInitialization {
        header,
        field_declarations: storage.field_declarations,
        storage: storage.storage,
        steps,
        dispatched,
        superclass,
        primary,
        supertypes,
    }))
}

/// Exact edges of the same admitted initialization which lowering will execute.
pub(super) fn constructor_initialization_callees(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    Ok(plan(selected, bodies, callees)?.map(|plan| initialization_callees(&plan, bodies)))
}

/// Exact edges of the admitted initialization of the object `class`.
pub(super) fn object_initialization_callees(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    Ok(object_plan(class, bodies, callees)?.map(|plan| initialization_callees(&plan, bodies)))
}

fn initialization_callees(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Vec<KlibDependencyCallable> {
    let roots = initialization_roots(plan);
    let superclass = plan
        .superclass
        .as_ref()
        .filter(|superclass| superclass.materialized)
        .map(|superclass| superclass.constructor.clone());
    plan.header
        .arena
        .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
        .into_iter()
        .chain(plan.dispatched.iter().map(|function| &function.base.symbol))
        .filter_map(|symbol| match &symbol.signature {
            KlibIrSignature::Public(signature) => {
                Some(KlibDeclarationSignature::Public(signature.clone()))
            }
            KlibIrSignature::Accessor(signature) => {
                Some(KlibDeclarationSignature::Accessor(signature.clone()))
            }
            _ => None,
        })
        .filter_map(|signature| {
            match bodies.linked_origin(&plan.header.contract.module, &signature) {
                KlibCalleeOrigin::Exact(identity) => Some(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
            }
        })
        .chain(superclass)
        .collect()
}

/// Object reads of the exact same admitted ordinary constructor initialization.
pub(super) fn constructor_initialization_objects(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibPublicIdSignature>, KlibBodyDeclineReason> {
    Ok(plan(selected, bodies, callees)?
        .map(|plan| initialization_objects(&plan))
        .unwrap_or_default())
}

pub(super) fn object_initialization_objects(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibPublicIdSignature>, KlibBodyDeclineReason> {
    let Some(plan) = object_plan(class, bodies, callees)? else {
        return Ok(Vec::new());
    };
    Ok(initialization_objects(&plan))
}

fn initialization_objects(plan: &ClassInitialization<'_, '_>) -> Vec<KlibPublicIdSignature> {
    plan.header
        .arena
        .emitted_object_symbols_from(initialization_roots(plan), super::EMITTED_CALL_FORMS)
        .into_iter()
        .filter_map(|symbol| match &symbol.signature {
            KlibIrSignature::Public(signature) if symbol.kind == KlibIrSymbolKind::Class => {
                Some(signature.clone())
            }
            _ => None,
        })
        .collect()
}

fn initialization_roots(plan: &ClassInitialization<'_, '_>) -> Vec<KlibIrExprId> {
    let mut roots = Vec::new();
    if let Some(superclass) = &plan.superclass {
        roots.extend(superclass.arguments.iter().copied());
    }
    for step in &plan.steps {
        match step {
            InitializationStep::Field { value, .. } => roots.push(*value),
            InitializationStep::Block(statements) => {
                roots.extend(plan.header.arena.statement_expression_roots(statements));
            }
        }
    }
    roots
}

/// The superclass constructor the primary constructor `identity` serializes a delegation to,
/// read from its body alone. Admitting the class's initialization needs that constructor's own
/// declaration, so a closure records it before asking.
pub(crate) fn delegated_superclass_constructor(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Option<KlibDependencyCallable> {
    let (arena, function) = bodies.function(identity)?;
    if !function.constructor {
        return None;
    }
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return None;
    };
    let Some(KlibIrStatement::Expression(delegate)) = statements.first() else {
        return None;
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return None;
    };
    let KlibIrSignature::Public(signature) = &access.symbol.signature else {
        return None;
    };
    match bodies.linked_origin(
        &identity.module,
        &KlibDeclarationSignature::Public(signature.clone()),
    ) {
        KlibCalleeOrigin::Exact(constructor) => Some(constructor),
        KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
    }
}

/// The members of an admitted class that dispatch reaches, which are emitted with the class
/// whatever a call of one would fold to: a virtual call runs the member's own body.
pub(super) fn constructor_dispatched_members(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match plan(selected, bodies, callees)? {
        Some(plan) => dispatched_members(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

/// The members dispatch reaches of the admitted object `class`.
pub(super) fn object_dispatched_members(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match object_plan(class, bodies, callees)? {
        Some(plan) => dispatched_members(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

fn dispatched_members(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    plan.dispatched
        .iter()
        .filter(|function| function.body.is_some())
        .map(|function| {
            let signature =
                crate::klib_libraries::member_signature(&function.base.symbol.signature)
                    .ok_or_else(|| mismatch("a dispatched member is not a member"))?;
            match bodies.linked_origin(&plan.header.contract.module, &signature) {
                KlibCalleeOrigin::Exact(identity) => Ok(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                    Err(mismatch("a dispatched member has no exact producer"))
                }
            }
        })
        .collect()
}

/// The declarations an admitted class initialization names without emitting their bodies: the
/// ones its overrides override, which their override edges name, and the constructor of a
/// superclass the target lays out, which the target builds. They are declaration evidence only.
pub(super) fn constructor_declaration_evidence(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match plan(selected, bodies, callees)? {
        Some(plan) => overridden_declarations(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

/// The declarations the overrides of the admitted object `class` override.
pub(super) fn object_overridden_declarations(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match object_plan(class, bodies, callees)? {
        Some(plan) => overridden_declarations(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

fn overridden_declarations(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let mut declarations = plan
        .superclass
        .iter()
        .filter(|superclass| !superclass.materialized)
        .map(|superclass| superclass.constructor.clone())
        .collect::<Vec<_>>();
    for symbol in plan
        .dispatched
        .iter()
        .flat_map(|function| &function.overridden)
    {
        let signature = crate::klib_libraries::member_signature(&symbol.signature)
            .ok_or_else(|| mismatch("an override names a declaration that is not a member"))?;
        declarations.extend(
            bodies
                .overridden_declarations(&plan.header.contract.module, &signature)
                .map_err(mismatch)?,
        );
    }
    Ok(declarations)
}
