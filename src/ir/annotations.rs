//! Retained annotation applications on IR declarations and the constants they encode.

use super::{IrConst, Ty, TypeName};

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
    /// A class literal `T::class` — tag `c` (its exact type descriptor). The semantic type keeps
    /// array dimensions and component identity until the backend encoding boundary.
    Class(Ty),
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
    /// Stable source declaration order. This is the declaration identity; `property` is payload and
    /// may be shared by an ordinary property and a member extension property.
    pub source_order: u32,
    pub property: String,
    pub annotations: DeclarationAnnotations,
}

#[derive(Clone, Debug, Default)]
pub struct DeclarationAnnotations {
    retained: Vec<RetainedAnnotation>,
    /// The declaration also carried non-SOURCE `@OptionalExpectation` applications with no actual
    /// on this platform. kotlinc removes them from IR with their `expect` classes, so no class
    /// file or metadata record names them, yet the declaration's metadata flags still report
    /// `hasAnnotations` (kotlinc derives that flag from FIR, before the removal).
    erased_optional_expectations: bool,
    /// A suppressed SOURCE application that remains declaration provenance even though its payload
    /// does not survive retention (suppressed immutable `@Volatile` is such a property). The JVM
    /// uses this provenance to retain the property's empty annotation marker; it does not set
    /// Kotlin metadata's `HAS_ANNOTATIONS` bit.
    suppressed_source_application: bool,
    /// How many trailing `retained` applications a compiler plugin attached after checking. They
    /// reach the class file only: kotlinc serializes metadata from FIR, before its IR plugins run.
    lowered: usize,
}

impl DeclarationAnnotations {
    /// These annotations without the applications of `annotation`.
    pub(crate) fn without(&self, annotation: TypeName) -> Self {
        let checked = self.retained.len() - self.lowered;
        let mut retained = Vec::with_capacity(self.retained.len());
        let mut lowered = 0;
        for (index, application) in self.retained.iter().enumerate() {
            if application.annotation.internal == annotation {
                continue;
            }
            lowered += usize::from(index >= checked);
            retained.push(application.clone());
        }
        Self {
            retained,
            lowered,
            ..self.clone()
        }
    }

    pub fn new(annotations: Vec<RetainedAnnotation>) -> Self {
        Self {
            retained: annotations,
            erased_optional_expectations: false,
            suppressed_source_application: false,
            lowered: 0,
        }
    }

    /// Partition checked applications for one declaration: SOURCE retention is dropped, an
    /// optional expectation leaves only the [`Self::declares_annotations`] fact, and every other
    /// application is retained.
    pub fn from_checked<'a>(
        applications: impl IntoIterator<Item = (&'a crate::types::AppliedAnnotation, AppliedAnnotation)>,
    ) -> Self {
        let mut annotations = Self::default();
        for (checked, annotation) in applications {
            if checked.retention == crate::types::AnnotationRetention::Source {
                continue;
            }
            if checked.facts.optional_expectation {
                annotations.erased_optional_expectations = true;
                continue;
            }
            annotations.retained.push(RetainedAnnotation {
                retention: checked.retention,
                annotation,
                facts: checked.facts,
            });
        }
        annotations
    }

    /// Whether the declaration carries annotation provenance: a retained application, an erased
    /// optional expectation, or a suppressed SOURCE application retained for target realization.
    /// Never derive it from [`Self::retains_none`].
    pub fn declares_annotations(&self) -> bool {
        !self.retained.is_empty()
            || self.erased_optional_expectations
            || self.suppressed_source_application
    }

    /// Whether Kotlin metadata reports `HAS_ANNOTATIONS`. Suppressed SOURCE provenance can require
    /// a target carrier without making the source declaration annotated in metadata.
    pub(crate) fn declares_metadata_annotations(&self) -> bool {
        self.retained.len() > self.lowered || self.erased_optional_expectations
    }

    pub(crate) fn with_suppressed_source_application(mut self, retained: bool) -> Self {
        self.suppressed_source_application = retained;
        self
    }

    /// Whether a suppressed SOURCE application remains part of the declaration's semantic
    /// provenance even though [`Self::retains_none`] is true.
    pub fn has_suppressed_source_application(&self) -> bool {
        self.suppressed_source_application
    }

    /// Append an application a compiler plugin attaches to the declaration after checking.
    pub(crate) fn push(&mut self, annotation: RetainedAnnotation) {
        self.retained.push(annotation);
        self.lowered += 1;
    }

