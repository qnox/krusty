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
    /// The superclass constructor the primary constructor delegates to, unless that is `Any`'s.
    pub(super) superclass: Option<SuperclassCall>,
    /// Every declared supertype, the superclass first.
    pub(super) supertypes: Vec<Ty>,
}

/// The delegation of a primary constructor to its superclass's constructor.
pub(super) struct SuperclassCall {
    pub(super) constructor: KlibDependencyCallable,
    /// The delegation's arguments, in the superclass constructor's parameter order.
    pub(super) arguments: Vec<KlibIrExprId>,
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
    let contract = header.contract;
    if contract.kind != TypeKind::Class
        || !matches!(
            contract.modality,
            KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
        )
        || contract.is_value
        || contract.is_inner
        || contract.is_expect
        || !contract.parameters.formals.is_empty()
        || contract.primary_constructor.as_ref()
            != match &selected.identity().declaration {
                KlibDeclarationSignature::Public(signature) => Some(signature),
                _ => None,
            }
        || header.class.value_class_representation.is_some()
    {
        return Ok(None);
    }
    let open = contract.modality != KotlinModality::Final;
    let mut constructors = Vec::new();
    let mut dispatched = Vec::new();
    for member in &header.class.members {
        match member {
            KlibIrMember::Function(function) => {
                let function = header.arena.function(*function);
                let modality = crate::metadata::semantic::declaration_modality(function.base.flags);
                if function.constructor {
                    constructors.push(&function.base.symbol);
                } else if function.body.is_none() {
                    // A fake override is the inherited declaration; an abstract member has no
                    // implementation to dispatch to. Any other bodyless member is the target's.
                    match modality {
                        _ if function.base.origin == "FAKE_OVERRIDE" => {}
                        KotlinModality::Abstract => dispatched.push(function),
                        _ => return Ok(None),
                    }
                } else if !function.overridden.is_empty()
                    || (open && modality != KotlinModality::Final)
                {
                    dispatched.push(function);
                }
            }
            // A nested class, a companion object included, is a class of its own.
            KlibIrMember::Property(_)
            | KlibIrMember::Field(_)
            | KlibIrMember::Class(_)
            | KlibIrMember::AnonymousInitializer { .. } => {}
            _ => return Ok(None),
        }
    }
    if constructors.as_slice() != [&header.function.base.symbol]
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
    if semantic_type(header.arena, &header.types, receiver.ty)? != Ty::obj_name(contract.classifier)
    {
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
        // The superclass is itself materialized, from its own admitted initialization.
        let constructor = match bodies.callee_origin(
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
        let Some(superclass) = plan(selected[0], bodies, callees)? else {
            return Ok(None);
        };
        if superclass.header.contract.modality == KotlinModality::Final {
            return Err(mismatch("a class extends a final class"));
        }
        let Some(arguments) = arguments.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(None);
        };
        let declared = Ty::obj_name(superclass.header.contract.classifier);
        (
            Some(SuperclassCall {
                constructor,
                arguments,
            }),
            declared,
        )
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
        supertypes,
    }))
}

/// Exact edges of the same admitted initialization which lowering will execute.
pub(super) fn constructor_initialization_callees(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    let Some(plan) = plan(selected, bodies, callees)? else {
        return Ok(None);
    };
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
    let superclass = plan
        .superclass
        .as_ref()
        .map(|superclass| superclass.constructor.clone());
    Ok(Some(
        plan.header
            .arena
            .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
            .into_iter()
            // The class's dispatched members are always emitted with it, as dispatch reaches them.
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
                match bodies.callee_origin(&plan.header.contract.module, &signature) {
                    KlibCalleeOrigin::Exact(identity) => Some(identity),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                }
            })
            // The superclass is initialized first, through its own constructor.
            .chain(superclass)
            .collect(),
    ))
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
    match bodies.callee_origin(
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
    let Some(plan) = plan(selected, bodies, callees)? else {
        return Ok(Vec::new());
    };
    plan.dispatched
        .iter()
        .filter(|function| function.body.is_some())
        .map(|function| {
            let signature = match &function.base.symbol.signature {
                KlibIrSignature::Public(signature) => {
                    KlibDeclarationSignature::Public(signature.clone())
                }
                _ => return Err(mismatch("a dispatched member has no public signature")),
            };
            match bodies.callee_origin(&plan.header.contract.module, &signature) {
                KlibCalleeOrigin::Exact(identity) => Ok(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                    Err(mismatch("a dispatched member has no exact producer"))
                }
            }
        })
        .collect()
}

/// The declarations the overrides of an admitted class override, which their override edges
/// name. They are declaration evidence only: an override edge executes none of their bodies.
pub(super) fn constructor_overridden_declarations(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let Some(plan) = plan(selected, bodies, callees)? else {
        return Ok(Vec::new());
    };
    let mut declarations = Vec::new();
    for symbol in plan
        .dispatched
        .iter()
        .flat_map(|function| &function.overridden)
    {
        let KlibIrSignature::Public(signature) = &symbol.signature else {
            return Err(mismatch(
                "an override names a declaration without a public signature",
            ));
        };
        declarations.extend(
            bodies
                .overridden_declarations(&plan.header.contract.module, signature)
                .map_err(mismatch)?,
        );
    }
    Ok(declarations)
}
