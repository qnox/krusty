//! Kotlin metadata `Type.annotation` applications, bound into the common annotation contract.
//!
//! The wire decoder leaves every string and classifier id unresolved; this adapter binds them
//! through the JVM string table so a dependency's type-use annotation is the same
//! [`ResolvedAnnotation`] a checked source application is.

use super::super::string_table::{resolve_class_name, resolve_string, Rec};
use crate::metadata::decode::{AnnotationArgumentValue, TypeAnnotation};
use crate::types::{type_name, wk, AnnotationValue, ResolvedAnnotation, Ty};

/// Bind one decoded application. `None` when an id does not resolve or the value has a shape the
/// common contract cannot carry (an unsigned argument); the caller treats that as undecodable
/// metadata rather than dropping the application.
pub(super) fn bind_type_annotation(
    annotation: &TypeAnnotation,
    records: &[Rec],
    d2: &[String],
) -> Option<ResolvedAnnotation> {
    let class = resolve_class_name(records, d2, annotation.class_id()? as usize)?;
    let arguments = annotation
        .arguments()
        .iter()
        .map(|argument| {
            let name = resolve_string(records, d2, argument.name_id? as usize)?;
            Some((name, bind_value(&argument.value, records, d2)?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ResolvedAnnotation {
        annotation: type_name(&class),
        arguments,
        facts: crate::types::AnnotationSemanticFacts::default(),
    })
}

/// Bind every application recorded on one `Type`, in order.
pub(super) fn bind_type_annotations(
    annotations: &[TypeAnnotation],
    records: &[Rec],
    d2: &[String],
) -> Option<Vec<ResolvedAnnotation>> {
    annotations
        .iter()
        .map(|annotation| bind_type_annotation(annotation, records, d2))
        .collect()
}

fn bind_value(
    value: &AnnotationArgumentValue,
    records: &[Rec],
    d2: &[String],
) -> Option<AnnotationValue> {
    // `Value.flags` bit 0 is IS_UNSIGNED, which the common contract has no slot for.
    if value.flags != 0 {
        return None;
    }
    let integer = || value.integer;
    Some(match value.kind {
        0 => AnnotationValue::Byte(i8::try_from(integer()?).ok()?),
        1 => AnnotationValue::Char(u16::try_from(integer()?).ok()?),
        2 => AnnotationValue::Short(i16::try_from(integer()?).ok()?),
        3 => AnnotationValue::Int(i32::try_from(integer()?).ok()?),
        4 => AnnotationValue::Long(integer()?),
        5 => AnnotationValue::Float(value.float?),
        6 => AnnotationValue::Double(value.double?),
        7 => AnnotationValue::Boolean(integer()? != 0),
        8 => AnnotationValue::string(&resolve_string(records, d2, value.string_id? as usize)?),
        9 => {
            let class = resolve_class_name(records, d2, value.class_id? as usize)?;
            let mut represented = Ty::obj_name(type_name(&class));
            for _ in 0..value.array_dimensions {
                represented = Ty::obj_args_name(wk::array(), &[represented]);
            }
            AnnotationValue::Class(represented)
        }
        10 => AnnotationValue::Enum(
            type_name(&resolve_class_name(records, d2, value.class_id? as usize)?),
            resolve_string(records, d2, value.enum_value_id? as usize)?,
        ),
        11 => {
            let nested = bind_type_annotation(value.annotation.as_deref()?, records, d2)?;
            AnnotationValue::Annotation {
                internal: nested.annotation,
                values: nested.arguments,
            }
        }
        12 => AnnotationValue::Array(
            value
                .elements
                .iter()
                .map(|element| bind_value(element, records, d2))
                .collect::<Option<Vec<_>>>()?,
        ),
        _ => return None,
    })
}
