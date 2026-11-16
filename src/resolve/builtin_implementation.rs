//! Builtin implementations selected for external members used by a checked source unit.

use crate::fir::{BuiltinImplementation, ExternalCallableId};
use crate::symbol_resolver::builtin_implementation_facts;
use crate::types::SemanticCallableOwner;

use super::selected_external_members::SelectedExternal;
use super::Checker;

pub(super) type Facts = crate::fx_hash::HashMap<ExternalCallableId, Box<[BuiltinImplementation]>>;

/// Derive implementation facts while the checker still owns the normalized source provider.
/// Backends receive the selected declaration identities and this result; they do not traverse
/// the hierarchy or repeat the query.
///
/// The selected declarations are the unit's ledger, which every committed selection records
/// into as the checker makes it (see [`super::selected_external_members`]).
pub(super) fn for_checked_unit(checker: &Checker<'_>) -> Result<Facts, ExternalCallableId> {
    let source = checker.libraries;
    let mut facts = Facts::default();
    for selected in checker.selected_external_members.snapshot() {
        match selected {
            SelectedExternal::Callable(identity) => record_member(&mut facts, source, identity)?,
            SelectedExternal::DefaultProvider(identity) => {
                // The provider is realized as its own declaration, which may be the default
                // holder of the selected one rather than an identity-carrying declaration.
                if let Some(realization) = source.external_callable(identity) {
                    if let (
                        crate::libraries::ExternalCallableKind::Member,
                        Some(SemanticCallableOwner::Classifier(owner)),
                    ) = (realization.kind, realization.declaration_owner)
                    {
                        facts.entry(identity).or_insert_with(|| {
                            builtin_implementation_facts(
                                source,
                                owner,
                                &realization.callable.name,
                                identity,
                            )
                        });
                    }
                }
            }
            SelectedExternal::Property(identity) => {
                let Some(property) = source.external_property(identity) else {
                    continue;
                };
                record_member(&mut facts, source, property.getter)?;
                if let Some(setter) = property.setter {
                    record_member(&mut facts, source, setter)?;
                }
            }
        }
    }

    if let Some(index) = checker.resolved_index {
        record_override_edges(&mut facts, index, source, checker.file_index)?;
    }
    Ok(facts)
}

fn record_member(
    facts: &mut Facts,
    source: &dyn crate::symbol_source::SymbolSource,
    identity: ExternalCallableId,
) -> Result<(), ExternalCallableId> {
    if let std::collections::hash_map::Entry::Vacant(slot) = facts.entry(identity) {
        // A declaration its provider cannot realize has no backend fact to complete; freezing
        // the unit's dependency facts reports it if the unit's IR references it.
        let Some(realization) = source.external_callable(identity) else {
            return Ok(());
        };
        if let Some(implementations) = realized_member_facts(source, identity, realization)? {
            slot.insert(implementations);
        }
    }
    Ok(())
}

