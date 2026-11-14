//! Builtin implementations selected for external members used by a checked source unit.

use crate::fir::{BuiltinImplementation, BuiltinOverrideDeclaration, ExternalCallableId};
use crate::symbol_resolver::{builtin_overrides, ResolvedMember};
use crate::symbol_source::SymbolSource;
use crate::types::{SemanticCallableOwner, TypeName};

use super::{Checker, ExprLowering, ResolvedCall, StmtLowering};

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
    nonvirtual.extend(checker.stmt_lowers.values().filter_map(|lowering| {
        let StmtLowering::SuperPropertyWrite { target } = lowering else {
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
    // A property write selects its setter in the statement checker rather than in the call
    // expression tables. Retain that exact external member as well, including Java synthetic
    // properties which have no external Kotlin property identity.
    for lowering in checker.stmt_lowers.values() {
        match lowering {
            StmtLowering::MemberPropertyWrite {
                setter: Some(setter),
                ..
            } => record_property_setter(checker, &mut selected, setter)?,
            StmtLowering::ImplicitPropertyWrite(target) => {
                if let crate::resolve::ImplicitPropertyWriteTarget::Member {
                    setter: Some(setter),
                    ..
                } = target.as_ref()
                {
                    record_property_setter(checker, &mut selected, &setter.callable)?;
                }
            }
            _ => {}
        }
    }

    // Override declarations are checked in Pass 1 and can be referenced by common IR even when
    // no source body calls them (for example, a data class's generated Any methods). Carry their
    // builtin implementation facts from the same finalized override plan that FIR lowering uses.
    if let Some(index) = checker.resolved_index {
        let source = crate::fir::SourceFileId::from_raw(checker.file_index);
        for raw in 0..index.declaration_count() {
            let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
            if !index
                .declaration_anchor(declaration)
                .is_some_and(|anchor| anchor.source == source)
            {
                continue;
            }
            for edge in index.function_overrides(declaration) {
                for (target, owner) in [
                    (edge.implementation, edge.implementation_owner),
                    (edge.overridden, edge.overridden_owner),
                ] {
                    if let crate::fir::ResolvedFunctionOverrideTarget::External(identity) = target {
                        selected
                            .entry(identity)
                            .or_insert_with(|| (owner, edge.name.clone()));
                    }
                }
            }
            for edge in index.property_overrides(declaration) {
                for (target, owner) in [
                    (edge.implementation, edge.implementation_owner),
                    (edge.overridden, edge.overridden_owner),
                ] {
                    if let crate::fir::ResolvedPropertyOverrideTarget::External(identity) = target {
                        selected
                            .entry(identity)
                            .or_insert_with(|| (owner, edge.name.clone()));
                    }
                }
            }
        }
    }

    let mut facts = selected
        .into_iter()
        .map(|(identity, (owner, name))| {
            (
                identity,
                implementation_facts(checker.libraries, owner, &name, identity),
            )
        })
        .collect::<Facts>();
    // A checked `super` target is emitted as a direct call. It cannot dynamically dispatch to a
    // represented builtin override, but still needs an explicit checked record at backend freeze.
    for identity in nonvirtual {
        facts.entry(identity).or_default();
    }
    Ok(facts)
}

/// Complete builtin facts for every exact external member already present in finalized IR. Inline
/// KLIB templates and compiler-generated checked operations can introduce member identities after
/// ordinary source-body checking, so the checker closes the same contract at the checked backend
/// handoff before emission starts.
pub(crate) fn for_checked_external_members(
    source: &dyn SymbolSource,
    identities: impl IntoIterator<Item = ExternalCallableId>,
    already_checked: &Facts,
) -> Result<Facts, ExternalCallableId> {
    let mut facts = Facts::default();
    for identity in identities {
        if let Some(checked) = already_checked.get(&identity) {
            facts.insert(identity, checked.clone());
            continue;
        }
        let realization = source.external_callable(identity).ok_or(identity)?;
        if realization.callable.external_identity != Some(identity) {
            return Err(identity);
        }
        if realization.kind != crate::libraries::ExternalCallableKind::Member {
            continue;
        }
        let Some(SemanticCallableOwner::Classifier(owner)) = realization.declaration_owner else {
            return Err(identity);
        };
        let name = match realization.callable.external_property_identity {
            Some(property) => {
                let property = source.external_property(property).ok_or(identity)?;
                if property.getter != identity && property.setter != Some(identity) {
                    return Err(identity);
                }
                property.name
            }
            None => realization.callable.name,
        };
        facts.insert(
            identity,
            implementation_facts(source, owner, &name, identity),
        );
    }
    Ok(facts)
}

fn implementation_facts(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    identity: ExternalCallableId,
) -> Box<[BuiltinImplementation]> {
    builtin_overrides(source, owner, name, identity)
        .into_iter()
        .map(|implementation| {
            let declaration =
                implementation
                    .declaration
                    .map(|declaration| BuiltinOverrideDeclaration {
                        operation: declaration
                            .callable
                            .compiler_intrinsic
                            .or(declaration.callable.member_realization.compiler_intrinsic()),
                        params: declaration.identity.params,
                        ret: declaration.identity.ret,
                    });
            BuiltinImplementation {
                receiver: implementation.receiver,
                declaration,
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn record_property_setter(
    checker: &Checker<'_>,
    selected: &mut crate::fx_hash::HashMap<ExternalCallableId, (TypeName, Box<str>)>,
    setter: &crate::libraries::LibraryCallable,
) -> Result<(), ExternalCallableId> {
    let Some(identity) = setter.external_identity else {
        return Ok(());
    };
    let Some(SemanticCallableOwner::Classifier(owner)) = setter.declaration_owner else {
        return Err(identity);
    };
    let name = match setter.external_property_identity {
        Some(property) => {
            checker
                .libraries
                .external_property(property)
                .ok_or(identity)?
                .name
        }
        None => setter.name.clone(),
    };
    selected
        .entry(identity)
        .or_insert_with(|| (owner, name.into_boxed_str()));
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_external_member_facts_cover_exact_ir_references() {
        let source = crate::libraries::EmptySymbolSource;
        let (functions, _) = crate::symbol_resolver::declared_member_declarations(
            &source,
            crate::types::Ty::obj_name(crate::types::wk::any()),
            "toString",
        )
        .into_parts();
        let [function] = functions.overloads.as_slice() else {
            panic!("the common class model declares one Any.toString member");
        };
        let identity = function
            .callable
            .external_identity
            .expect("the member has a provider identity");

        let facts = for_checked_external_members(&source, [identity], &Facts::default())
            .expect("the exact checked member has complete override facts");
        assert!(facts.contains_key(&identity));
        assert!(matches!(
            for_checked_external_members(
                &source,
                [ExternalCallableId::from_raw(u32::MAX)],
                &Facts::default(),
            ),
            Err(identity) if identity == ExternalCallableId::from_raw(u32::MAX)
        ));
    }
}
