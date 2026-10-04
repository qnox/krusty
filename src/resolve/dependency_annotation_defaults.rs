//! Declaration defaults of a dependency annotation constructed as a value (`Ann()`).
//!
//! The provider records the default each element declares (a JVM `AnnotationDefault`, for
//! example). Each value is checked against the element's declared type and closed into the same
//! file-independent [`DefaultValue`] a module annotation's defaults use, so checked FIR carries one
//! shape whatever declared the annotation.

use super::ResolvedAnnotationConstruction;
use crate::libraries::{DefaultValue, LibraryMember, LibraryType};
use crate::symbol_source::SymbolSource;
use crate::types::{AnnotationValue, Ty};

/// The construction shape of the dependency annotation `classifier` through its selected
/// constructor `member`. A default the constructor itself publishes wins; every other element takes
/// the default its declaration records. `None` when the provider publishes no complete shape.
pub(super) fn dependency_construction(
    libraries: &dyn SymbolSource,
    classifier: &LibraryType,
    member: &LibraryMember,
) -> Option<ResolvedAnnotationConstruction> {
    let parameters = classifier.annotation_application()?.parameters;
    if parameters.names.len() != parameters.types.len()
        || parameters.names.len() != member.params.len()
    {
        return None;
    }
    let members = parameters
        .names
        .into_iter()
        .zip(parameters.types)
        .collect::<Vec<_>>();
    let mut defaults = member.default_values.clone();
    defaults.resize(members.len(), None);
    for (default, declared) in defaults.iter_mut().zip(dependency_annotation_defaults(
        libraries, classifier, &members,
    )) {
        if default.is_none() {
            *default = declared;
        }
    }
    Some(ResolvedAnnotationConstruction { members, defaults })
}

/// The default of each of `members`, parallel to it. An element without an evaluated default of
/// its declared type has none.
fn dependency_annotation_defaults(
    libraries: &dyn SymbolSource,
    classifier: &LibraryType,
    members: &[(String, Ty)],
) -> Vec<Option<DefaultValue>> {
    members
        .iter()
        .map(|(name, ty)| {
            classifier
                .annotation_element_default(name)
                .and_then(|default| default.value())
                .and_then(|value| element_default(libraries, value, *ty))
        })
        .collect()
}

/// Check a provider's element value against the element's declared type. A value of another
/// shape is no default this element can take.
fn element_default(
    libraries: &dyn SymbolSource,
    value: &AnnotationValue,
    declared: Ty,
) -> Option<DefaultValue> {
    let declared = declared.non_null();
    if let Some(element) = declared.array_read_elem() {
        let AnnotationValue::Array(values) = value else {
            return None;
        };
        return Some(DefaultValue::Array {
            array_type: declared,
            elements: values
                .iter()
                .map(|value| element_default(libraries, value, element))
                .collect::<Option<_>>()?,
        });
    }
    let classifier = declared.kotlin_class_internal();
    match value {
        AnnotationValue::Int(value) => {
            matches!(declared, Ty::Int | Ty::UInt).then_some(DefaultValue::Int(i64::from(*value)))
        }
        AnnotationValue::Byte(value) => {
            matches!(declared, Ty::Byte | Ty::UByte).then_some(DefaultValue::Int(i64::from(*value)))
        }
        AnnotationValue::Short(value) => matches!(declared, Ty::Short | Ty::UShort)
            .then_some(DefaultValue::Int(i64::from(*value))),
        AnnotationValue::Long(value) => {
            matches!(declared, Ty::Long | Ty::ULong).then_some(DefaultValue::Long(*value))
        }
        AnnotationValue::Float(value) => {
            (declared == Ty::Float).then_some(DefaultValue::Float(*value))
        }
        AnnotationValue::Double(value) => {
            (declared == Ty::Double).then_some(DefaultValue::Double(*value))
        }
        AnnotationValue::Boolean(value) => {
            (declared == Ty::Boolean).then_some(DefaultValue::Bool(*value))
        }
        AnnotationValue::Char(value) => {
            (declared == Ty::Char).then_some(DefaultValue::Char(*value))
        }
        AnnotationValue::String(value) => {
            (declared == Ty::String).then(|| DefaultValue::Str(value.clone()))
        }
        AnnotationValue::Enum(enum_classifier, entry) => (classifier == Some(*enum_classifier))
            .then(|| DefaultValue::EnumEntry {
                classifier: *enum_classifier,
                name: entry.clone(),
            }),
        AnnotationValue::Class(target) => {
            (classifier == Some(crate::types::wk::kclass())).then(|| DefaultValue::KClass(*target))
        }
        AnnotationValue::Annotation { internal, values } => {
            if classifier != Some(*internal) {
                return None;
            }
            let nested = libraries.classifier(*internal)?;
            let parameters = nested.annotation_application()?.parameters;
            if parameters.names.len() != parameters.types.len()
                || values
                    .iter()
                    .any(|(name, _)| !parameters.names.contains(name))
            {
                return None;
            }
            let members = parameters
                .names
                .into_iter()
                .zip(parameters.types)
                .collect::<Vec<_>>();
            // A nested default lists only its explicit arguments; every other member takes that
            // annotation's own declaration default.
            let declared_defaults = dependency_annotation_defaults(libraries, &nested, &members);
            let values = members
                .iter()
                .zip(declared_defaults)
                .map(|((member, ty), declared)| {
                    match values.iter().find(|(name, _)| name == member) {
                        Some((_, value)) => element_default(libraries, value, *ty),
                        None => declared,
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            Some(DefaultValue::Annotation {
                classifier: *internal,
                members,
                values,
            })
        }
        AnnotationValue::Array(_) => None,
    }
}
