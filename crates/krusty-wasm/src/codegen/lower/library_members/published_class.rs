//! Physical dispatch through a class already emitted by a dependency module.

use krusty::backend::{
    instantiate_dependency_signature, BackendCallableFact, BackendClassifierSource,
};
use krusty::ir::Callee;
use krusty::libraries::{ExternalCallableKind, MemberRealization};
use krusty::types::Ty;

use super::super::super::super::encode::{Module, ValType};
use super::super::super::super::objects::REFERENCE;
use super::super::super::laid_out_classes::LaidOutClasses;
use super::super::{carrier, Unsupported};
use super::{library_member_identity, library_member_owner, MemberRole};

pub(in crate::codegen) struct PublishedMember {
    pub(in crate::codegen) table: u32,
    pub(in crate::codegen) slot: u32,
    pub(in crate::codegen) signature: u32,
    pub(in crate::codegen) applied_parameters: Box<[Ty]>,
    pub(in crate::codegen) applied_result: Ty,
}

/// The same selected declaration controls both dependency prewalking and body emission.
/// No executable export is required for an abstract declaration's physical table slot.
pub(in crate::codegen) fn select(
    fact: &BackendCallableFact,
    role: MemberRole,
    callee: &Callee,
    classifiers: &dyn BackendClassifierSource,
    layouts: &LaidOutClasses,
    module: &mut Module,
) -> Option<Result<PublishedMember, Unsupported>> {
    if role != MemberRole::Function
        || !fact.builtin_implementations.is_empty()
        || fact.kind != ExternalCallableKind::Member
        || fact.member_realization != MemberRealization::Dispatch
        || fact.compiler_operation().is_some()
        || fact.owner_is_interface
    {
        return None;
    }
    let owner = library_member_owner(fact)?;
    let identity = classifiers.classifier(owner)?.klib_identity.clone()?;
    let Some(modifiers) = fact.klib_body_declaration_modifiers else {
        return Some(Err(
            "a selected class member without its authenticated declaration modality".into(),
        ));
    };
    if !modifiers.is_overridable() {
        return None;
    }
    let layout = match layouts.published_layout(&identity) {
        Ok(Some(layout)) => layout,
        Ok(None) => {
            return Some(Err(
                "a selected class member without its producer layout".into()
            ))
        }
        Err(reason) => return Some(Err(reason)),
    };
    Some((|| {
        if fact
            .klib_body_declaration_modifiers
            .is_some_and(|modifiers| modifiers.is_suspend())
        {
            return Err(
                "a suspend class member requires its authenticated continuation frame".into(),
            );
        }
        let applied = instantiate_dependency_signature(fact, callee).map_err(str::to_owned)?;
        let Callee::External { params, ret, .. } = callee else {
            return Err("a published member without its checked external call".into());
        };
        if applied.parameters.as_ref() != params.as_slice()
            || applied.result != *ret
            || applied.receiver.is_some()
        {
            return Err("a published class member disagrees with its checked application".into());
        }
        let identity = library_member_identity(fact, role)
            .ok_or("a published class member without its exact declaration identity")?;
        let slot = *layout
            .declaration_slots
            .get(&identity)
            .ok_or_else(|| {
                format!(
                    "a selected class member without its producer slot binding: owner={owner:?}, declaration={:?}, requested_identity={identity:?}, published_declarations={}, physical_slots={}",
                    fact.dependency_callable,
                    layout.declaration_slots.len(),
                    layout.slot_types.len(),
                )
            })?;
        let signature = layout
            .slot_types
            .get(slot as usize)
            .copied()
            .flatten()
            .ok_or("a published class member slot without its physical signature")?;
        let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
        let result = fact.declared_ret.unwrap_or(fact.ret);
        let mut frame = vec![REFERENCE];
        for &parameter in params {
            frame.push(carrier(parameter)?.ok_or("a `Unit` class member parameter")?);
        }
        let results: Vec<ValType> = carrier(result)?.into_iter().collect();
        if module.func_type(frame, results) != signature {
            return Err("a selected class member frame differing from its producer slot".into());
        }
        Ok(PublishedMember {
            table: layout.table_type,
            slot,
            signature,
            applied_parameters: applied.parameters,
            applied_result: applied.result,
        })
    })())
}
