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
    KlibIrArguments, KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrField, KlibIrMember,
    KlibIrStatement,
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
        || contract.modality != KotlinModality::Final
        || contract.is_value
        || contract.is_inner
        || contract.is_expect
        || contract.primary_constructor.as_ref()
            != match &selected.identity().declaration {
                KlibDeclarationSignature::Public(signature) => Some(signature),
                _ => None,
            }
        || header.class.value_class_representation.is_some()
    {
        return Ok(None);
    }
    let mut constructors = Vec::new();
    for member in &header.class.members {
        match member {
            KlibIrMember::Function(function) => {
                let function = header.arena.function(*function);
                if !function.constructor {
                    return Ok(None);
                }
                constructors.push(&function.base.symbol);
            }
            KlibIrMember::Property(_)
            | KlibIrMember::Field(_)
            | KlibIrMember::AnonymousInitializer { .. } => {}
            _ => return Ok(None),
        }
    }
    if constructors.as_slice() != [&header.function.base.symbol]
        || header.function.regular_parameters.iter().any(|parameter| {
            parameter.default_value.is_some() || parameter.vararg_element_type.is_some()
        })
        || header.class.supertypes.len() != 1
    {
        return Ok(None);
    }
    let declared_superclass =
        semantic_type(header.arena, &header.types, header.class.supertypes[0])?;
    if declared_superclass != Ty::obj_name(crate::types::wk::any()) {
        return Ok(None);
    }
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
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a nontrivial primary constructor body",
        ));
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &header.arena.expr(*delegate).kind
    else {
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
            super::ir_builtins::root_constructor(signature, || {
                callees.validate_root_constructor(&contract.module, signature, bodies)
            })?
        }
        _ => false,
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !no_operands
        || !access.type_arguments.is_empty()
        || !root
        || !matches!(&header.arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == &header.class.base.symbol)
    {
        return Err(mismatch(
            "an ordinary class does not delegate to Any and initialize its own instance",
        ));
    }
    let mut steps = Vec::new();
    let storage =
        super::class_storage::authenticate(&header, declared_superclass, |member, field| {
            record_initializer(member, field, &mut steps)
        })?;
    Ok(Some(ClassInitialization {
        header,
        field_declarations: storage.field_declarations,
        storage: storage.storage,
        steps,
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
    for step in &plan.steps {
        match step {
            InitializationStep::Field { value, .. } => roots.push(*value),
            InitializationStep::Block(statements) => {
                roots.extend(plan.header.arena.statement_expression_roots(statements));
            }
        }
    }
    Ok(Some(
        plan.header
            .arena
            .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
            .into_iter()
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
            .collect(),
    ))
}
