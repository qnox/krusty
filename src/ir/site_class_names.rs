//! The names Kotlin's IR gives a local classifier that a serialized body declares.
//!
//! A local classifier lowered from source records its naming provenance in
//! [`super::IrLocalClassNameProvenance`]. One lowered from a serialized KLIB body has no source to
//! record: its library already chose its name. Nor has an anonymous object an inline call
//! regenerates under its caller, which Kotlin's IR names as the caller's own. Their declaration
//! site is the container of the code that declares them and their own name, which a backend
//! renders the way Kotlin's IR renders `kotlinFqName`.

use crate::types::TypeName;

use super::ClassId;

/// The declaration container and own name of a local classifier with no source provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSiteClassName {
    /// The declaration the body declaring the classifier belongs to. An executable scope
    /// contributes no segment to the name.
    pub container: IrSiteClassContainer,
    /// The classifier's own serialized name: an anonymous object's is Kotlin's
    /// `<no name provided>`.
    pub name: Box<str>,
}

impl IrSiteClassName {
    /// Kotlin's own name for an anonymous object.
    pub const ANONYMOUS_OBJECT: &'static str = "<no name provided>";

    /// The site name of an anonymous object that `container`'s code declares.
    pub fn anonymous_object(container: IrSiteClassContainer) -> Self {
        Self {
            container,
            name: Self::ANONYMOUS_OBJECT.into(),
        }
    }
}

/// The declaration that names a local classifier with no source provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSiteClassContainer {
    /// A top-level declaration of this package.
    Package(TypeName),
    /// A member of this top-level or nested classifier.
    Classifier(TypeName),
    /// Code of this file's local or anonymous classifier, named as that classifier is.
    Class(ClassId),
}