/// Override declarations can be referenced by common IR even when no source body calls them (for
/// example, a data class's generated Any methods). Carry the builtin implementation facts of every
/// external declaration an override edge of a `file_index` declaration names, from the same
/// finalized override plan that FIR lowering uses. Module declarations have their plans from
/// Pass 1; a body-local classifier's plan is published after its checked body, so its publisher
/// records these facts again then. Inherited dependency interface defaults are carried the same
/// way.
pub(crate) fn record_override_edges(
    facts: &mut Facts,
    index: &crate::fir::ResolvedModuleIndex,
    source: &dyn crate::symbol_source::SymbolSource,
    file_index: u32,
) -> Result<(), ExternalCallableId> {
    let file = crate::fir::SourceFileId::from_raw(file_index);
    for raw in 0..index.declaration_count() {
        let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
        if !index
            .declaration_anchor(declaration)
            .is_some_and(|anchor| anchor.source == file)
        {
            continue;
        }
        for edge in index.function_overrides(declaration) {
            for (target, owner) in [
                (edge.implementation, edge.implementation_owner),
                (edge.overridden, edge.overridden_owner),
            ] {
                if let crate::fir::ResolvedFunctionOverrideTarget::External(identity) = target {
                    facts.entry(identity).or_insert_with(|| {
                        builtin_implementation_facts(source, owner, &edge.name, identity)
                    });
                }
            }
        }
        for edge in index.property_overrides(declaration) {
            for (target, owner) in [
                (edge.implementation, edge.implementation_owner),
                (edge.overridden, edge.overridden_owner),
            ] {
                if let crate::fir::ResolvedPropertyOverrideTarget::External(identity) = target {
                    facts.entry(identity).or_insert_with(|| {
                        builtin_implementation_facts(source, owner, &edge.name, identity)
                    });
                }
            }
        }
        // A generated delegation forwarder implements the dependency declarations its delegated
        // interface member overrides, as a source override does.
        for delegation in index
            .classifier_header(declaration)
            .map(|header| &*header.interface_delegations)
            .unwrap_or_default()
        {
            for member in &delegation.members {
                match member {
                    crate::fir::ResolvedDelegatedMember::Function(function) => {
                        for overridden in &function.overridden {
                            if let crate::fir::ResolvedFunctionOverrideTarget::External(identity) =
                                overridden.target
                            {
                                record_member(facts, source, identity)?;
                            }
                        }
                    }
                    crate::fir::ResolvedDelegatedMember::Property(property) => {
                        for overridden in &property.overridden {
                            if let crate::fir::ResolvedPropertyOverrideTarget::External(identity) =
                                overridden.target
                            {
                                record_member(facts, source, identity)?;
                            }
                        }
                    }
                }
            }
        }
        // A classifier that inherits a dependency interface's default body without overriding it
        // dispatches to that declaration (or its holder) from its own class tables, and its
        // compatibility override names the declaration whose annotations it carries.
        for default in index.inherited_defaults(declaration).into_iter().flatten() {
            if let Some(
                crate::fir::InheritedDefaultAnnotationOrigin::Function(
                    crate::fir::ResolvedFunctionOverrideTarget::External(identity),
                )
                | crate::fir::InheritedDefaultAnnotationOrigin::Getter(
                    crate::fir::ResolvedPropertyOverrideTarget::External(identity),
                )
                | crate::fir::InheritedDefaultAnnotationOrigin::Setter(
                    crate::fir::ResolvedPropertyOverrideTarget::External(identity),
                ),
            ) = default.annotation_origin
            {
                record_member(facts, source, identity)?;
            }
            let (crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(identity)
            | crate::fir::InheritedDefaultBody::DependencyHolder(identity)) = default.body
            else {
                continue;
            };
            record_member(facts, source, identity)?;
        }
    }
    Ok(())
}

/// [`record_override_edges`] for a `file_index` whose body-local override plans were just
/// published outside a checked unit, such as the classifiers of a retained inline body.
pub(crate) fn published_override_facts(
    index: &crate::fir::ResolvedModuleIndex,
    symbols: &super::SymbolTable,
    file_index: u32,
) -> Result<Facts, ExternalCallableId> {
    use super::CheckerSymbolEnvironment;
    let mut facts = Facts::default();
    record_override_edges(&mut facts, index, symbols.libraries(), file_index)?;
    Ok(facts)
}

/// The builtin implementations of the selected external member `identity`, from its own
/// declaration: its declaring classifier and its property-or-function name. A declaration that is
/// not a member dispatches to nothing.
pub(super) fn for_external_member(
    source: &dyn crate::symbol_source::SymbolSource,
    identity: ExternalCallableId,
) -> Result<Box<[BuiltinImplementation]>, ExternalCallableId> {
    Ok(member_facts(source, identity)?.unwrap_or_default())
}

/// [`for_external_member`] for a member declaration; `None` for a declaration that is not one.
fn member_facts(
    source: &dyn crate::symbol_source::SymbolSource,
    identity: ExternalCallableId,
) -> Result<Option<Box<[BuiltinImplementation]>>, ExternalCallableId> {
    let realization = source.external_callable(identity).ok_or(identity)?;
    realized_member_facts(source, identity, realization)
}

fn realized_member_facts(
    source: &dyn crate::symbol_source::SymbolSource,
    identity: ExternalCallableId,
    realization: crate::libraries::ExternalCallableRealization,
) -> Result<Option<Box<[BuiltinImplementation]>>, ExternalCallableId> {
    if realization.callable.external_identity != Some(identity) {
        return Err(identity);
    }
    if realization.kind != crate::libraries::ExternalCallableKind::Member {
        return Ok(None);
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
    Ok(Some(builtin_implementation_facts(
        source, owner, &name, identity,
    )))
}
