//! A library classifier's declaration as its source language declared it, for a consumer that
//! shows declarations rather than resolving against them.
//!
//! [`super::LibraryType`] is the classifier resolution works with: a platform class and its Kotlin
//! face merged, typealiases expanded, and the facts a class file adds for the compiler (`Metadata`,
//! JVM meta-annotations) kept. A declaration header is the declaration before any of that: one
//! language's view, each supertype as written, and only the annotations that declaration states.

use crate::metadata::semantic::KotlinModality;
use crate::types::{ResolvedAnnotation, Ty, TypeName, TypeVariance};

use super::{ClassifierAccess, TypeKind};

/// The language a library classifier was declared in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationLanguage {
    Kotlin,
    Java,
}

/// A library classifier's own declaration, in Kotlin terms.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclarationHeader {
    pub language: DeclarationLanguage,
    pub kind: TypeKind,
    pub access: ClassifierAccess,
    pub modality: KotlinModality,
    pub is_data: bool,
    /// A Kotlin `fun interface`. Whether a Java interface converts a lambda is a resolution question.
    pub is_fun: bool,
    pub is_inner: bool,
    pub is_value: bool,
    pub type_parameters: Vec<HeaderTypeParameter>,
    /// The direct supertypes in the order the declaration lists them. A Java class's are in
    /// Kotlin terms: `Object` is left out, a mapped class becomes its Kotlin classifier, and a
    /// mapped collection interface becomes its mutable Kotlin form after every other supertype.
    pub supertypes: Vec<HeaderSupertype>,
    /// The annotations the declaration states, in class-file order. A Java annotation class's
    /// meta-annotations are their Kotlin counterparts.
    pub annotations: Vec<ResolvedAnnotation>,
}

impl DeclarationHeader {
    pub fn is_sealed(&self) -> bool {
        self.modality == KotlinModality::Sealed
    }

    /// Abstract or sealed.
    pub fn is_abstract(&self) -> bool {
        self.modality.is_abstract()
    }

    pub fn is_final(&self) -> bool {
        self.modality == KotlinModality::Final
    }
}

/// A type parameter of a declaration header. The bounds are the declared ones; an unbounded Kotlin
/// parameter has none, and a Java one has `Object` mapped to `kotlin.Any`.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderTypeParameter {
    pub name: String,
    pub variance: TypeVariance,
    pub reified: bool,
    pub bounds: Vec<Ty>,
}

/// A supertype of a declaration header, and the typealias the declaration named it with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderSupertype {
    pub ty: Ty,
    pub alias: Option<TypeName>,
}
