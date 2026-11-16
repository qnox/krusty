//! The selected declaration of a functional interface used by serialized dependency bodies.

use super::IrSamTarget;
use crate::libraries::LibraryType;
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
