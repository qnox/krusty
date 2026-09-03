//! Kotlin metadata annotations attached to a `Type` protobuf.
//!
//! Type-use annotations carry semantic function-shape facts such as
//! `ExtensionFunctionType` and `ContextFunctionTypeParams`.  Keep their wire
//! arguments here instead of reducing them to class ids in the main metadata
//! decoder.

use super::Pb;

pub(super) struct TypeAnnotation {
    class_id: Option<u64>,
    arguments: Vec<AnnotationArgument>,
}

struct AnnotationArgument {
    name_id: Option<u64>,
    value: AnnotationValue,
}

enum AnnotationValue {
    Int(i64),
    Other,
}

impl TypeAnnotation {
    pub(super) fn class_id(&self) -> Option<u64> {
        self.class_id
    }

    pub(super) fn int_arguments(&self) -> impl Iterator<Item = (u64, i64)> + '_ {
        self.arguments.iter().filter_map(|argument| {
            let AnnotationValue::Int(value) = argument.value else {
                return None;
            };
            Some((argument.name_id?, value))
        })
    }
}

pub(super) fn parse(body: &[u8]) -> Option<TypeAnnotation> {
    let mut pb = Pb { b: body, i: 0 };
    let mut class_id = None;
    let mut arguments = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => class_id = pb.varint(),
            (2, 2) => {
                let len = pb.varint()? as usize;
                arguments.push(parse_argument(pb.bytes(len)?)?);
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some(TypeAnnotation {
        class_id,
        arguments,
    })
}

fn parse_argument(body: &[u8]) -> Option<AnnotationArgument> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name_id = None;
    let mut value = AnnotationValue::Other;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => name_id = pb.varint(),
            (2, 2) => {
                let len = pb.varint()? as usize;
                value = parse_value(pb.bytes(len)?)?;
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some(AnnotationArgument { name_id, value })
}

fn parse_value(body: &[u8]) -> Option<AnnotationValue> {
    let mut pb = Pb { b: body, i: 0 };
    let mut kind = None;
    let mut integer = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => kind = pb.varint(),
            (2, 0) => integer = pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    // Annotation.Argument.Value.Type.INT = 3. Integer payloads use protobuf
    // zigzag encoding (`sint64`), including the context-parameter count.
    Some(if kind == Some(3) {
        integer
            .map(|value| AnnotationValue::Int(unzigzag_i64(value)))
            .unwrap_or(AnnotationValue::Other)
    } else {
        AnnotationValue::Other
    })
}

fn unzigzag_i64(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}
