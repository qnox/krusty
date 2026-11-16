//! The names Kotlin's IR gives a local classifier that a serialized body declares.
//!
//! A local classifier lowered from source records its naming provenance in
//! [`super::IrLocalClassNameProvenance`]. One lowered from a serialized KLIB body has no source to
//! record: its library already chose its name. Its declaration site is the container of the body
//! that declares it and its own serialized name, which a backend renders the way Kotlin's IR
//! renders `kotlinFqName`.

use crate::types::TypeName;

/// The declaration container and own name of a local classifier from a serialized body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSiteClassName {
    /// The declaration the body declaring the classifier belongs to. An executable scope
    /// contributes no segment to the name.
    pub container: IrSiteClassContainer,
    /// The classifier's own serialized name: an anonymous object's is Kotlin's
    /// `<no name provided>`.
    pub name: Box<str>,
}

/// The declaration that names a serialized body's local classifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSiteClassContainer {
    /// A top-level declaration of this package.
    Package(TypeName),
    /// A member of this top-level or nested classifier.
    Classifier(TypeName),
}
