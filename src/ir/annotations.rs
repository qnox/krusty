//! Retained annotation applications on IR declarations and the constants they encode.

use super::{IrConst, TypeName};

/// Backend-agnostic retention fact resolved by the frontend.
pub type AnnoRetention = crate::types::AnnotationRetention;

/// A resolved JVM annotation value (`element_value`, JVMS §4.7.16.1) — an annotation argument folded to
/// the constant the class file encodes.
#[derive(Clone, Debug)]
pub enum AnnoValue {
    /// A primitive/`String` constant (encoded by tag `B`/`C`/`D`/`F`/`I`/`J`/`S`/`Z`/`s`).
    Const(IrConst),
    /// An enum constant `(enum_type_internal, const_name)` — tag `e`.
    Enum(TypeName, String),
    /// A class literal `T::class` `(type_internal)` — tag `c` (its type descriptor).
    Class(TypeName),
    /// A nested annotation instance `A(...)` — tag `@`.
    Annotation(AppliedAnnotation),
    /// An array `[…]` — tag `[`.
    Array(Vec<AnnoValue>),
}

/// User annotations on one field. Retention remains a semantic fact on each annotation; the backend
/// chooses its physical representation.
#[derive(Clone, Debug)]
pub struct FieldAnnotations {
    pub field: String,
    pub annotations: DeclarationAnnotations,
}

/// One retained annotation application on a declaration. The application payload stays independent
/// of retention so nested annotation values can reuse [`AppliedAnnotation`] without inventing a
/// declaration-retention fact for the nested value.
#[derive(Clone, Debug)]
pub struct RetainedAnnotation {
    pub retention: AnnoRetention,
    pub annotation: AppliedAnnotation,
    pub facts: crate::types::AnnotationSemanticFacts,
}

/// Backend-agnostic annotations on any declaration kind. A HIDDEN-deprecated declaration is
/// identified from these records rather than a separate flag: the annotation IS the fact.
/// User annotations that landed on one PROPERTY. Retention remains semantic until a backend maps
/// the declaration onto its physical representation (a JVM marker method, for example).
#[derive(Clone, Debug)]
pub struct PropertyAnnotations {
    pub property: String,
    pub annotations: DeclarationAnnotations,
}

#[derive(Clone, Debug, Default)]
pub struct DeclarationAnnotations(Vec<RetainedAnnotation>);

impl DeclarationAnnotations {
    pub fn new(annotations: Vec<RetainedAnnotation>) -> Self {
        Self(annotations)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, RetainedAnnotation> {
        self.0.iter()
    }

    pub(super) fn iter_mut(&mut self) -> std::slice::IterMut<'_, RetainedAnnotation> {
        self.0.iter_mut()
    }

    /// Applied annotation payloads in declaration order, independent of the physical retention
    /// partition a backend may later require.
    pub fn applications(&self) -> impl Iterator<Item = &AppliedAnnotation> {
        self.0.iter().map(|retained| &retained.annotation)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether these annotations include `@kotlin.Deprecated` at any level. kotlinc additionally
    /// stamps the classic `Deprecated` class-file attribute on such a declaration, beside the
    /// annotation itself.
    pub fn deprecated(&self) -> bool {
        self.iter()
            .any(|retained| retained.annotation.internal.matches("kotlin/Deprecated"))
    }

    /// Whether these annotations include `@kotlin.Deprecated(level = DeprecationLevel.HIDDEN)`.
    /// kotlinc removes such a declaration from resolution and emits its realization
    /// `ACC_SYNTHETIC`; both facts follow from this one annotation.
    pub fn deprecated_hidden(&self) -> bool {
        self.iter().any(|retained| retained.facts.deprecated_hidden)
    }
}

/// An applied annotation (`@Anno(...)`) to encode into a `RuntimeVisibleAnnotations` attribute.
#[derive(Clone, Debug)]
pub struct AppliedAnnotation {
    /// The annotation type's internal name (`Anno`).
    pub internal: TypeName,
    /// `element_value_pairs`: `(element_name, value)` in declaration order.
    pub values: Vec<(String, AnnoValue)>,
}
