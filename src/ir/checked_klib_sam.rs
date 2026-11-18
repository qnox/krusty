//! The selected declaration of a functional interface used by serialized dependency bodies.

use super::IrSamTarget;
use crate::libraries::{
    classifier_type_parameter_bounds, sam_application_bindings, sam_substitute, LibraryType,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use std::sync::Arc;

pub(crate) type KlibSamKey = (String, KlibPublicIdSignature);

/// The classifier and abstract-method contract are frozen together before backend entry.
#[derive(Clone)]
pub(crate) struct CheckedKlibSamDeclaration {
    pub(crate) classifier: Arc<LibraryType>,
    pub(crate) target: IrSamTarget,
}

impl std::fmt::Debug for CheckedKlibSamDeclaration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckedKlibSamDeclaration")
            .field("identity", &self.classifier.klib_identity)
            .field("target", &self.target)
            .finish()
    }
}

impl CheckedKlibSamDeclaration {
    /// Apply the selected declaration using only its frozen classifier facts.
    /// This performs no member or hierarchy selection.
    pub(crate) fn applied(&self, applied: crate::types::Ty) -> Option<IrSamTarget> {
        if applied.obj_internal()? != self.target.classifier
            || applied.type_args().len() != self.classifier.type_params().len()
        {
            return None;
        }
        let (logical, capture_read) = sam_application_bindings(&self.classifier, applied);
        let bounds = classifier_type_parameter_bounds(&self.classifier);
        let mut target = self.target.clone();
        target.parameters = self
            .target
            .parameters
            .iter()
            .map(|&p| sam_substitute(p, &bounds, &logical))
            .collect();
        target.contravariant_parameters = self
            .target
            .parameters
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                self.target.contravariant_parameters[i]
                    || !crate::assignable::same_flexible_type(
                        target.parameters[i],
                        sam_substitute(p, &bounds, &capture_read),
                    )
            })
            .collect();
        target.result = sam_substitute(self.target.result, &bounds, &logical);
        Some(target)
    }
}
