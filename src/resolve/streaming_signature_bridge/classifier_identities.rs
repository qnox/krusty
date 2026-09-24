//! Stable classifier identities published by the compact header inventory.

use super::*;
use crate::fir::DeclarationKind;

pub(super) fn compact_classifier_identities(
    headers: &crate::fir::StreamedHeaderModule,
) -> HashMap<crate::fir::DeclarationId, TypeName> {
    headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == DeclarationKind::Classifier)
        .map(|stub| {
            let (_, identity) =
                super::super::signature_collection::compact_classifier_identity(headers, stub)
                    .expect("every classifier stub must retain its stable semantic identity");
            (stub.id, identity)
        })
        .collect()
}
