//! Builtin implementations selected for external members used by a checked source unit.

use crate::fir::{BuiltinImplementation, ExternalCallableId};
use crate::symbol_resolver::{builtin_implementation_facts, ResolvedMember};
use crate::types::{SemanticCallableOwner, TypeName};

use super::{CallableReferenceTarget, Checker, ExprLowering, ResolvedCall, StmtLowering};

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
    // Callable references carry their selected member or property accessor in the lowering
    // decision rather than the call tables. Freeze the same override facts for those identities;
    // the backend must not reopen the member hierarchy when it realizes a reference.
    for lowering in checker.expr_lowers.values() {
        let target = match lowering {
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => target,
            _ => continue,
        };
        match target {
            CallableReferenceTarget::Classifier(member)
            | CallableReferenceTarget::Member { member, .. } => {
                record_library_member(&mut selected, member)?;
            }
            CallableReferenceTarget::ClassifierProperty { property, .. } => {
                if let Some(getter) = &property.getter {
                    record_library_member(&mut selected, getter)?;
                }
            }
            CallableReferenceTarget::Property(property) => {
                record_property_reference(&mut selected, property)?;
            }
            CallableReferenceTarget::Extension { .. }
            | CallableReferenceTarget::TopLevelProperty(_) => {}
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
                builtin_implementation_facts(checker.libraries, owner, &name, identity),
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
    record_library_member(selected, &member.member)
}

fn record_library_member(
    selected: &mut crate::fx_hash::HashMap<ExternalCallableId, (TypeName, Box<str>)>,
    member: &crate::libraries::LibraryMember,
) -> Result<(), ExternalCallableId> {
    let Some(identity) = member.external_identity else {
        return Ok(());
    };
    let Some(SemanticCallableOwner::Classifier(owner)) = member.declaration_owner else {
        return Err(identity);
    };
    selected
        .entry(identity)
        .or_insert_with(|| (owner, member.name.clone().into_boxed_str()));
    Ok(())
}

fn record_property_reference(
    selected: &mut crate::fx_hash::HashMap<ExternalCallableId, (TypeName, Box<str>)>,
    property: &crate::symbol_resolver::ResolvedPropertyRef,
) -> Result<(), ExternalCallableId> {
    for accessor in std::iter::once(&property.getter).chain(property.setter.iter()) {
        let Some(identity) = accessor.external_identity else {
            continue;
        };
        let Some(SemanticCallableOwner::Classifier(owner)) = accessor.declaration_owner else {
            return Err(identity);
        };
        selected
            .entry(identity)
            .or_insert_with(|| (owner, property.name.clone().into_boxed_str()));
    }
    Ok(())
}
