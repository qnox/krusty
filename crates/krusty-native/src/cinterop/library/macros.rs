//! A header's object-like macros as Kotlin/Native binds them.
//!
//! An integer, floating or string constant is a `const val` of the expression's type (a cast's
//! typedef kept). Any other well-typed expression is a property whose getter cannot be called,
//! since direct calls have no C function to evaluate it with; its type is the expression's, an
//! array or function decayed to a pointer. A `_Bool`, enum-class or pointer constant, and an
//! expression of a struct, `void` or unsupported type, are not bound. The constants come first,
//! then the other expressions, each in the order the macros were defined.

use super::super::model::{FloatKind, MacroConstant, MacroValue, Type};
use super::enums::{constant, constant_property};
use super::kotlin_type::{string, Primitive};
use super::mirror::{Info, Mapper, Mirror};
use super::stub::{flags, Annotation, Property, Value};

/// The macros' properties, constants first.
pub(super) fn macro_properties(mapper: &Mapper<'_>, macros: &[MacroConstant]) -> Vec<Property> {
    let mut constants = Vec::new();
    let mut expressions = Vec::new();
    for constant_macro in macros {
        match &constant_macro.value {
            MacroValue::Expression(ty) => {
                if let Some(property) = expression_property(mapper, &constant_macro.name, *ty) {
                    expressions.push(property);
                }
            }
            value => {
                if let Some(property) = constant_value(mapper, &constant_macro.name, value) {
                    constants.push(property);
                }
            }
        }
    }
    constants.extend(expressions);
    constants
}

fn constant_value(mapper: &Mapper<'_>, name: &str, value: &MacroValue) -> Option<Property> {
    let (ty, value) = match value {
        MacroValue::String(text) => (string(), Value::String(text.to_string())),
        MacroValue::Integer { value, ty } => {
            let mirror = mapper.mirror(*ty).ok()?;
            let Mirror::ByValue {
                info: Info::Primitive(primitive),
                ..
            } = &mirror
            else {
                return None;
            };
            if matches!(
                primitive,
                Primitive::Boolean | Primitive::Float | Primitive::Double | Primitive::Vector128
            ) {
                return None;
            }
            (mirror.argument(), constant(*primitive, *value))
        }
        MacroValue::Floating { value, ty } => {
            let mirror = mapper.mirror(*ty).ok()?;
            let constant = match &mirror {
                Mirror::ByValue {
                    info: Info::Primitive(Primitive::Float),
                    ..
                } => Value::Float(*value as f32),
                Mirror::ByValue {
                    info: Info::Primitive(Primitive::Double),
                    ..
                } => Value::Double(*value),
                _ => return None,
            };
            (mirror.argument(), constant)
        }
        MacroValue::Expression(_) => return None,
    };
    Some(constant_property(name, ty, value))
}

fn expression_property(
    mapper: &Mapper<'_>,
    name: &str,
    ty: super::super::model::TypeId,
) -> Option<Property> {
    let declarations = mapper.declarations;
    let mirror = match declarations.ty(declarations.canonical(ty)) {
        Type::Array { element, .. } => mapper.pointer_to(*element).ok()?,
        Type::Function(_) => mapper.pointer_to(ty).ok()?,
        Type::Float(FloatKind::LongDouble) | Type::Void => return None,
        _ => mapper.mirror(ty).ok()?,
    };
    let Mirror::ByValue { .. } = mirror else {
        return None;
    };
    Some(Property {
        name: name.to_string(),
        flags: flags::GLOBAL_VAL,
        ty: mirror.argument(),
        getter_flags: flags::ANNOTATED_ACCESSOR,
        setter_flags: None,
        annotations: vec![Annotation::experimental()],
        getter_annotations: vec![Annotation::direct(&format!(
            "{name} unsupported: https://youtrack.jetbrains.com/issue/KT-82031"
        ))],
        setter_annotations: Vec::new(),
        constant: None,
    })
}