    /// Append an application a compiler plugin attaches after checking that Kotlin metadata
    /// records as well, as kotlinc's metadata declaration registrar adds it: it follows the
    /// checked applications and precedes the plugin's other ones.
    pub(crate) fn push_metadata_visible(&mut self, annotation: RetainedAnnotation) {
        let checked = self.retained.len() - self.lowered;
        self.retained.insert(checked, annotation);
    }

    pub fn iter(&self) -> std::slice::Iter<'_, RetainedAnnotation> {
        self.retained.iter()
    }

    pub(super) fn iter_mut(&mut self) -> std::slice::IterMut<'_, RetainedAnnotation> {
        self.retained.iter_mut()
    }

    /// Applied annotation payloads in declaration order, independent of the physical retention
    /// partition a backend may later require.
    pub fn applications(&self) -> impl Iterator<Item = &AppliedAnnotation> {
        self.retained.iter().map(|retained| &retained.annotation)
    }

    /// The checked applications Kotlin metadata records: [`Self::applications`] without the ones a
    /// compiler plugin attached, save those it made metadata-visible.
    pub(crate) fn checked_applications(&self) -> impl Iterator<Item = &AppliedAnnotation> {
        self.retained[..self.retained.len() - self.lowered]
            .iter()
            .map(|retained| &retained.annotation)
    }

    /// Whether no application survives into the class file. This is a physical fact; whether the
    /// declaration declares annotations is [`Self::declares_annotations`].
    pub fn retains_none(&self) -> bool {
        self.retained.is_empty()
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

/// Annotations written on one property's accessors (`@A get()`, `@A set(v)`) and on its setter's
/// value parameter (`set(@A v)`).
#[derive(Clone, Debug, Default)]
pub struct AccessorAnnotations {
    pub getter: DeclarationAnnotations,
    pub setter: DeclarationAnnotations,
    pub setter_parameter: DeclarationAnnotations,
}

impl AccessorAnnotations {
    pub fn declares_annotations(&self) -> bool {
        self.getter.declares_annotations()
            || self.setter.declares_annotations()
            || self.setter_parameter.declares_annotations()
    }
}

/// A frontend-checked annotation argument, as the constant the backends encode.
impl From<&crate::types::AnnotationValue> for AnnoValue {
    fn from(value: &crate::types::AnnotationValue) -> Self {
        use crate::types::AnnotationValue;
        match value {
            AnnotationValue::Int(value) => AnnoValue::Const(IrConst::Int(*value)),
            AnnotationValue::Byte(value) => AnnoValue::Const(IrConst::Byte(*value)),
            AnnotationValue::Short(value) => AnnoValue::Const(IrConst::Short(*value)),
            AnnotationValue::Long(value) => AnnoValue::Const(IrConst::Long(*value)),
            AnnotationValue::Float(value) => AnnoValue::Const(IrConst::Float(*value)),
            AnnotationValue::Double(value) => AnnoValue::Const(IrConst::Double(*value)),
            AnnotationValue::Boolean(value) => AnnoValue::Const(IrConst::Boolean(*value)),
            AnnotationValue::Char(value) => AnnoValue::Const(IrConst::Char(*value)),
            AnnotationValue::String(value) => AnnoValue::Const(IrConst::String(value.clone())),
            AnnotationValue::Enum(internal, constant) => {
                AnnoValue::Enum(*internal, constant.clone())
            }
            AnnotationValue::Class(ty) => AnnoValue::Class(*ty),
            AnnotationValue::Annotation {
                internal, values, ..
            } => AnnoValue::Annotation(AppliedAnnotation {
                internal: *internal,
                values: values
                    .iter()
                    .map(|(name, value)| (name.clone(), AnnoValue::from(value)))
                    .collect(),
            }),
            AnnotationValue::Array(values) => {
                AnnoValue::Array(values.iter().map(AnnoValue::from).collect())
            }
        }
    }
}

/// A provider-normalized annotation application (a checked source one, or one decoded from a
/// dependency), as the applied annotation the backends encode.
impl From<&crate::types::ResolvedAnnotation> for AppliedAnnotation {
    fn from(annotation: &crate::types::ResolvedAnnotation) -> Self {
        Self {
            internal: annotation.annotation,
            values: annotation
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), AnnoValue::from(value)))
                .collect(),
        }
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
