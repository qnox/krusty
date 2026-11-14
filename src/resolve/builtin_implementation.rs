//! Builtin implementations selected for external members used by a checked source unit.

use crate::fir::{BuiltinImplementation, BuiltinOverrideDeclaration, ExternalCallableId};
use crate::symbol_resolver::{builtin_overrides, ResolvedMember};
use crate::types::{SemanticCallableOwner, TypeName};

use super::{Checker, ExprLowering, ResolvedCall};

pub(super) type Facts = crate::fx_hash::HashMap<ExternalCallableId, Box<[BuiltinImplementation]>>;

/// Derive implementation facts while the checker still owns the normalized source provider.
/// Backends receive the selected declaration identities and this result; they do not traverse
/// the hierarchy or repeat the query.
pub(super) fn for_checked_unit(checker: &Checker<'_>) -> Result<Facts, ExternalCallableId> {
    let mut selected =
        crate::fx_hash::HashMap::<ExternalCallableId, (TypeName, Box<str>)>::default();
    let mut nonvirtual = checker
        .resolved_super_calls
        .values()
        .filter_map(|call| call.external_declaration)
        .collect::<crate::fx_hash::HashSet<_>>();
    nonvirtual.extend(checker.expr_lowers.values().filter_map(|lowering| {
        let ExprLowering::SuperPropertyWrite { target } = lowering else {
            return None;
        };
        target.external_declaration
    }));
    for call in checker
        .resolved_calls
        .values()
        .chain(checker.resolved_operator_calls.values())
        .chain(checker.resolved_stmt_operator_calls.values())
        .chain(
            checker
                .resolved_destructure_components
                .values()
                .map(Box::as_ref),
        )
    {
        if let ResolvedCall::Member(member) = call {
            record_member(&mut selected, member)?;
        }
    }
    for protocol in checker.iterator_protocols.values() {
        for call in [&protocol.iterator, &protocol.has_next, &protocol.next] {
            if let ResolvedCall::Member(member) = call.as_ref() {
                record_member(&mut selected, member)?;
            }
        }
    }
    for target in checker.for_range_iterator_protocols.values() {
        if let ResolvedCall::Member(member) = target.range.as_ref() {
            record_member(&mut selected, member)?;
        }
        for call in [
            &target.protocol.iterator,
            &target.protocol.has_next,
            &target.protocol.next,
        ] {
            if let ResolvedCall::Member(member) = call.as_ref() {
                record_member(&mut selected, member)?;
            }
        }
    }

    // The accessor identity is recorded as a callable, but override matching uses the Kotlin
    // property declaration name. The checked lowering already retains both identities.
    for lowering in checker.expr_lowers.values() {
        if let ExprLowering::MemberPropertyRead {
            accessor: Some(accessor),
            owner,
            name,
            ..
        } = lowering
        {
            if let Some(identity) = accessor.external_identity {
                selected.insert(identity, (*owner, name.clone().into_boxed_str()));
            }
        }
    }

    let mut facts = selected
        .into_iter()
        .map(|(identity, (owner, name))| {
            let implementations = builtin_overrides(checker.libraries, owner, &name, identity)
                .into_iter()
                .map(|implementation| {
                    let declaration =
                        implementation
                            .declaration
                            .map(|declaration| BuiltinOverrideDeclaration {
                                operation: declaration.callable.compiler_intrinsic.or(declaration
                                    .callable
                                    .member_realization
                                    .compiler_intrinsic()),
                                params: declaration.identity.params,
                                ret: declaration.identity.ret,
                            });
                    BuiltinImplementation {
                        receiver: implementation.receiver,
                        declaration,
                    }
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            (identity, implementations)
        })
        .collect::<Facts>();
    // A checked `super` target is emitted as a direct call. It cannot dynamically dispatch to a
    // represented builtin override, but still needs an explicit checked record at backend freeze.
    for identity in nonvirtual {
        facts.entry(identity).or_default();
    }
    Ok(facts)
}

fn record_member(
    selected: &mut crate::fx_hash::HashMap<ExternalCallableId, (TypeName, Box<str>)>,
    member: &ResolvedMember,
) -> Result<(), ExternalCallableId> {
    let Some(identity) = member.member.external_identity else {
        return Ok(());
    };
    let Some(SemanticCallableOwner::Classifier(owner)) = member.member.declaration_owner else {
        return Err(identity);
    };
    selected
        .entry(identity)
        .or_insert_with(|| (owner, member.member.name.clone().into_boxed_str()));
    Ok(())
}
