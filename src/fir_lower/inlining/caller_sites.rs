//! The container that names the anonymous objects an inline call regenerates under its caller.

use super::super::BodyLowering;
use crate::fir::DeclarationId;
use crate::ir::IrSiteClassContainer;

impl BodyLowering<'_> {
    /// The container whose code `caller` is, as Kotlin's IR names a local classifier declared
    /// there: the nearest enclosing classifier, by its class in this file when it is local, or
    /// else the caller's package. `None` when the caller's declaration is unknown.
    pub(super) fn caller_site(&self, caller: DeclarationId) -> Option<IrSiteClassContainer> {
        let Some(classifier) = self.index.enclosing_classifier(caller) else {
            let source = self.index.declaration_anchor(caller)?.source;
            return self
                .index
                .source_package(source)
                .map(IrSiteClassContainer::Package);
        };
        match self
            .ir
            .checked_classifier_classes
            .get(&classifier.declaration)
        {
            Some(&class) if self.ir.local_class_name_provenance.contains_key(&class) => {
                Some(IrSiteClassContainer::Class(class))
            }
            _ if self
                .index
                .local_class_name_provenance(classifier.declaration)
                .is_some() =>
            {
                None
            }
            _ => Some(IrSiteClassContainer::Classifier(classifier.classifier)),
        }
    }
}
