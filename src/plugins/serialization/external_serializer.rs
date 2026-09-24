//! How generated code reaches the serializer of a `@Serializable` classifier this file does not
//! declare: one from another file of the module or from a dependency.
//!
//! The classifier's own compilation decided what it generated, by its kind: an object has no
//! serializer class at all (kotlinc builds an `ObjectSerializer` over its `INSTANCE` wherever it is
//! needed), an enum, a sealed or abstract class, an interface and a generic class publish their
//! serializer through the companion's generated `serializer(…)`, and only a plain non-generic class
//! has a `$serializer` object to read. kotlinc reaches each one exactly that way.

use crate::kt_string::KtString;
use crate::types::{
    type_name, AnnotationValue, ClassifierDeclarationFacts, ClassifierDeclarationKind,
    ResolvedAnnotation, TypeName,
};

use super::annotations::SERIAL_NAME_FQ;

/// The serializer of a `@Serializable` classifier declared outside this file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalSerializer {
    /// A provider-confirmed serializer object: one `@Serializable(with = …)` names, or an exact
    /// generated serializer classifier published by the declaring compilation.
    Singleton(TypeName),
    /// An object's `ObjectSerializer(<serial name>, INSTANCE, …)`, constructed where it is used.
    Object {
        serial_name: KtString,
        /// At least one retained object annotation is itself marked `@SerialInfo`. Until its typed
        /// values can be materialized as runtime annotation instances, emission must stay residual.
        serial_info_unsupported: bool,
    },
    /// The companion's generated `serializer(…)`, taking one `KSerializer` per type parameter.
    Companion {
        field: Box<str>,
        companion: TypeName,
        type_parameters: usize,
    },
}

/// The serializer kotlinc's plugin generated for `classifier`, a `@Serializable` declaration with no
/// `with =` serializer of its own. `None` when the facts cannot name it: the caller leaves the
/// element underivable, which is reported, rather than guessing a class that may not exist.
pub(crate) fn generated_external_serializer(
    _classifier: TypeName,
    declaration: &ClassifierDeclarationFacts,
    annotations: &[ResolvedAnnotation],
) -> Option<ExternalSerializer> {
    match declaration.kind {
        ClassifierDeclarationKind::Annotation => None,
        ClassifierDeclarationKind::Object => Some(ExternalSerializer::Object {
            serial_name: class_serial_name(annotations)?
                .or_else(|| declaration.qualified_name.as_deref().map(KtString::from))?,
            serial_info_unsupported: false,
        }),
        ClassifierDeclarationKind::Enum | ClassifierDeclarationKind::Interface => {
            companion_serializer(declaration)
        }
        ClassifierDeclarationKind::Class
            if declaration.is_abstract || declaration.own_type_parameter_count > 0 =>
        {
            companion_serializer(declaration)
        }
        // The provider must publish the generated classifier before a caller may read its
        // singleton. Naming the plugin ABI is not evidence that this declaration generated it.
        ClassifierDeclarationKind::Class => None,
    }
}

/// The companion that carries the generated `serializer(…)`. A dependency's metadata records it,
/// generated or declared. Absence is an unsupported external shape, never permission to invent the
/// conventional name.
fn companion_serializer(declaration: &ClassifierDeclarationFacts) -> Option<ExternalSerializer> {
    let (field, companion) = declaration.companion.as_ref()?;
    Some(ExternalSerializer::Companion {
        field: field.clone(),
        companion: *companion,
        type_parameters: declaration.own_type_parameter_count,
    })
}

