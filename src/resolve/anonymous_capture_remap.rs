//! Reconcile nested capture sources when an anonymous class finalizes its own storage.

use super::receiver_uses::{ReceiverUseSite, ReceiverUseSites};
use super::{AnonymousLexicalClassScope, AnonymousObjectCapture, AnonymousObjectCaptureSource};
use crate::{ast::DeclId, fx_hash::HashMap};

pub(super) struct CaptureFieldRemap<'a> {
    pub(super) fields: &'a [Option<u32>],
    pub(super) original: &'a [AnonymousObjectCapture],
    pub(super) forwarded: &'a ReceiverUseSites,
}

pub(super) fn remap_direct_anonymous_class_storage_captures(
    owner: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    remap: CaptureFieldRemap<'_>,
    captures: &mut HashMap<DeclId, Vec<AnonymousObjectCapture>>,
) -> bool {
    for (&declaration, &candidate_owner) in &lexical_scope.owners {
        if candidate_owner != owner {
            continue;
        }
        let Some(child_captures) = captures.get_mut(&declaration) else {
            continue;
        };
        for capture in child_captures {
            let AnonymousObjectCaptureSource::ClassStorage { field } = capture.source else {
                continue;
            };
            if remap
                .forwarded
                .contains(ReceiverUseSite::Declaration(declaration))
            {
                // This construction moved with its enclosing argument. Its operand belongs to
                // the caller, even when another class-owned use still keeps the original field.
                let Some(original) = remap.original.get(field as usize) else {
                    return false;
                };
                if capture.receiver_role != original.receiver_role {
                    return false;
                }
                capture.source = original.source;
                capture.lexical_binding = original.lexical_binding;
                capture.lexical_shadow_depth = original.lexical_shadow_depth;
                capture.capture_dependency = original.capture_dependency;
            } else {
                let Some(Some(finalized_field)) = remap.fields.get(field as usize) else {
                    return false;
                };
                capture.source = AnonymousObjectCaptureSource::ClassStorage {
                    field: *finalized_field,
                };
            }
        }
    }
    true
}
