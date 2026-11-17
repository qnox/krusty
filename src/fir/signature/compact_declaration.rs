//! Compact declaration publication for the stable index.
//!
//! One declaration publishes its semantic header, qualified emission name, and, for a classifier,
//! its identity together. A hoisted classifier keeps the written source segment as an alias of that
//! same declaration; the emission name stays the qualified lookup identity.

use super::{DeclarationId, DeclarationKind, ResolvedDeclarationHeader, ResolvedModuleIndex};
use crate::types::TypeName;

impl ResolvedModuleIndex {
    /// Publish one compact declaration before header syntax and lookup spellings are destroyed.
    ///
    /// `emission_name` is the qualified lookup identity stored on the header. `source_spelling` is
    /// the written segment of a hoisted classifier, indexed on the same [`DeclarationId`] when it
    /// differs. `classifier` is that declaration's semantic identity, required for a classifier.
    pub(crate) fn publish_compact_declaration(
        &mut self,
        declaration: DeclarationId,
        header: ResolvedDeclarationHeader,
        emission_name: Option<&str>,
        source_spelling: Option<&str>,
        classifier: Option<TypeName>,
    ) {
        let kind = header.kind;
        self.publish_declaration_header(declaration, header, emission_name);
        if kind != DeclarationKind::Classifier {
            return;
        }
        if let Some(spelling) = source_spelling {
            if emission_name != Some(spelling) {
                let name = self.intern_declaration_name(spelling);
                let entries = self.declarations_by_name.entry(name).or_default();
                if !entries.contains(&declaration) {
                    entries.push(declaration);
                }
            }
        }
        self.publish_classifier_identity(
            declaration,
            classifier.expect("a compact classifier publishes its semantic identity"),
        );
    }
}
