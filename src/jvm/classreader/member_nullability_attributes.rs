//! Nullability facts decoded from declaration and type-use member attributes.
//!
//! A declaration annotation on a method, parameter, or field and a `TYPE_USE` annotation on the
//! member type state the same fact. Kotlin flattens qualifiers on any nested-class segment onto the
//! member result, so a valid path may contain only `INNER_TYPE` steps. Array-element, wildcard, and
//! type-argument paths do not qualify the outer member type.

use super::{read_element_value, ElementValueExtract, ReadError, Reader, C};
use crate::jvm::java_nullability_qualifiers::descriptor_nullability_qualifier;
use crate::jvm::JavaNullability;

/// `target_type` values (JVMS Table 4.7.20-A) whose annotation qualifies a member's own type.
const FIELD: u8 = 0x13;
const METHOD_RETURN: u8 = 0x14;
const METHOD_FORMAL_PARAMETER: u8 = 0x16;

/// The member types a `Runtime*TypeAnnotations` attribute qualifies at their outermost level.
#[derive(Default)]
pub(super) struct TypeUseNullability {
    /// The field's type or the method's return type.
    pub(super) member: Option<JavaNullability>,
    /// Formal parameters by `formal_parameter_index`.
    pub(super) parameters: Vec<Option<JavaNullability>>,
}

pub(super) fn read_type_use_nullability(
    body: &[u8],
    cp: &[C],
    out: &mut TypeUseNullability,
) -> Result<(), ReadError> {
    let mut r = Reader { b: body, i: 0 };
    let annotation_count = r.u2()?;
    for _ in 0..annotation_count {
        let target_type = r.u1()?;
        let parameter = skip_target_info(&mut r, target_type)?;
        let path_length = r.u1()?;
        let mut is_classifier_path = true;
        for _ in 0..path_length {
            let kind = r.u1()?;
            let argument = r.u1()?;
            if kind != 1 || argument != 0 {
                // ARRAY_ELEMENT, WILDCARD_BOUND, and TYPE_ARGUMENT address a type below the
                // member's result classifier. Malformed INNER_TYPE entries likewise cannot
                // establish outer-member nullability.
                is_classifier_path = false;
            }
        }
        let descriptor = match cp.get(usize::from(r.u2()?)) {
            Some(C::Utf8(value)) => value.as_str(),
            _ => "",
        };
        let pair_count = r.u2()?;
        for _ in 0..pair_count {
            r.u2()?;
            read_element_value(&mut r, cp, ElementValueExtract::Skip)?;
        }
        let Some(nullability) = descriptor_nullability_qualifier(descriptor)
            .filter(|_| is_classifier_path)
            .map(|qualifier| qualifier.nullability)
        else {
            continue;
        };
        match (target_type, parameter) {
            (FIELD | METHOD_RETURN, _) => out.member = Some(nullability),
            (METHOD_FORMAL_PARAMETER, Some(index)) => {
                let index = usize::from(index);
                if out.parameters.len() <= index {
                    out.parameters.resize(index + 1, None);
                }
                out.parameters[index] = Some(nullability);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Skip a `target_info` union (JVMS 4.7.20.1), returning a formal parameter's index.
fn skip_target_info(r: &mut Reader, target_type: u8) -> Result<Option<u8>, ReadError> {
    match target_type {
        // type_parameter_target, formal_parameter_target
        0x00 | 0x01 => {
            r.u1()?;
        }
        METHOD_FORMAL_PARAMETER => return Ok(Some(r.u1()?)),
        // supertype_target, throws_target, catch_target, offset_target
        0x10 | 0x17 | 0x42..=0x46 => {
            r.u2()?;
        }
        // type_parameter_bound_target
        0x11 | 0x12 => {
            r.u1()?;
            r.u1()?;
        }
        // empty_target
        0x13..=0x15 => {}
        // localvar_target
        0x40 | 0x41 => {
            let table_length = r.u2()?;
            r.take(usize::from(table_length) * 6)?;
        }
        // type_argument_target
        0x47..=0x4B => {
            r.u2()?;
            r.u1()?;
        }
        _ => return Err(ReadError::Truncated),
    }
    Ok(None)
}

/// Declaration nullability annotations of each formal parameter
/// (`Runtime*ParameterAnnotations`).
pub(super) fn read_parameter_nullability(
    body: &[u8],
    cp: &[C],
    out: &mut Vec<Option<JavaNullability>>,
) -> Result<(), ReadError> {
    let utf8 = |index: u16| match cp.get(index as usize) {
        Some(C::Utf8(value)) => value.as_str(),
        _ => "",
    };
    let mut r = Reader { b: body, i: 0 };
    let parameter_count = r.u1()? as usize;
    out.resize(out.len().max(parameter_count), None);
    for slot in out.iter_mut().take(parameter_count) {
        let annotation_count = r.u2()?;
        for _ in 0..annotation_count {
            let descriptor = utf8(r.u2()?);
            let nullability =
                descriptor_nullability_qualifier(descriptor).map(|qualifier| qualifier.nullability);
            let pair_count = r.u2()?;
            for _ in 0..pair_count {
                r.u2()?;
                read_element_value(&mut r, cp, ElementValueExtract::Skip)?;
            }
            if let Some(nullable) = nullability {
                *slot = Some(nullable);
            }
        }
    }
    Ok(())
}
