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

/// One `Annotation.Argument`: its name's string id and its value.
pub(crate) struct AnnotationArgument {
    pub(crate) name_id: Option<u64>,
    pub(crate) value: AnnotationArgumentValue,
}

/// One `Annotation.Argument.Value`, every id still unresolved. `kind` is the wire enum
/// (`BYTE 0 … ARRAY 12`); an absent kind is the protobuf default, `BYTE`.
pub(crate) struct AnnotationArgumentValue {
    pub(crate) kind: u64,
    pub(crate) integer: Option<i64>,
    pub(crate) float: Option<f32>,
    pub(crate) double: Option<f64>,
    pub(crate) string_id: Option<u64>,
    pub(crate) class_id: Option<u64>,
    pub(crate) enum_value_id: Option<u64>,
    pub(crate) annotation: Option<Box<TypeAnnotation>>,
    pub(crate) elements: Vec<AnnotationArgumentValue>,
    pub(crate) array_dimensions: u64,
    pub(crate) flags: u64,
}

impl TypeAnnotation {
    pub(crate) fn class_id(&self) -> Option<u64> {
        self.class_id
    }

    pub(crate) fn arguments(&self) -> &[AnnotationArgument] {
        &self.arguments
    }

    pub(crate) fn int_arguments(&self) -> impl Iterator<Item = (u64, i64)> + '_ {
        self.arguments.iter().filter_map(|argument| {
            if argument.value.kind != 3 {
                return None;
            }
            Some((argument.name_id?, argument.value.integer?))
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
    let mut value = None;
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => name_id = protobuf.varint(),
            (2, 2) => {
                let length = protobuf.varint()? as usize;
                value = Some(parse_annotation_value(protobuf.bytes(length)?)?);
            }
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    Some(AnnotationArgument {
        name_id,
        value: value?,
    })
}

fn parse_annotation_value(body: &[u8]) -> Option<AnnotationArgumentValue> {
    let mut protobuf = Pb::new(body);
    let mut value = AnnotationArgumentValue {
        kind: 0,
        integer: None,
        float: None,
        double: None,
        string_id: None,
        class_id: None,
        enum_value_id: None,
        annotation: None,
        elements: Vec::new(),
        array_dimensions: 0,
        flags: 0,
    };
    while !protobuf.at_end() {
        let tag = protobuf.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => value.kind = protobuf.varint()?,
            // Integer payloads use protobuf zigzag encoding.
            (2, 0) => value.integer = Some(unzigzag_i64(protobuf.varint()?)),
            (3, 5) => {
                let bytes = protobuf.bytes(4)?.try_into().ok()?;
                value.float = Some(f32::from_le_bytes(bytes));
            }
            (4, 1) => {
                let bytes = protobuf.bytes(8)?.try_into().ok()?;
                value.double = Some(f64::from_le_bytes(bytes));
            }
            (5, 0) => value.string_id = Some(protobuf.varint()?),
            (6, 0) => value.class_id = Some(protobuf.varint()?),
            (7, 0) => value.enum_value_id = Some(protobuf.varint()?),
            (8, 2) => {
                let length = protobuf.varint()? as usize;
                value.annotation = Some(Box::new(parse_type_annotation(protobuf.bytes(length)?)?));
            }
            (9, 2) => {
                let length = protobuf.varint()? as usize;
                value
                    .elements
                    .push(parse_annotation_value(protobuf.bytes(length)?)?);
            }
            (10, 0) => value.flags = protobuf.varint()?,
            (11, 0) => value.array_dimensions = protobuf.varint()?,
            (_, wire) => protobuf.skip(wire)?,
        }
    }
    Some(value)
}

fn unzigzag_i64(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}
