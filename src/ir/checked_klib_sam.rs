//! The selected declaration of a functional interface used by serialized dependency bodies.

use super::IrSamTarget;
use crate::libraries::{KlibDependencyCallable, LibraryType};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::types::Ty;
use std::sync::Arc;

pub(crate) type KlibSamKey = (String, KlibPublicIdSignature);

/// Instantiates a frozen declaration at one applied type of its classifier.
pub(crate) type ApplySamDeclaration = fn(&LibraryType, &IrSamTarget, Ty) -> Option<IrSamTarget>;

/// The classifier and abstract-method contract are frozen together before backend entry.
#[derive(Clone)]
pub(crate) struct CheckedKlibSamDeclaration {
    pub(crate) classifier: Arc<LibraryType>,
    pub(crate) target: IrSamTarget,
    /// The declarations a lambda converted to this interface may name as the one it overrides:
    /// the selected method and the methods of its dispatch frames.
    pub(crate) overridable: Box<[KlibDependencyCallable]>,
    /// The checker's application of the declaration, so lowering instantiates it without
    /// depending on the resolver or selecting anything again.
    pub(crate) apply: ApplySamDeclaration,
}

impl CheckedKlibSamDeclaration {
    /// The declaration's method contract at `applied`, an application of its classifier.
    pub(crate) fn applied(&self, applied: Ty) -> Option<IrSamTarget> {
        (self.apply)(&self.classifier, &self.target, applied)
    }
}

impl std::fmt::Debug for CheckedKlibSamDeclaration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckedKlibSamDeclaration")
            .field("identity", &self.classifier.klib_identity)
            .field("target", &self.target)
            .field("overridable", &self.overridable)
            .finish()
    }
}
