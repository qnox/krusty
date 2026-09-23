//! Carrier-independent wire shape of Kotlin metadata `Type` messages.
//!
//! JVM `@Metadata`, `.kotlin_builtins`, and KLIB fragments share this protobuf contract. Numeric
//! string, classifier, and type-table ids remain unresolved here; the owning semantic adapter binds
//! them in its own stable name domain.

use super::Pb;

pub(crate) struct TypeAnnotation {
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
    pub(crate) fn class_id(&self) -> Option<u64> {
        self.class_id
    }

    pub(crate) fn int_arguments(&self) -> impl Iterator<Item = (u64, i64)> + '_ {
        self.arguments.iter().filter_map(|argument| {
            let AnnotationValue::Int(value) = argument.value else {
                return None;
            };
            Some((argument.name_id?, value))
        })
    }
}

pub(crate) struct ParsedTypeNode<'a> {
    pub(crate) class_id: Option<u64>,
    pub(crate) type_parameter_id: Option<u64>,
    pub(crate) type_parameter_name_id: Option<u64>,
    pub(crate) type_alias_id: Option<u64>,
    pub(crate) nullable: bool,
    pub(crate) definitely_non_null: bool,
    pub(crate) suspend: bool,
    pub(crate) flexible_upper_bound: Option<&'a [u8]>,
    pub(crate) flexible_upper_bound_id: Option<u64>,
    pub(crate) arguments: Vec<ParsedTypeArgument<'a>>,
    pub(crate) annotations: Vec<TypeAnnotation>,
}

#[derive(Clone, Copy)]
pub(crate) enum ParsedProjection {
    In,
    Out,
    Invariant,
}

/// A type argument is either an inline `Type`, an id into the carrier's `TypeTable`, or a star
/// projection with no type.
pub(crate) enum ParsedTypeArgument<'a> {
    Inline(&'a [u8], ParsedProjection),
    Table(u64, ParsedProjection),
    Star,
}

pub(crate) fn parse_type_node(body: &[u8]) -> Option<ParsedTypeNode<'_>> {
    let mut protobuf = Pb::new(body);
    let mut node = ParsedTypeNode {
        class_id: None,
        type_parameter_id: None,
        type_parameter_name_id: None,
        type_alias_id: None,
        nullable: false,
        definitely_non_null: false,
        suspend: false,
        flexible_upper_bound: None,
        flexible_upper_bound_id: None,
        arguments: Vec::new(),
        annotations: Vec::new(),
    };
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                let flags = protobuf.varint()?;
                node.suspend = flags & 0x1 != 0;
                node.definitely_non_null = flags & 0x2 != 0;
            }
            (3, 0) => node.nullable = protobuf.varint()? != 0,
            (5, 2) => {
                let length = protobuf.varint()? as usize;
                node.flexible_upper_bound = Some(protobuf.bytes(length)?);
            }
            (6, 0) => node.class_id = Some(protobuf.varint()?),
            (7, 0) => node.type_parameter_id = Some(protobuf.varint()?),
            (8, 0) => node.flexible_upper_bound_id = Some(protobuf.varint()?),
            (9, 0) => node.type_parameter_name_id = Some(protobuf.varint()?),
            (12, 0) => node.type_alias_id = Some(protobuf.varint()?),
            (2, 2) => {
                let length = protobuf.varint()? as usize;
                let mut argument = Pb::new(protobuf.bytes(length)?);
                let mut projection = ParsedProjection::Invariant;
                let mut star = false;
                let mut inline = None;
                let mut table = None;
                while !argument.at_end() {
                    let tag = argument.varint()?;
                    match (tag >> 3, tag & 7) {
                        (1, 0) => {
                            projection = match argument.varint()? {
                                0 => ParsedProjection::In,
                                1 => ParsedProjection::Out,
                                2 => ParsedProjection::Invariant,
                                3 => {
                                    star = true;
                                    ParsedProjection::Invariant
                                }
                                _ => return None,
                            }
                        }
                        (2, 2) => {
                            let length = argument.varint()? as usize;
                            inline = Some(argument.bytes(length)?);
                        }
                        (3, 0) => table = Some(argument.varint()?),
                        (_, wire) => argument.skip(wire)?,
                    }
                }
                if star {
                    node.arguments.push(ParsedTypeArgument::Star);
                } else {
                    node.arguments.push(match (inline, table) {
                        (Some(body), _) => ParsedTypeArgument::Inline(body, projection),
                        (None, Some(id)) => ParsedTypeArgument::Table(id, projection),
                        (None, None) => return None,
                    });
                }
            }
            // JVM metadata uses extension 100; common/KLIB metadata uses extension 170.
            (100 | 170, 2) => {
                let length = protobuf.varint()? as usize;
                node.annotations
                    .push(parse_type_annotation(protobuf.bytes(length)?)?);
            }
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    Some(node)
}

fn parse_type_annotation(body: &[u8]) -> Option<TypeAnnotation> {
    let mut protobuf = Pb::new(body);
    let mut class_id = None;
    let mut arguments = Vec::new();
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => class_id = protobuf.varint(),
            (2, 2) => {
                let length = protobuf.varint()? as usize;
                arguments.push(parse_annotation_argument(protobuf.bytes(length)?)?);
            }
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    Some(TypeAnnotation {
        class_id,
        arguments,
    })
}

fn parse_annotation_argument(body: &[u8]) -> Option<AnnotationArgument> {
    let mut protobuf = Pb::new(body);
    let mut name_id = None;
    let mut value = AnnotationValue::Other;
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => name_id = protobuf.varint(),
            (2, 2) => {
                let length = protobuf.varint()? as usize;
                value = parse_annotation_value(protobuf.bytes(length)?)?;
            }
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    Some(AnnotationArgument { name_id, value })
}

fn parse_annotation_value(body: &[u8]) -> Option<AnnotationValue> {
    let mut protobuf = Pb::new(body);
    let mut kind = None;
    let mut integer = None;
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => kind = protobuf.varint(),
            (2, 0) => integer = protobuf.varint(),
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    // Annotation.Argument.Value.Type.INT = 3. Integer payloads use protobuf zigzag encoding.
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