/// A class-level `@SerialName("…")` among the classifier's resolved annotations: `Some(None)` when
/// there is none, `None` when there is one whose value the provider did not carry, which leaves the
/// name unknown rather than the qualified one.
fn class_serial_name(annotations: &[ResolvedAnnotation]) -> Option<Option<KtString>> {
    let serial_name = type_name(SERIAL_NAME_FQ);
    let Some(application) = annotations
        .iter()
        .find(|annotation| annotation.annotation == serial_name)
    else {
        return Some(None);
    };
    match application.arguments.as_slice() {
        [(_, AnnotationValue::String(value))] => Some(Some(value.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(kind: ClassifierDeclarationKind) -> ClassifierDeclarationFacts {
        ClassifierDeclarationFacts {
            kind,
            is_abstract: false,
            own_type_parameter_count: 0,
            companion: Some((Box::from("Companion"), type_name("dep/Kind$Companion"))),
            qualified_name: Some(Box::from("dep.Kind")),
            source: false,
        }
    }

    #[test]
    fn each_kind_reaches_the_serializer_its_compilation_generated() {
        let classifier = type_name("dep/Kind");
        let companion = ExternalSerializer::Companion {
            field: Box::from("Companion"),
            companion: type_name("dep/Kind$Companion"),
            type_parameters: 0,
        };
        assert_eq!(
            generated_external_serializer(classifier, &facts(ClassifierDeclarationKind::Enum), &[]),
            Some(companion.clone())
        );
        assert_eq!(
            generated_external_serializer(
                classifier,
                &ClassifierDeclarationFacts {
                    is_abstract: true,
                    ..facts(ClassifierDeclarationKind::Class)
                },
                &[]
            ),
            Some(companion)
        );
        assert_eq!(
            generated_external_serializer(
                classifier,
                &ClassifierDeclarationFacts {
                    own_type_parameter_count: 2,
                    ..facts(ClassifierDeclarationKind::Class)
                },
                &[]
            ),
            Some(ExternalSerializer::Companion {
                field: Box::from("Companion"),
                companion: type_name("dep/Kind$Companion"),
                type_parameters: 2,
            })
        );
        assert_eq!(
            generated_external_serializer(
                classifier,
                &facts(ClassifierDeclarationKind::Class),
                &[]
            ),
            None
        );
        assert_eq!(
            generated_external_serializer(
                classifier,
                &facts(ClassifierDeclarationKind::Object),
                &[]
            ),
            Some(ExternalSerializer::Object {
                serial_name: KtString::from("dep.Kind"),
                serial_info_unsupported: false,
            })
        );
    }

    #[test]
    fn an_object_is_named_by_its_serial_name() {
        let named = ResolvedAnnotation {
            annotation: type_name(SERIAL_NAME_FQ),
            arguments: vec![(
                "value".to_owned(),
                AnnotationValue::String(KtString::from("custom")),
            )],
        };
        assert_eq!(
            generated_external_serializer(
                type_name("dep/Kind"),
                &facts(ClassifierDeclarationKind::Object),
                &[named]
            ),
            Some(ExternalSerializer::Object {
                serial_name: KtString::from("custom"),
                serial_info_unsupported: false,
            })
        );
    }

    /// A dependency whose metadata records no companion has no generated `serializer(…)` to call.
    #[test]
    fn a_dependency_without_a_recorded_companion_is_not_guessed() {
        let declaration = ClassifierDeclarationFacts {
            companion: None,
            ..facts(ClassifierDeclarationKind::Enum)
        };
        assert_eq!(
            generated_external_serializer(type_name("dep/Kind"), &declaration, &[]),
            None
        );
        let source = ClassifierDeclarationFacts {
            source: true,
            ..declaration
        };
        assert_eq!(
            generated_external_serializer(type_name("dep/Kind"), &source, &[]),
            None
        );
    }

    /// An object without a qualified name or a readable `@SerialName` has no serial name to build
    /// with.
    #[test]
    fn an_unnamed_object_is_not_guessed() {
        let declaration = ClassifierDeclarationFacts {
            qualified_name: None,
            ..facts(ClassifierDeclarationKind::Object)
        };
        assert_eq!(
            generated_external_serializer(type_name("dep/Kind"), &declaration, &[]),
            None
        );
        let unreadable = ResolvedAnnotation {
            annotation: type_name(SERIAL_NAME_FQ),
            arguments: Vec::new(),
        };
        assert_eq!(
            generated_external_serializer(
                type_name("dep/Kind"),
                &facts(ClassifierDeclarationKind::Object),
                &[unreadable]
            ),
            None
        );
    }
}
