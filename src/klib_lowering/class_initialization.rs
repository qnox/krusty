//! Ordinary-class initialization steps and the exact edges an admitted class executes.
//!
//! Fields come from actual serialized members; constructor parameters never imply storage. Edge
//! planning and lowering share one prepared class, so an admitted class with invalid
//! initialization declines transactionally in both.

use super::decline::KlibBodyDeclineReason;
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrField, KlibIrMember, KlibIrStatement};
use crate::metadata::klib_ir::KlibIrSignature;

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

/// Exact edges of the same admitted class which lowering will execute: its initialization and
/// every member it installs with the class.
pub(super) fn constructor_initialization_callees(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    let module = selected.identity().module.clone();
    let Some((prepared, _)) = super::same_library_class::prepare_public(
        super::same_library_class::LocalClassSource {
            bodies,
            module: &module,
            validate_root: &|root| callees.validate_root_constructor(&module, root, bodies),
        },
        selected,
    )?
    else {
        return Ok(None);
    };
    let mut roots = Vec::new();
    for step in &prepared.steps {
        match step {
            InitializationStep::Field { value, .. } => roots.push(*value),
            InitializationStep::Block(statements) => {
                roots.extend(prepared.arena.statement_expression_roots(statements));
            }
        }
    }
    for member in &prepared.members {
        roots.extend(prepared.arena.statement_expression_roots(member.statements));
    }
    // The class's own members are installed with it rather than reached as declarations.
    let own = prepared
        .members
        .iter()
        .map(|member| &member.signature)
        .collect::<Vec<_>>();
    Ok(Some(
        prepared
            .arena
            .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
            .into_iter()
            .filter(|symbol| !own.contains(&&symbol.signature))
            .filter_map(|symbol| match &symbol.signature {
                KlibIrSignature::Public(signature) => {
                    Some(KlibDeclarationSignature::Public(signature.clone()))
                }
                KlibIrSignature::Accessor(signature) => {
                    Some(KlibDeclarationSignature::Accessor(signature.clone()))
                }
                _ => None,
            })
            .filter_map(
                |signature| match bodies.callee_origin(&module, &signature) {
                    KlibCalleeOrigin::Exact(identity) => Some(identity),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect(),
    ))
}
