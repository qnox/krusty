//! Source-coordinate ownership across the streaming frontend boundary.
//!
//! Most parser coordinates are temporary and disappear before Pass 2. Package-function signature
//! origins are the narrow exception: common IR carries them so a target can locate a diagnostic
//! after representation has selected a physical declaration.

use std::collections::HashMap;

use super::{DeclarationId, ResolvedModuleIndex};
use crate::diag::Span;

#[derive(Debug, Default, PartialEq)]
pub(super) struct PackageFunctionSourceOrigins {
    signatures: HashMap<DeclarationId, Span>,
}

impl PackageFunctionSourceOrigins {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.signatures.len() * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<Span>())
    }
}

impl ResolvedModuleIndex {
    /// Same-pass source coordinate used only while checking retained inline/default fragments.
    /// Production finalization destroys this sidecar before Pass 2 starts.
    pub(crate) fn declaration_range(&self, declaration: DeclarationId) -> Option<Span> {
        self.temporary_declaration_range(declaration)
    }

    pub(crate) fn publish_package_function_signature_span(
        &mut self,
        declaration: DeclarationId,
        span: Span,
    ) {
        let replaced = self
            .package_function_origins
            .signatures
            .insert(declaration, span);
        assert!(
            replaced.is_none(),
            "a package function signature origin may be published only once"
        );
    }

    pub(crate) fn package_function_signature_span(
        &self,
        declaration: DeclarationId,
    ) -> Option<Span> {
        self.package_function_origins
            .signatures
            .get(&declaration)
            .copied()
    }

    pub(crate) fn release_source_coordinates(&mut self) {
        self.release_temporary_source_coordinates();
    }

    pub fn retains_source_coordinates(&self) -> bool {
        self.retains_temporary_source_coordinates()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_function_origin_round_trips_by_stable_declaration() {
        let mut index = ResolvedModuleIndex::default();
        let declaration = DeclarationId::from_raw(7);
        let span = Span::new(11, 29);

        index.publish_package_function_signature_span(declaration, span);

        assert_eq!(
            index.package_function_signature_span(declaration),
            Some(span)
        );
        assert_eq!(
            index.package_function_signature_span(DeclarationId::from_raw(8)),
            None
        );
    }
}
