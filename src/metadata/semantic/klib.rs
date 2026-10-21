use super::*;
use crate::metadata::decode::{
    field, packed_varints, parse_type_node, parse_type_param, parse_value_parameter, require_wire,
    ClassAnnotationProtocol, Cursor, DecodedPackageFragment, ParameterDecodeError,
    ParsedTypeArgument, ParsedTypeNode, ParsedTypeParam, ParsedValueParam, ParsedVariance, QName,
};
use crate::metadata::semantic as metadata;

fn semantic_error(detail: impl Into<String>) -> PackageFragmentDecodeError {
    PackageFragmentDecodeError {
        offset: 0,
        detail: detail.into(),
    }
}

fn parameter_error(context: &str, error: ParameterDecodeError) -> PackageFragmentDecodeError {
    let detail = match error {
        ParameterDecodeError::MalformedWire => format!("malformed {context}"),
        ParameterDecodeError::MissingField(field) => {
            format!("{context} has no {field}")
        }
        ParameterDecodeError::InvalidVariance(variance) => {
            let declaration = context.strip_suffix(" type parameter").unwrap_or(context);
            format!("invalid type-parameter variance {variance} in {declaration} declaration")
        }
    };
    semantic_error(detail)
}

fn semantic_string(
    strings: &[String],
    id: u64,
    context: &str,
) -> Result<String, PackageFragmentDecodeError> {
    let index = usize::try_from(id)
        .map_err(|_| semantic_error(format!("{context} string id exceeds host range")))?;
    strings
        .get(index)
        .cloned()
        .ok_or_else(|| semantic_error(format!("{context} references absent string {id}")))
}

fn semantic_qname(
    strings: &[String],
    qnames: &[QName],
    id: u64,
    context: &str,
) -> Result<String, PackageFragmentDecodeError> {
    let id = usize::try_from(id)
        .map_err(|_| semantic_error(format!("{context} qualified-name id exceeds host range")))?;
    let name = metadata::resolve_qname(qnames, strings, id)
        .map_err(|error| semantic_error(format!("{context} {error}")))?;
    if name.is_empty() {
        return Err(semantic_error(format!(
            "{context} resolves to an empty qualified name"
        )));
    }
    Ok(name)
}

/// Decode the scalar compile-time value carried by a `const val` property record.
fn semantic_constant(
    body: &[u8],
    strings: &[String],
) -> Result<Option<crate::libraries::LibConst>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut kind = None;
    let mut integral = None;
    let mut float = None;
    let mut double = None;
    let mut string = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "constant value")?;
        match (number, wire) {
            (1, 0) => kind = Some(cursor.varint("constant value kind")?),
            (2, 0) => {
                let raw = cursor.varint("constant integral value")?;
                integral = Some(((raw >> 1) as i64) ^ -((raw & 1) as i64));
            }
            (3, 5) => float = Some(f32::from_bits(cursor.fixed32("constant float value")?)),
            (4, 1) => double = Some(f64::from_bits(cursor.fixed64("constant double value")?)),
            (5, 0) => {
                let id = cursor.varint("constant string value")?;
                string = Some(semantic_string(strings, id, "constant string value")?);
            }
            (_, wire) => cursor.skip(wire, "constant value")?,
        }
    }
    use crate::libraries::LibConst;
    Ok(match kind {
        Some(0 | 1 | 2 | 3 | 7) => integral
            .and_then(|value| i32::try_from(value).ok())
            .map(LibConst::Int),
        Some(4) => integral.map(LibConst::Long),
        Some(5) => float.map(LibConst::Float),
        Some(6) => double.map(LibConst::Double),
        Some(8) => string.map(|value| LibConst::Str(crate::kt_string::KtString::from(value))),
        _ => None,
    })
}

fn validate_annotation_value(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(), PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut kinds = 0;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "annotation value")?;
        match (number, wire) {
            (1, 0) => {
                kinds += 1;
                let kind = cursor.varint("annotation value kind")?;
                if kind > 12 {
                    return Err(semantic_error(format!(
                        "invalid annotation value kind {kind}"
                    )));
                }
            }
            (5, 0) => {
                let id = cursor.varint("annotation string value")?;
                semantic_string(strings, id, "annotation string value")?;
            }
            (6, 0) => {
                let id = cursor.varint("annotation class value")?;
                semantic_qname(strings, qnames, id, "annotation class value")?;
            }
            (7, 0) => {
                let id = cursor.varint("annotation enum value")?;
                semantic_string(strings, id, "annotation enum value")?;
            }
            (8, 2) => {
                let nested = cursor.length_delimited("nested annotation")?.0;
                validate_annotation(nested, strings, qnames)?;
            }
            (9, 2) => {
                let nested = cursor.length_delimited("annotation array element")?.0;
                validate_annotation_value(nested, strings, qnames)?;
            }
            (_, wire) => cursor.skip(wire, "annotation value")?,
        }
    }
    if kinds != 1 {
        return Err(semantic_error(format!(
            "annotation value has {kinds} kind fields"
        )));
    }
    Ok(())
}

fn annotation_identity(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<crate::types::TypeName, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut class_id = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "annotation")?;
        match (number, wire) {
            (1, 0) => {
                if class_id.is_some() {
                    return Err(semantic_error("annotation has duplicate class identity"));
                }
                class_id = Some(cursor.varint("annotation class id")?);
            }
            (2, 2) => {
                let argument = cursor.length_delimited("annotation argument")?.0;
                let mut argument = Cursor::new(argument, 0);
                let mut name = None;
                let mut value = None;
                while !argument.at_end() {
                    let (number, wire) = field(&mut argument, "annotation argument")?;
                    match (number, wire) {
                        (1, 0) => name = Some(argument.varint("annotation argument name")?),
                        (2, 2) => {
                            value = Some(argument.length_delimited("annotation argument value")?.0)
                        }
                        (_, wire) => argument.skip(wire, "annotation argument")?,
                    }
                }
                semantic_string(
                    strings,
                    name.ok_or_else(|| semantic_error("annotation argument has no name"))?,
                    "annotation argument",
                )?;
                validate_annotation_value(
                    value.ok_or_else(|| semantic_error("annotation argument has no value"))?,
                    strings,
                    qnames,
                )?;
            }
            (_, wire) => cursor.skip(wire, "annotation")?,
        }
    }
    let identity = semantic_qname(
        strings,
        qnames,
        class_id.ok_or_else(|| semantic_error("annotation has no class identity"))?,
        "annotation",
    )?;
    Ok(crate::types::type_name(&identity))
}

fn validate_annotation(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(), PackageFragmentDecodeError> {
    annotation_identity(body, strings, qnames).map(drop)
}

/// The annotations under `annotation_fields` of `body`, each with its decoded arguments.
fn annotation_applications(
    body: &[u8],
    annotation_fields: &[u64],
    strings: &[String],
    qnames: &[QName],
    context: &str,
) -> Result<Vec<metadata::AnnotationApplication>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut annotations = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, context)?;
        if annotation_fields.contains(&number) {
            require_wire(&cursor, wire, 2, context)?;
            let annotation = cursor.length_delimited("annotation")?.0;
            annotations.push(annotation_application(annotation, strings, qnames)?);
        } else {
            cursor.skip(wire, context)?;
        }
    }
    Ok(annotations)
}

fn annotation_application(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<metadata::AnnotationApplication, PackageFragmentDecodeError> {
    let identity = annotation_identity(body, strings, qnames)?;
    let mut cursor = Cursor::new(body, 0);
    let mut arguments = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "annotation")?;
        if (number, wire) != (2, 2) {
            cursor.skip(wire, "annotation")?;
            continue;
        }
        let argument = cursor.length_delimited("annotation argument")?.0;
        let mut argument = Cursor::new(argument, 0);
        let mut name = None;
        let mut value = metadata::AnnotationArgument::Other;
        while !argument.at_end() {
            let (number, wire) = field(&mut argument, "annotation argument")?;
            match (number, wire) {
                (1, 0) => name = Some(argument.varint("annotation argument name")?),
                (2, 2) => {
                    let body = argument.length_delimited("annotation argument value")?.0;
                    value = annotation_argument(body, strings, qnames)?;
                }
                (_, wire) => argument.skip(wire, "annotation argument")?,
            }
        }
        let name = semantic_string(
            strings,
            name.ok_or_else(|| semantic_error("annotation argument has no name"))?,
            "annotation argument",
        )?;
        arguments.push((name, value));
    }
    Ok(metadata::AnnotationApplication {
        identity,
        arguments,
    })
}

/// `Annotation.Argument.Value`: `type` (1) STRING = 8 names `string_value` (5), ENUM = 10 names
/// `class_id` (6) and `enum_value_id` (7), and ARRAY = 12 repeats `array_element` (9).
/// [`validate_annotation_value`] has already checked the rest of the shape.
fn annotation_argument(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<metadata::AnnotationArgument, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut kind = None;
    let mut string = None;
    let mut class = None;
    let mut entry = None;
    let mut elements = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "annotation value")?;
        match (number, wire) {
            (1, 0) => kind = Some(cursor.varint("annotation value kind")?),
            (5, 0) => {
                let id = cursor.varint("annotation string value")?;
                string = Some(semantic_string(strings, id, "annotation string value")?);
            }
            (6, 0) => {
                let id = cursor.varint("annotation class value")?;
                class = Some(semantic_qname(
                    strings,
                    qnames,
                    id,
                    "annotation class value",
                )?);
            }
            (7, 0) => {
                let id = cursor.varint("annotation enum value")?;
                entry = Some(semantic_string(strings, id, "annotation enum value")?);
            }
            (9, 2) => {
                let element = cursor.length_delimited("annotation array element")?.0;
                elements.push(annotation_argument(element, strings, qnames)?);
            }
            (_, wire) => cursor.skip(wire, "annotation value")?,
        }
    }
    Ok(match (kind, string, class, entry) {
        (Some(8), Some(value), _, _) => metadata::AnnotationArgument::String(value),
        (Some(8), None, _, _) => {
            return Err(semantic_error("string annotation value has no value"))
        }
        (Some(10), _, Some(class), Some(entry)) => metadata::AnnotationArgument::Enum {
            class: crate::types::type_name(&class),
            entry,
        },
        (Some(10), _, _, _) => {
            return Err(semantic_error(
                "enum annotation value has no class or entry",
            ))
        }
        (Some(12), _, _, _) => metadata::AnnotationArgument::Array(elements),
        _ => metadata::AnnotationArgument::Other,
    })
}

fn annotation_identities(
    body: &[u8],
    annotation_fields: &[u64],
    strings: &[String],
    qnames: &[QName],
    context: &str,
) -> Result<Vec<crate::types::TypeName>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut annotations = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, context)?;
        if annotation_fields.contains(&number) {
            require_wire(&cursor, wire, 2, context)?;
            let annotation = cursor.length_delimited("annotation")?.0;
            annotations.push(annotation_identity(annotation, strings, qnames)?);
        } else {
            cursor.skip(wire, context)?;
        }
    }
    Ok(annotations)
}

fn validate_annotation_fields(
    body: &[u8],
    annotation_fields: &[u64],
    strings: &[String],
    qnames: &[QName],
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    annotation_identities(body, annotation_fields, strings, qnames, context).map(drop)
}

fn message_bodies<'a>(
    body: &'a [u8],
    field_number: u64,
    context: &str,
) -> Result<Vec<&'a [u8]>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut bodies = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, context)?;
        if number == field_number {
            require_wire(&cursor, wire, 2, context)?;
            bodies.push(cursor.length_delimited(context)?.0);
        } else {
            cursor.skip(wire, context)?;
        }
    }
    Ok(bodies)
}

struct SemanticTypeTable<'a> {
    types: Vec<&'a [u8]>,
    first_nullable: Option<usize>,
}

fn type_table_bodies<'a>(
    carrier: &'a [u8],
    context: &str,
) -> Result<Option<SemanticTypeTable<'a>>, PackageFragmentDecodeError> {
    let tables = message_bodies(carrier, 30, context)?;
    if tables.len() > 1 {
        return Err(semantic_error(format!("duplicate type table in {context}")));
    }
    let Some(table) = tables.first() else {
        return Ok(None);
    };
    let types = message_bodies(table, 1, "type table")?;
    let mut cursor = Cursor::new(table, 0);
    let mut first_nullable = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "type table")?;
        if number == 2 {
            require_wire(&cursor, wire, 0, "type table")?;
            if first_nullable.is_some() {
                return Err(semantic_error("duplicate type-table first-nullable index"));
            }
            let raw = cursor.varint("type-table first-nullable index")?;
            let signed = raw as u32 as i32;
            if signed < -1 {
                return Err(semantic_error(format!(
                    "invalid type-table first-nullable index {signed}"
                )));
            }
            if signed >= 0 {
                first_nullable = Some(signed as usize);
            }
        } else {
            cursor.skip(wire, "type table")?;
        }
    }
    if first_nullable.is_some_and(|index| index > types.len()) {
        return Err(semantic_error(
            "type-table first-nullable index exceeds its type count",
        ));
    }
    Ok(Some(SemanticTypeTable {
        types,
        first_nullable,
    }))
}

struct SemanticTables<'a> {
    strings: &'a [String],
    qnames: &'a [QName],
    types: Vec<&'a [u8]>,
    first_nullable: Option<usize>,
}

struct DecodedTypeEdges {
    flexible_upper: Option<metadata::KotlinType>,
    outer: Option<metadata::KotlinType>,
}

impl SemanticTables<'_> {
    fn validate_type_edges(
        &self,
        body: &[u8],
        type_parameters: &TypeParameterScope,
        depth: u32,
        context: &str,
    ) -> Result<DecodedTypeEdges, PackageFragmentDecodeError> {
        let mut cursor = Cursor::new(body, 0);
        let mut flexible_inline = Vec::new();
        let mut flexible_ids = Vec::new();
        let mut outer_inline = Vec::new();
        let mut outer_ids = Vec::new();
        let mut abbreviated_inline = Vec::new();
        let mut abbreviated_ids = Vec::new();
        while !cursor.at_end() {
            let (number, wire) = field(&mut cursor, context)?;
            match (number, wire) {
                (2, 2) => {
                    let argument = cursor.length_delimited("type argument")?.0;
                    let mut argument = Cursor::new(argument, 0);
                    let mut projection = 2;
                    let mut inline = Vec::new();
                    let mut ids = Vec::new();
                    while !argument.at_end() {
                        let (number, wire) = field(&mut argument, "type argument")?;
                        match (number, wire) {
                            (1, 0) => projection = argument.varint("type projection")?,
                            (2, 2) => inline.push(argument.length_delimited("argument type")?.0),
                            (3, 0) => ids.push(argument.varint("argument type id")?),
                            (_, wire) => argument.skip(wire, "type argument")?,
                        }
                    }
                    if projection > 3 {
                        return Err(semantic_error(format!(
                            "invalid type projection {projection} in {context}"
                        )));
                    }
                    if projection == 3 {
                        if !inline.is_empty() || !ids.is_empty() {
                            return Err(semantic_error(format!(
                                "star projection carries a type in {context}"
                            )));
                        }
                    } else {
                        match (inline.as_slice(), ids.as_slice()) {
                            ([_], []) | ([], [_]) => {}
                            _ => {
                                return Err(semantic_error(format!(
                                    "type argument has duplicate or conflicting type forms in {context}"
                                )));
                            }
                        }
                    }
                }
                (5, 2) => flexible_inline.push(cursor.length_delimited("flexible upper type")?.0),
                (8, 0) => flexible_ids.push(cursor.varint("flexible upper type id")?),
                (10, 2) => outer_inline.push(cursor.length_delimited("outer type")?.0),
                (11, 0) => outer_ids.push(cursor.varint("outer type id")?),
                (13, 2) => abbreviated_inline.push(cursor.length_delimited("abbreviated type")?.0),
                (14, 0) => abbreviated_ids.push(cursor.varint("abbreviated type id")?),
                (100 | 170, 2) => {
                    let annotation = cursor.length_delimited("type annotation")?.0;
                    validate_annotation(annotation, self.strings, self.qnames)?;
                }
                (_, wire) => cursor.skip(wire, context)?,
            }
        }
        let decode_edge =
            |inline: Vec<&[u8]>,
             ids: Vec<u64>,
             label: &str|
             -> Result<Option<metadata::KotlinType>, PackageFragmentDecodeError> {
                match (inline.as_slice(), ids.as_slice()) {
                    ([], []) => Ok(None),
                    ([body], []) => self.ty(body, type_parameters, depth + 1, label).map(Some),
                    ([], [id]) => self
                        .ty_by_id(*id, type_parameters, depth + 1, label)
                        .map(Some),
                    _ => Err(semantic_error(format!(
                        "{label} has duplicate or conflicting forms in {context}"
                    ))),
                }
            };
        let flexible_upper = decode_edge(flexible_inline, flexible_ids, "flexible upper type")?;
        let outer = decode_edge(outer_inline, outer_ids, "outer type")?;
        decode_edge(abbreviated_inline, abbreviated_ids, "abbreviated type")?;
        Ok(DecodedTypeEdges {
            flexible_upper,
            outer,
        })
    }

    fn ty(
        &self,
        body: &[u8],
        type_parameters: &TypeParameterScope,
        depth: u32,
        context: &str,
    ) -> Result<metadata::KotlinType, PackageFragmentDecodeError> {
        if depth > 16 {
            return Err(semantic_error(format!(
                "cyclic or excessively nested type in {context}"
            )));
        }
        let node = parse_type_node(body)
            .ok_or_else(|| semantic_error(format!("invalid semantic type in {context}")))?;
        let edges = self.validate_type_edges(body, type_parameters, depth, context)?;
        let shape = self.function_type_shape(&node, context)?;
        let mut arguments = Vec::with_capacity(node.arguments.len());
        for argument in node.arguments {
            let argument = match argument {
                ParsedTypeArgument::Inline(body, projection) => {
                    let ty = self.ty(body, type_parameters, depth + 1, context)?;
                    metadata::project_kotlin_type(projection, ty)
                }
                ParsedTypeArgument::Table(id, projection) => {
                    let ty = self.ty_by_id(id, type_parameters, depth + 1, context)?;
                    metadata::project_kotlin_type(projection, ty)
                }
                ParsedTypeArgument::Star => metadata::KotlinType::Star,
            };
            arguments.push(argument);
        }
        let classifier_count = usize::from(node.class_id.is_some())
            + usize::from(node.type_alias_id.is_some())
            + usize::from(node.type_parameter_id.is_some())
            + usize::from(node.type_parameter_name_id.is_some());
        if classifier_count != 1 {
            return Err(semantic_error(format!(
                "{context} type has {classifier_count} classifier identities"
            )));
        }
        let nullable = node.nullable
            || edges
                .flexible_upper
                .as_ref()
                .is_some_and(metadata::KotlinType::nullable);
        let nullable = nullable && !node.definitely_non_null;
        if let Some(id) = node.class_id.or(node.type_alias_id) {
            if let Some(outer) = edges.outer {
                let metadata::KotlinType::Class {
                    args: outer_arguments,
                    ..
                } = outer
                else {
                    return Err(semantic_error(format!(
                        "outer type in {context} is not a classifier"
                    )));
                };
                arguments.extend(outer_arguments);
            }
            return Ok(metadata::KotlinType::Class {
                internal: semantic_qname(self.strings, self.qnames, id, context)?,
                args: arguments,
                nullable,
                shape,
            });
        }
        if edges.outer.is_some() {
            return Err(semantic_error(format!(
                "non-classifier type in {context} carries an outer type"
            )));
        }
        let (id, name) = if let Some(id) = node.type_parameter_id {
            let name = type_parameters.by_id.get(&id).cloned().ok_or_else(|| {
                semantic_error(format!("{context} references absent type parameter {id}"))
            })?;
            (id, name)
        } else if let Some(name_id) = node.type_parameter_name_id {
            // The name form addresses a type parameter of the immediate owner only.
            let name = semantic_string(self.strings, name_id, context)?;
            let id = type_parameters.own.get(&name).copied().ok_or_else(|| {
                semantic_error(format!(
                    "{context} names type parameter {name}, which its owner does not declare"
                ))
            })?;
            (id, name)
        } else {
            return Err(semantic_error(format!(
                "{context} type has no classifier or type parameter"
            )));
        };
        Ok(metadata::KotlinType::Param {
            name,
            id: metadata::KotlinTypeParameterId(id),
            nullable,
        })
    }

    fn function_type_shape(
        &self,
        node: &ParsedTypeNode<'_>,
        context: &str,
    ) -> Result<metadata::KotlinFunctionTypeShape, PackageFragmentDecodeError> {
        let mut shape = metadata::KotlinFunctionTypeShape {
            suspend: node.suspend,
            ..Default::default()
        };
        for annotation in &node.annotations {
            let Some(id) = annotation.class_id() else {
                continue;
            };
            match semantic_qname(self.strings, self.qnames, id, context)?.as_str() {
                "kotlin/ExtensionFunctionType" => shape.receiver = true,
                "kotlin/ContextFunctionTypeParams" => {
                    let count = annotation
                        .int_arguments()
                        .find_map(|(name, value)| {
                            (semantic_string(self.strings, name, context).ok().as_deref()
                                == Some("count"))
                            .then_some(value)
                        })
                        .ok_or_else(|| {
                            semantic_error(format!(
                                "a context function type in {context} declares no count"
                            ))
                        })?;
                    shape.context_count = usize::try_from(count).map_err(|_| {
                        semantic_error(format!(
                            "a context function type in {context} declares {count} parameters"
                        ))
                    })?;
                }
                _ => {}
            }
        }
        Ok(shape)
    }

    fn ty_by_id(
        &self,
        id: u64,
        type_parameters: &TypeParameterScope,
        depth: u32,
        context: &str,
    ) -> Result<metadata::KotlinType, PackageFragmentDecodeError> {
        let id = usize::try_from(id)
            .map_err(|_| semantic_error(format!("{context} type id exceeds host range")))?;
        let body = self
            .types
            .get(id)
            .ok_or_else(|| semantic_error(format!("{context} references absent type {id}")))?;
        let definitely_non_null = parse_type_node(body)
            .ok_or_else(|| semantic_error(format!("invalid semantic type in {context}")))?
            .definitely_non_null;
        let ty = self.ty(body, type_parameters, depth, context)?;
        if self
            .first_nullable
            .is_some_and(|first| id >= first && !definitely_non_null)
        {
            Ok(match ty {
                metadata::KotlinType::Class {
                    internal,
                    args,
                    shape,
                    ..
                } => metadata::KotlinType::Class {
                    internal,
                    args,
                    nullable: true,
                    shape,
                },
                metadata::KotlinType::Param { name, id, .. } => metadata::KotlinType::Param {
                    name,
                    id,
                    nullable: true,
                },
                metadata::KotlinType::InProjection(inner) => {
                    metadata::KotlinType::InProjection(inner)
                }
                metadata::KotlinType::OutProjection(inner) => {
                    metadata::KotlinType::OutProjection(inner)
                }
                metadata::KotlinType::Star => metadata::KotlinType::Star,
            })
        } else {
            Ok(ty)
        }
    }

    fn type_ref(
        &self,
        inline: Option<&[u8]>,
        table_id: Option<u64>,
        type_parameters: &TypeParameterScope,
        context: &str,
    ) -> Result<metadata::KotlinType, PackageFragmentDecodeError> {
        match (inline, table_id) {
            (Some(body), None) => self.ty(body, type_parameters, 0, context),
            (None, Some(id)) => self.ty_by_id(id, type_parameters, 0, context),
            (None, None) => Err(semantic_error(format!("{context} has no type"))),
            (Some(_), Some(_)) => Err(semantic_error(format!(
                "{context} has conflicting inline and table type forms"
            ))),
        }
    }

    fn type_occurrence_spelling(
        &self,
        body: &[u8],
        depth: u32,
        context: &str,
    ) -> Result<crate::spelling::Spelled, PackageFragmentDecodeError> {
        if depth > 16 {
            return Err(semantic_error(format!(
                "cyclic or excessively nested type in {context}"
            )));
        }
        let node = parse_type_node(body)
            .ok_or_else(|| semantic_error(format!("invalid semantic type in {context}")))?;
        let mut args = Vec::with_capacity(node.arguments.len());
        for argument in node.arguments {
            let spelling = match argument {
                ParsedTypeArgument::Inline(body, _) => {
                    self.type_occurrence_spelling(body, depth + 1, context)?
                }
                ParsedTypeArgument::Table(id, _) => {
                    let id = usize::try_from(id).map_err(|_| {
                        semantic_error(format!("{context} type id exceeds host range"))
                    })?;
                    let body = self.types.get(id).ok_or_else(|| {
                        semantic_error(format!("{context} references absent type {id}"))
                    })?;
                    self.type_occurrence_spelling(body, depth + 1, context)?
                }
                ParsedTypeArgument::Star => Default::default(),
            };
            args.push(spelling);
        }
        let outer = match (node.outer_type, node.outer_type_id) {
            (Some(body), None) => Some(self.type_occurrence_spelling(body, depth + 1, context)?),
            (None, Some(id)) => {
                let id = usize::try_from(id).map_err(|_| {
                    semantic_error(format!("{context} outer type id exceeds host range"))
                })?;
                let body = self.types.get(id).ok_or_else(|| {
                    semantic_error(format!("{context} references absent outer type {id}"))
                })?;
                Some(self.type_occurrence_spelling(body, depth + 1, context)?)
            }
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(semantic_error(format!(
                    "{context} has conflicting inline and table outer types"
                )))
            }
        };
        if let Some(outer) = outer {
            // KLIB semantic types flatten an inner classifier's enclosing arguments after its own
            // arguments. Keep the sidecar path parallel to that exact normalized shape.
            args.extend(outer.args);
        }
        Ok(crate::spelling::Spelled {
            definitely_non_null: node.definitely_non_null,
            use_site_definitely_non_null: node.definitely_non_null,
            args,
            ..Default::default()
        })
    }

    fn type_occurrence_spelling_ref(
        &self,
        inline: Option<&[u8]>,
        table_id: Option<u64>,
        context: &str,
    ) -> Result<crate::spelling::Spelled, PackageFragmentDecodeError> {
        match (inline, table_id) {
            (Some(body), None) => self.type_occurrence_spelling(body, 0, context),
            (None, Some(id)) => {
                let id = usize::try_from(id)
                    .map_err(|_| semantic_error(format!("{context} type id exceeds host range")))?;
                let body = self.types.get(id).ok_or_else(|| {
                    semantic_error(format!("{context} references absent type {id}"))
                })?;
                self.type_occurrence_spelling(body, 0, context)
            }
            (None, None) => Err(semantic_error(format!("{context} has no type"))),
            (Some(_), Some(_)) => Err(semantic_error(format!(
                "{context} has conflicting inline and table type forms"
            ))),
        }
    }

    fn type_parameters(
        &self,
        bodies: &[&[u8]],
        inherited: &TypeParameterScope,
        context: &str,
    ) -> Result<(Vec<metadata::KotlinTypeParameter>, TypeParameterScope), PackageFragmentDecodeError>
    {
        let mut names = TypeParameterScope {
            by_id: inherited.by_id.clone(),
            own: crate::fx_hash::HashMap::default(),
        };
        let mut parsed = Vec::with_capacity(bodies.len());
        for body in bodies {
            let mut cursor = Cursor::new(body, 0);
            let mut ids = 0;
            let mut names_seen = 0;
            let mut variances = 0;
            while !cursor.at_end() {
                let (number, wire) = field(&mut cursor, "type-parameter declaration")?;
                match (number, wire) {
                    (1, 0) => {
                        ids += 1;
                        cursor.varint("type-parameter id")?;
                    }
                    (2, 0) => {
                        names_seen += 1;
                        cursor.varint("type-parameter name")?;
                    }
                    (4, 0) => {
                        variances += 1;
                        let variance = cursor.varint("type-parameter variance")?;
                        if variance > 2 {
                            return Err(semantic_error(format!(
                                "invalid type-parameter variance {variance} in {context}"
                            )));
                        }
                    }
                    (_, wire) => cursor.skip(wire, "type-parameter declaration")?,
                }
            }
            if ids != 1 || names_seen != 1 || variances > 1 {
                return Err(semantic_error(format!(
                    "duplicate required field in type parameter in {context}"
                )));
            }
            let parameter = parse_type_param(body).map_err(|error| {
                semantic_error(format!(
                    "invalid semantic type parameter in {context}: {error:?}"
                ))
            })?;
            let name = semantic_string(self.strings, parameter.name_id, context)?;
            if names.by_id.insert(parameter.id, name.clone()).is_some() {
                return Err(semantic_error(format!(
                    "duplicate type-parameter id {} in {context}",
                    parameter.id
                )));
            }
            if names.own.insert(name, parameter.id).is_some() {
                return Err(semantic_error(format!(
                    "duplicate type-parameter name in {context}"
                )));
            }
            parsed.push(parameter);
        }
        let mut parameters = Vec::with_capacity(parsed.len());
        for parameter in parsed {
            let name = names
                .by_id
                .get(&parameter.id)
                .cloned()
                .ok_or_else(|| semantic_error("lost decoded type-parameter identity"))?;
            let mut bounds = Vec::new();
            for body in &parameter.upper_bound_bodies {
                bounds.push(self.ty(body, &names, 0, context)?);
            }
            for id in parameter.upper_bound_ids {
                bounds.push(self.ty_by_id(id, &names, 0, context)?);
            }
            let mut only_input = false;
            for annotation in parameter.annotation_bodies {
                validate_annotation(&annotation, self.strings, self.qnames)?;
                let mut cursor = Cursor::new(&annotation, 0);
                let mut class_id = None;
                while !cursor.at_end() {
                    let (number, wire) = field(&mut cursor, "type-parameter annotation")?;
                    if number == 1 {
                        require_wire(&cursor, wire, 0, "type-parameter annotation")?;
                        class_id = Some(cursor.varint("annotation class id")?);
                    } else {
                        cursor.skip(wire, "type-parameter annotation")?;
                    }
                }
                let class_id = class_id.ok_or_else(|| {
                    semantic_error("type-parameter annotation has no class identity")
                })?;
                only_input |= semantic_qname(
                    self.strings,
                    self.qnames,
                    class_id,
                    "type-parameter annotation",
                )? == "kotlin/internal/OnlyInputTypes";
            }
            parameters.push(metadata::KotlinTypeParameter {
                id: metadata::KotlinTypeParameterId(parameter.id),
                name,
                bounds,
                variance: match parameter.variance {
                    ParsedVariance::In => crate::types::TypeVariance::In,
                    ParsedVariance::Out => crate::types::TypeVariance::Out,
                    ParsedVariance::Invariant => crate::types::TypeVariance::Invariant,
                },
                only_input,
                reified: parameter.reified,
            });
        }
        Ok((parameters, names))
    }
}

/// The type parameters a declaration's types can reference: every one in scope by its metadata
/// id, and the declaration's own by name for the immediate-owner `type_parameter_name` form.
#[derive(Clone, Default)]
struct TypeParameterScope {
    by_id: crate::fx_hash::HashMap<u64, String>,
    own: crate::fx_hash::HashMap<String, u64>,
}

struct SemanticValueParameter {
    ty: metadata::KotlinType,
    name: String,
    has_default: bool,
    inline_modifier: crate::types::InlineParameterModifier,
    is_vararg: bool,
}

fn semantic_value_parameter(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &TypeParameterScope,
    context: &str,
) -> Result<SemanticValueParameter, PackageFragmentDecodeError> {
    validate_annotation_fields(body, &[7, 170], tables.strings, tables.qnames, context)?;
    for value in message_bodies(body, 8, context)? {
        validate_annotation_value(value, tables.strings, tables.qnames)?;
    }
    let parameter = parse_value_parameter(body).map_err(|error| {
        semantic_error(format!("invalid value parameter in {context}: {error:?}"))
    })?;
    let name = semantic_string(tables.strings, parameter.name_id, context)?;
    let ty = tables.type_ref(
        parameter.type_body.as_deref(),
        parameter.type_id,
        type_parameters,
        context,
    )?;
    if parameter.vararg_elem_body.is_some() || parameter.vararg_elem_id.is_some() {
        tables.type_ref(
            parameter.vararg_elem_body.as_deref(),
            parameter.vararg_elem_id,
            type_parameters,
            "vararg element type",
        )?;
    }
    if parameter.equality_bound_body.is_some() || parameter.equality_bound_id.is_some() {
        tables.type_ref(
            parameter.equality_bound_body.as_deref(),
            parameter.equality_bound_id,
            type_parameters,
            "value-parameter equality bound",
        )?;
    }
    let declared = crate::metadata::DeclaredValueParameter::of_flags(parameter.flags);
    Ok(SemanticValueParameter {
        ty,
        name,
        has_default: declared.declares_default,
        inline_modifier: declared.inline_modifier,
        is_vararg: parameter.vararg_elem_body.is_some() || parameter.vararg_elem_id.is_some(),
    })
}

struct SemanticFunctionShape {
    name_id: u64,
    has_receiver: bool,
    value_params: Vec<ParsedValueParam>,
    type_params: Vec<ParsedTypeParam>,
    return_body: Option<Vec<u8>>,
    return_type_id: Option<u64>,
    receiver_body: Option<Vec<u8>>,
    receiver_type_id: Option<u64>,
    companion_receiver_body: Option<Vec<u8>>,
    companion_receiver_type_id: Option<u64>,
    context_receiver_bodies: Vec<Vec<u8>>,
    context_receiver_type_ids: Vec<u64>,
    context_params: Vec<ParsedValueParam>,
    visibility: crate::types::Visibility,
    is_inline: bool,
    is_suspend: bool,
    is_expect: bool,
    is_external: bool,
    modality: metadata::KotlinModality,
    is_operator: bool,
    is_infix: bool,
    is_synthesized: bool,
    is_static: bool,
    return_value_status: crate::types::ReturnValueStatus,
}

/// Read the target-neutral declaration fields of a Kotlin `Function` message. JVM method
/// signatures and annotations are deliberately left to their owning adapters; a KLIB provider
/// consumes only the source declaration written in the common protobuf.
fn semantic_function_shape(
    body: &[u8],
) -> Result<SemanticFunctionShape, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut compatibility_flags = None;
    let mut modern_flags = None;
    let mut name_id = 0;
    let mut has_receiver = false;
    let mut value_params = Vec::new();
    let mut type_params = Vec::new();
    let mut return_body = None;
    let mut return_type_id = None;
    let mut receiver_body = None;
    let mut receiver_type_id = None;
    let mut companion_receiver_body = None;
    let mut companion_receiver_type_id = None;
    let mut context_receiver_bodies = Vec::new();
    let mut context_receiver_type_ids = Vec::new();
    let mut context_params = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "function declaration")?;
        match (number, wire) {
            (1, 0) => compatibility_flags = Some(cursor.varint("function compatibility flags")?),
            (9, 0) => modern_flags = Some(cursor.varint("function flags")?),
            (2, 0) => name_id = cursor.varint("function name")?,
            (3, 2) => {
                return_body = Some(cursor.length_delimited("function return type")?.0.to_vec());
            }
            (4, 2) => {
                let body = cursor.length_delimited("function type parameter")?.0;
                type_params.push(
                    parse_type_param(body)
                        .map_err(|error| parameter_error("function type parameter", error))?,
                );
            }
            (5, 2) => {
                has_receiver = true;
                receiver_body = Some(
                    cursor
                        .length_delimited("function receiver type")?
                        .0
                        .to_vec(),
                );
            }
            (6, 2) => {
                let body = cursor.length_delimited("function value parameter")?.0;
                value_params.push(
                    parse_value_parameter(body)
                        .map_err(|error| parameter_error("function value parameter", error))?,
                );
            }
            (7, 0) => return_type_id = Some(cursor.varint("function return type id")?),
            (8, 0) => {
                has_receiver = true;
                receiver_type_id = Some(cursor.varint("function receiver type id")?);
            }
            (10, 2) => context_receiver_bodies.push(
                cursor
                    .length_delimited("function context receiver type")?
                    .0
                    .to_vec(),
            ),
            (11, 0) => {
                context_receiver_type_ids.push(cursor.varint("function context receiver type id")?)
            }
            (11, 2) => {
                let (packed, _) = cursor.length_delimited("function context receiver type ids")?;
                context_receiver_type_ids.extend(packed_varints(packed).ok_or_else(|| {
                    semantic_error("invalid packed function context receiver type ids")
                })?);
            }
            (13, 2) => {
                let body = cursor.length_delimited("function context parameter")?.0;
                context_params.push(
                    parse_value_parameter(body)
                        .map_err(|error| parameter_error("function context parameter", error))?,
                );
            }
            (14, 2) => {
                if companion_receiver_body.is_some() {
                    return Err(semantic_error(
                        "function companion extension receiver has duplicate inline forms",
                    ));
                }
                companion_receiver_body = Some(
                    cursor
                        .length_delimited("function companion extension receiver")?
                        .0
                        .to_vec(),
                );
            }
            (15, 0) => {
                if companion_receiver_type_id.is_some() {
                    return Err(semantic_error(
                        "function companion extension receiver has duplicate table forms",
                    ));
                }
                companion_receiver_type_id =
                    Some(cursor.varint("function companion extension receiver type id")?);
            }
            (_, wire) => cursor.skip(wire, "function declaration")?,
        }
    }
    let flags = modern_flags.or(compatibility_flags).unwrap_or(6);
    Ok(SemanticFunctionShape {
        name_id,
        has_receiver,
        value_params,
        type_params,
        return_body,
        return_type_id,
        receiver_body,
        receiver_type_id,
        companion_receiver_body,
        companion_receiver_type_id,
        context_receiver_bodies,
        context_receiver_type_ids,
        context_params,
        visibility: crate::types::Visibility::from_metadata((flags >> 1) & 0x7),
        is_inline: flags & (1 << 10) != 0,
        is_suspend: flags & (1 << 13) != 0,
        is_expect: flags & crate::metadata::function_flags::IS_EXPECT != 0,
        is_external: flags & crate::metadata::function_flags::IS_EXTERNAL != 0,
        modality: metadata::declaration_modality(flags),
        is_operator: flags & (1 << 8) != 0,
        is_infix: flags & (1 << 9) != 0,
        is_synthesized: (flags >> 6) & 0x3 == 3,
        is_static: modern_flags
            .is_some_and(|flags| flags & crate::metadata::function_flags::IS_STATIC != 0),
        return_value_status: modern_flags.map_or_else(Default::default, |flags| {
            crate::types::ReturnValueStatus::from_metadata(
                (flags >> crate::metadata::function_flags::RETURN_VALUE_STATUS_SHIFT) & 0x3,
            )
        }),
    })
}

/// The role a written context parameter declares. Metadata records an anonymous context parameter
/// (`context(_: T)`) under the reserved name kotlinc gives it, `_` or `<unused var>`, never as a
/// name a source declaration could bind.
fn context_parameter_kind(name: &str) -> crate::types::ContextParameterKind {
    match name {
        "_" | "<unused var>" => crate::types::ContextParameterKind::Anonymous,
        _ => crate::types::ContextParameterKind::Named,
    }
}

fn semantic_function(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &TypeParameterScope,
) -> Result<(metadata::KotlinMember, metadata::KotlinFunction), PackageFragmentDecodeError> {
    let contract_type_table = type_table_bodies(body, "function declaration")?;
    // Metadata's own annotation fields, `.kotlin_builtins`' `BuiltInsProtoBuf.functionAnnotation`
    // (150), and the KLIB extensions (170, 171).
    let annotation_applications = annotation_applications(
        body,
        &[12, 34, 150, 170, 171],
        tables.strings,
        tables.qnames,
        "function declaration",
    )?;
    let annotations = annotation_applications
        .iter()
        .map(|annotation| annotation.identity)
        .collect::<Vec<_>>();
    let function = semantic_function_shape(body)?;
    let type_parameter_bodies = message_bodies(body, 4, "function declaration")?;
    if function.type_params.len() != type_parameter_bodies.len() {
        return Err(semantic_error(
            "function contains a type parameter that did not decode completely",
        ));
    }
    let (formals, type_parameters) =
        tables.type_parameters(&type_parameter_bodies, inherited, "function declaration")?;
    let context_parameter_bodies = message_bodies(body, 13, "function declaration")?;
    if context_parameter_bodies.len() != function.context_params.len() {
        return Err(semantic_error(
            "function contains a context parameter that did not decode completely",
        ));
    }
    let contracts = message_bodies(body, 32, "function declaration")?;
    if contracts.len() > 1 {
        return Err(semantic_error("duplicate function contract"));
    }
    let contract = if let Some(contract) = contracts.first() {
        // Contract type ids address the nearest declared table. A function-local table wins when
        // present; otherwise the ids address the containing class or package table. Kotlin/Native's
        // `kotlin.test` fragment relies on the latter shape for all of its declared contracts.
        let (types, first_nullable) = match contract_type_table {
            Some(table) => (table.types, table.first_nullable),
            None => (tables.types.clone(), tables.first_nullable),
        };
        let contract_tables = SemanticTables {
            strings: tables.strings,
            qnames: tables.qnames,
            types,
            first_nullable,
        };
        let bounds = metadata::semantic_bounds(&formals, &crate::fx_hash::HashMap::default());
        let mut resolve_type = |reference: crate::metadata::contract_decoder::ContractTypeRef<
            '_,
        >| {
            use crate::metadata::contract_decoder::ContractTypeRef;
            let decoded = match reference {
                ContractTypeRef::Inline(body) => {
                    contract_tables.ty(body, &type_parameters, 0, "contract is-instance type")?
                }
                ContractTypeRef::Table(id) => contract_tables.ty_by_id(
                    id,
                    &type_parameters,
                    0,
                    "contract is-instance type",
                )?,
            };
            Ok(metadata::semantic_ty(&decoded, &bounds))
        };
        crate::metadata::contract_decoder::decode_contract(contract, &mut resolve_type)?
            .map(std::sync::Arc::new)
    } else {
        None
    };
    let name = semantic_string(tables.strings, function.name_id, "function declaration")?;
    let ret = tables.type_ref(
        function.return_body.as_deref(),
        function.return_type_id,
        &type_parameters,
        "function return type",
    )?;
    let ret_nullable = ret.nullable();
    let mut member_params = Vec::with_capacity(function.value_params.len());
    let mut top_params = Vec::new();
    let mut param_names = Vec::new();
    let mut param_defaults = Vec::new();
    let mut inline_modifiers = Vec::new();
    let mut vararg = None;
    let mut context_receivers = Vec::new();
    for body in &function.context_receiver_bodies {
        context_receivers.push(tables.ty(body, &type_parameters, 0, "context receiver")?);
    }
    for id in &function.context_receiver_type_ids {
        context_receivers.push(tables.ty_by_id(*id, &type_parameters, 0, "context receiver")?);
    }
    let mut context_params = Vec::new();
    for body in context_parameter_bodies {
        context_params.push(semantic_value_parameter(
            body,
            tables,
            &type_parameters,
            "context parameter",
        )?);
    }
    // Kotlin 2.2+ writes a context parameter twice: as `context_parameter` and, for older readers,
    // as the legacy `context_receiver_type` of the same type. A function has one context list.
    let contexts = if context_params.is_empty() {
        context_receivers
            .into_iter()
            .map(|ty| {
                (
                    ty,
                    String::new(),
                    false,
                    crate::types::InlineParameterModifier::None,
                    crate::types::ContextParameterKind::LegacyReceiver,
                )
            })
            .collect::<Vec<_>>()
    } else if context_receivers.is_empty()
        || context_receivers
            .iter()
            .eq(context_params.iter().map(|parameter| &parameter.ty))
    {
        context_params
            .into_iter()
            .map(|parameter| {
                let kind = context_parameter_kind(&parameter.name);
                (
                    parameter.ty,
                    parameter.name,
                    parameter.has_default,
                    parameter.inline_modifier,
                    kind,
                )
            })
            .collect()
    } else {
        return Err(semantic_error(
            "function context parameters disagree with its legacy context receivers",
        ));
    };
    let context_types = contexts
        .iter()
        .map(|(ty, _, _, _, _)| ty.clone())
        .collect::<Vec<_>>();
    let context_kinds = contexts
        .iter()
        .map(|(_, _, _, _, kind)| *kind)
        .collect::<Vec<_>>();
    for (ty, name, has_default, inline_modifier, _) in contexts {
        top_params.push(ty);
        param_names.push(name);
        param_defaults.push(has_default);
        inline_modifiers.push(inline_modifier);
    }
    let value_bodies = message_bodies(body, 6, "function declaration")?;
    if value_bodies.len() != function.value_params.len() {
        return Err(semantic_error(
            "function contains a value parameter that did not decode completely",
        ));
    }
    for value in value_bodies {
        let parameter =
            semantic_value_parameter(value, tables, &type_parameters, "function parameter")?;
        let top_index = top_params.len();
        if parameter.is_vararg {
            vararg = Some(top_index);
        }
        member_params.push(parameter.ty.clone());
        top_params.push(parameter.ty);
        param_names.push(parameter.name);
        param_defaults.push(parameter.has_default);
        inline_modifiers.push(parameter.inline_modifier);
    }
    let has_companion_receiver =
        function.companion_receiver_body.is_some() || function.companion_receiver_type_id.is_some();
    if function.has_receiver && has_companion_receiver {
        return Err(semantic_error(
            "function has both ordinary and companion extension receivers",
        ));
    }
    let receiver = if function.has_receiver {
        Some(tables.type_ref(
            function.receiver_body.as_deref(),
            function.receiver_type_id,
            &type_parameters,
            "function receiver",
        )?)
    } else if has_companion_receiver {
        Some(tables.type_ref(
            function.companion_receiver_body.as_deref(),
            function.companion_receiver_type_id,
            &type_parameters,
            "function companion extension receiver",
        )?)
    } else {
        None
    };
    // Function 14/15 is itself the canonical companion-extension placement fact. Some KLIB
    // producers do not redundantly set the modern static flag, so normalizing only from that bit
    // would publish this declaration as an ordinary package extension.
    let is_static = function.is_static || has_companion_receiver;
    let written = member_params.len();
    let leading = top_params.len() - written;
    let member = metadata::KotlinMember {
        name: name.clone(),
        receiver: receiver.clone(),
        context_params: context_types.clone(),
        visibility: function.visibility,
        params: member_params,
        ret: ret.clone(),
        is_property: false,
        is_var: false,
        is_expect: function.is_expect,
        is_operator: function.is_operator,
        is_infix: function.is_infix,
        is_abstract: function.modality == metadata::KotlinModality::Abstract,
        is_static,
        return_value_status: function.return_value_status,
        formals: formals.clone(),
        ret_nullable,
        constant: None,
        param_names: param_names[param_names.len() - written..].to_vec(),
        param_defaults: param_defaults[param_defaults.len() - written..].to_vec(),
        vararg: vararg.and_then(|index| index.checked_sub(leading)),
        annotations: annotations.clone(),
        annotation_applications: annotation_applications.clone(),
        contract: contract.clone(),
    };
    let declaration = metadata::KotlinFunction {
        name,
        receiver,
        params: top_params,
        ret,
        formals,
        param_names,
        param_defaults,
        inline_modifiers,
        vararg,
        visibility: function.visibility,
        modality: function.modality,
        is_inline: function.is_inline,
        has_reified_type_params: function
            .type_params
            .iter()
            .any(|parameter| parameter.reified),
        is_suspend: function.is_suspend,
        is_operator: function.is_operator,
        is_infix: function.is_infix,
        is_synthesized: function.is_synthesized,
        is_expect: function.is_expect,
        is_external: function.is_external,
        is_static,
        context_count: context_types.len(),
        context_kinds,
        annotations,
        annotation_applications,
        return_value_status: function.return_value_status,
        contract,
    };
    Ok((member, declaration))
}

fn semantic_accessor_modifiers(
    flags: Option<u64>,
    property_flags: u64,
    annotations: Vec<crate::types::TypeName>,
    context: &str,
) -> Result<metadata::KotlinAccessorModifiers, PackageFragmentDecodeError> {
    use crate::metadata::property_flags;
    let flags = flags.unwrap_or(property_flags & property_flags::ACCESSOR_DEFAULT_MASK);
    let modifiers = metadata::KotlinAccessorModifiers {
        visibility: metadata::declaration_visibility(flags),
        modality: metadata::declaration_modality(flags),
        is_default: flags & property_flags::ACCESSOR_IS_NOT_DEFAULT == 0,
        is_inline: flags & property_flags::ACCESSOR_IS_INLINE != 0,
        is_external: flags & property_flags::ACCESSOR_IS_EXTERNAL != 0,
        annotations,
    };
    if modifiers.is_default && (modifiers.is_inline || modifiers.is_external) {
        return Err(semantic_error(format!(
            "{context} declares inline or external without a non-default accessor"
        )));
    }
    Ok(modifiers)
}

fn semantic_property(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &TypeParameterScope,
) -> Result<(metadata::KotlinMember, metadata::KotlinProperty), PackageFragmentDecodeError> {
    validate_annotation_fields(
        body,
        &[
            14, 15, 16, 33, 34, 35, 152, 153, 170, 177, 178, 181, 182, 183,
        ],
        tables.strings,
        tables.qnames,
        "property declaration",
    )?;
    // The property declaration's own annotations: `.kotlin_builtins` writes them to
    // `BuiltInsProtoBuf.propertyAnnotation` (field 150), the same extension number a function uses,
    // and a KLIB to `KlibMetadataProtoBuf.propertyAnnotation` (170). The other annotation fields
    // above are accessor, backing-field and delegate annotations.
    let annotation_applications = annotation_applications(
        body,
        &[150, 170],
        tables.strings,
        tables.qnames,
        "property declaration",
    )?;
    let annotations = annotation_applications
        .iter()
        .map(|annotation| annotation.identity)
        .collect::<Vec<_>>();
    let type_parameter_bodies = message_bodies(body, 4, "property declaration")?;
    let (formals, type_parameters) =
        tables.type_parameters(&type_parameter_bodies, inherited, "property declaration")?;
    let mut cursor = Cursor::new(body, 0);
    let mut name = None;
    let mut return_body = None;
    let mut return_id = None;
    let mut compatibility_flags = None;
    let mut modern_flags = None;
    let mut receiver_body = None;
    let mut receiver_id = None;
    let mut companion_receiver_body = None;
    let mut companion_receiver_id = None;
    let mut context_receivers = Vec::new();
    let mut context_params = Vec::new();
    let mut constant = None;
    let mut getter_flags = None;
    let mut setter_flags = None;
    let mut setter_parameter_name = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "property declaration")?;
        match (number, wire) {
            (1, 0) => compatibility_flags = Some(cursor.varint("property compatibility flags")?),
            (2, 0) => name = Some(cursor.varint("property name")?),
            (3, 2) => return_body = Some(cursor.length_delimited("property return type")?.0),
            (9, 0) => return_id = Some(cursor.varint("property return type id")?),
            (5, 2) => {
                let nested = cursor.length_delimited("property receiver type")?.0;
                tables.ty(nested, &type_parameters, 0, "property receiver type")?;
                receiver_body = Some(nested);
            }
            (10, 0) => {
                let id = cursor.varint("property receiver type id")?;
                tables.ty_by_id(id, &type_parameters, 0, "property receiver")?;
                receiver_id = Some(id);
            }
            (12, 2) => {
                let nested = cursor.length_delimited("property context receiver type")?.0;
                context_receivers.push(tables.ty(
                    nested,
                    &type_parameters,
                    0,
                    "property context receiver",
                )?);
            }
            (18, 2) => {
                if companion_receiver_body.is_some() {
                    return Err(semantic_error(
                        "property companion extension receiver has duplicate inline forms",
                    ));
                }
                companion_receiver_body = Some(
                    cursor
                        .length_delimited("property companion extension receiver")?
                        .0,
                );
            }
            (19, 0) => {
                if companion_receiver_id.is_some() {
                    return Err(semantic_error(
                        "property companion extension receiver has duplicate table forms",
                    ));
                }
                companion_receiver_id =
                    Some(cursor.varint("property companion extension receiver type id")?);
            }
            (13, 0) => {
                let id = cursor.varint("property context receiver type id")?;
                context_receivers.push(tables.ty_by_id(
                    id,
                    &type_parameters,
                    0,
                    "property context receiver",
                )?);
            }
            (13, 2) => {
                let (packed, base) =
                    cursor.length_delimited("property context receiver type ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("property context receiver type id")?;
                    context_receivers.push(tables.ty_by_id(
                        id,
                        &type_parameters,
                        0,
                        "property context receiver",
                    )?);
                }
            }
            (173, 2) => {
                let value = cursor.length_delimited("property compile-time value")?.0;
                validate_annotation_value(value, tables.strings, tables.qnames)?;
                constant = semantic_constant(value, tables.strings)?;
            }
            (11, 0) => modern_flags = Some(cursor.varint("property flags")?),
            (7, 0) => getter_flags = Some(cursor.varint("property getter flags")?),
            (8, 0) => setter_flags = Some(cursor.varint("property setter flags")?),
            (6, 2) => {
                let parameter = cursor.length_delimited("property setter parameter")?.0;
                setter_parameter_name = Some(
                    semantic_value_parameter(
                        parameter,
                        tables,
                        &type_parameters,
                        "property setter parameter",
                    )?
                    .name,
                );
            }
            (17, 2) => {
                let parameter = cursor.length_delimited("property context parameter")?.0;
                context_params.push(semantic_value_parameter(
                    parameter,
                    tables,
                    &type_parameters,
                    "property context parameter",
                )?);
            }
            (_, wire) => cursor.skip(wire, "property declaration")?,
        }
    }
    let name = semantic_string(
        tables.strings,
        name.ok_or_else(|| semantic_error("property has no name"))?,
        "property declaration",
    )?;
    let ret = tables.type_ref(
        return_body,
        return_id,
        &type_parameters,
        "property return type",
    )?;
    let flags = modern_flags
        .or(compatibility_flags)
        .unwrap_or(crate::metadata::property_flags::DEFAULT);
    // The static bit exists only in the modern flag layout.
    let static_flag =
        modern_flags.is_some_and(|flags| flags & crate::metadata::property_flags::IS_STATIC != 0);
    let has_ordinary_receiver = receiver_body.is_some() || receiver_id.is_some();
    let has_companion_receiver =
        companion_receiver_body.is_some() || companion_receiver_id.is_some();
    if has_ordinary_receiver && has_companion_receiver {
        return Err(semantic_error(
            "property has both ordinary and companion extension receivers",
        ));
    }
    let receiver = if has_ordinary_receiver {
        Some(tables.type_ref(
            receiver_body,
            receiver_id,
            &type_parameters,
            "property receiver",
        )?)
    } else if has_companion_receiver {
        Some(tables.type_ref(
            companion_receiver_body,
            companion_receiver_id,
            &type_parameters,
            "property companion extension receiver",
        )?)
    } else {
        None
    };
    // Property 18/19 is the companion-extension placement fact even when the producer omits the
    // redundant modern static flag.
    let is_static = static_flag || has_companion_receiver;
    // As for functions, a context parameter is also written as a legacy context receiver.
    let contexts = if context_params.is_empty() {
        context_receivers
            .into_iter()
            .map(|ty| {
                (
                    ty,
                    String::new(),
                    crate::types::ContextParameterKind::LegacyReceiver,
                )
            })
            .collect::<Vec<_>>()
    } else if context_receivers.is_empty()
        || context_receivers
            .iter()
            .eq(context_params.iter().map(|parameter| &parameter.ty))
    {
        context_params
            .into_iter()
            .map(|parameter| {
                let kind = context_parameter_kind(&parameter.name);
                (parameter.ty, parameter.name, kind)
            })
            .collect()
    } else {
        return Err(semantic_error(
            "property context parameters disagree with its legacy context receivers",
        ));
    };
    let context_params = contexts
        .iter()
        .map(|(ty, _, _)| ty.clone())
        .collect::<Vec<_>>();
    let context_param_names = contexts
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect::<Vec<_>>();
    let context_kinds = contexts
        .iter()
        .map(|(_, _, kind)| *kind)
        .collect::<Vec<_>>();
    let visibility = metadata::declaration_visibility(flags);
    let accessor_annotations = |fields: &[u64]| {
        self::annotation_applications(
            body,
            fields,
            tables.strings,
            tables.qnames,
            "property accessor",
        )
        .map(|annotations| {
            annotations
                .into_iter()
                .map(|annotation| annotation.identity)
                .collect()
        })
    };
    let getter_modifiers = semantic_accessor_modifiers(
        getter_flags,
        flags,
        accessor_annotations(&[15, 152, 177])?,
        "property getter",
    )?;
    let setter_modifiers = semantic_accessor_modifiers(
        setter_flags,
        flags,
        accessor_annotations(&[16, 153, 178])?,
        "property setter",
    )?;
    let member = metadata::KotlinMember {
        name: name.clone(),
        receiver: receiver.clone(),
        context_params: context_params.clone(),
        visibility,
        params: Vec::new(),
        ret: ret.clone(),
        is_property: true,
        is_var: flags & crate::metadata::property_flags::IS_VAR != 0,
        is_expect: flags & crate::metadata::property_flags::IS_EXPECT != 0,
        is_operator: false,
        is_infix: false,
        is_abstract: flags & crate::metadata::property_flags::MODALITY_MASK
            == crate::metadata::property_flags::MODALITY_ABSTRACT,
        is_static,
        return_value_status: modern_flags.map_or_else(Default::default, |flags| {
            crate::types::ReturnValueStatus::from_metadata(
                (flags >> crate::metadata::property_flags::RETURN_VALUE_STATUS_SHIFT) & 0x3,
            )
        }),
        formals: formals.clone(),
        ret_nullable: ret.nullable(),
        constant: constant.clone(),
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        vararg: None,
        annotations: annotations.clone(),
        annotation_applications,
        contract: None,
    };
    let property = metadata::KotlinProperty {
        name,
        receiver,
        ty: ret,
        formals,
        visibility,
        modality: metadata::declaration_modality(flags),
        getter_modifiers,
        setter_modifiers,
        is_external: flags & crate::metadata::property_flags::IS_EXTERNAL != 0,
        setter_parameter_name,
        is_var: flags & crate::metadata::property_flags::IS_VAR != 0,
        is_const: flags & crate::metadata::property_flags::IS_CONST != 0,
        is_expect: flags & crate::metadata::property_flags::IS_EXPECT != 0,
        is_static,
        context_count: context_params.len(),
        context_params,
        context_param_names,
        context_kinds,
        constant,
        annotations,
        return_value_status: member.return_value_status,
    };
    Ok((member, property))
}

fn semantic_type_alias(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &TypeParameterScope,
) -> Result<metadata::KotlinTypeAlias, PackageFragmentDecodeError> {
    validate_annotation_fields(
        body,
        &[8],
        tables.strings,
        tables.qnames,
        "type-alias declaration",
    )?;
    let parameter_bodies = message_bodies(body, 3, "type-alias declaration")?;
    let (formals, type_parameters) =
        tables.type_parameters(&parameter_bodies, inherited, "type-alias declaration")?;
    let mut cursor = Cursor::new(body, 0);
    let mut flags = 6;
    let mut name = None;
    let mut underlying_body = None;
    let mut underlying_id = None;
    let mut expanded_body = None;
    let mut expanded_id = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "type-alias declaration")?;
        match (number, wire) {
            (1, 0) => flags = cursor.varint("type-alias flags")?,
            (2, 0) => name = Some(cursor.varint("type-alias name")?),
            (4, 2) => {
                underlying_body = Some(cursor.length_delimited("type-alias underlying type")?.0)
            }
            (5, 0) => underlying_id = Some(cursor.varint("type-alias underlying type id")?),
            (6, 2) => expanded_body = Some(cursor.length_delimited("type-alias expanded type")?.0),
            (7, 0) => expanded_id = Some(cursor.varint("type-alias expanded type id")?),
            (_, wire) => cursor.skip(wire, "type-alias declaration")?,
        }
    }
    let name = semantic_string(
        tables.strings,
        name.ok_or_else(|| semantic_error("type alias has no name"))?,
        "type-alias declaration",
    )?;
    tables.type_ref(
        underlying_body,
        underlying_id,
        &type_parameters,
        "type-alias underlying type",
    )?;
    let expansion = tables.type_ref(
        expanded_body,
        expanded_id,
        &type_parameters,
        "type-alias expanded type",
    )?;
    let expansion_spelling = tables.type_occurrence_spelling_ref(
        expanded_body,
        expanded_id,
        "type-alias expanded type",
    )?;
    Ok(metadata::KotlinTypeAlias {
        name,
        formals,
        expansion,
        expansion_spelling,
        visibility: metadata::declaration_visibility(flags),
    })
}

fn semantic_enum_entry(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<String, PackageFragmentDecodeError> {
    validate_annotation_fields(body, &[2, 170], strings, qnames, "enum entry")?;
    let mut cursor = Cursor::new(body, 0);
    let mut name = None;
    let mut names = 0;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "enum entry")?;
        if number == 1 {
            require_wire(&cursor, wire, 0, "enum entry")?;
            let id = cursor.varint("enum-entry name id")?;
            name = Some(semantic_string(strings, id, "enum entry")?);
            names += 1;
        } else {
            cursor.skip(wire, "enum entry")?;
        }
    }
    if names != 1 {
        return Err(semantic_error(format!(
            "enum entry has {names} name fields"
        )));
    }
    name.ok_or_else(|| semantic_error("enum entry has no name"))
}

fn semantic_constructor(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &TypeParameterScope,
) -> Result<metadata::KotlinConstructor, PackageFragmentDecodeError> {
    validate_annotation_fields(
        body,
        &[3, 170],
        tables.strings,
        tables.qnames,
        "constructor declaration",
    )?;
    let mut cursor = Cursor::new(body, 0);
    let mut flags = 6;
    let mut params = Vec::new();
    let mut names = Vec::new();
    let mut defaults = Vec::new();
    let mut vararg = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "constructor declaration")?;
        match (number, wire) {
            (1, 0) => flags = cursor.varint("constructor flags")?,
            (2, 2) => {
                let body = cursor.length_delimited("constructor value parameter")?.0;
                let parameter = semantic_value_parameter(
                    body,
                    tables,
                    type_parameters,
                    "constructor parameter",
                )?;
                if parameter.is_vararg {
                    vararg = Some(params.len());
                }
                params.push(parameter.ty);
                names.push(parameter.name);
                defaults.push(parameter.has_default);
            }
            (_, wire) => cursor.skip(wire, "constructor declaration")?,
        }
    }
    Ok(metadata::KotlinConstructor {
        // Constructor flag bit 4 is `isSecondary`.
        is_primary: flags & (1 << 4) == 0,
        params,
        param_names: names,
        param_defaults: defaults,
        vararg,
        visibility: crate::types::Visibility::from_metadata((flags >> 1) & 0x7),
    })
}

#[derive(Clone, Copy)]
struct SemanticClassHeader {
    flags: u64,
    fq_name: u64,
}

fn semantic_class_header(body: &[u8]) -> Result<SemanticClassHeader, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut flags = 6;
    let mut fq_name = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "class declaration")?;
        match (number, wire) {
            (1, 0) => flags = cursor.varint("class flags")?,
            (3, 0) => fq_name = Some(cursor.varint("class qualified-name id")?),
            (_, wire) => cursor.skip(wire, "class declaration")?,
        }
    }
    Ok(SemanticClassHeader {
        flags,
        fq_name: fq_name.ok_or_else(|| semantic_error("class has no qualified name"))?,
    })
}

fn semantic_class(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
    header: SemanticClassHeader,
    inherited_type_parameters: &TypeParameterScope,
    annotation_protocol: ClassAnnotationProtocol,
) -> Result<(String, metadata::KotlinClass, TypeParameterScope), PackageFragmentDecodeError> {
    // kotlinc reads `Class.annotation` (25) and falls back to the protocol's class-annotation
    // extension only when the class carries none there.
    let mut annotations =
        annotation_applications(body, &[25], strings, qnames, "class declaration")?;
    let extension_annotations = annotation_applications(
        body,
        &[annotation_protocol.extension_field()],
        strings,
        qnames,
        "class declaration",
    )?;
    if annotations.is_empty() {
        annotations = extension_annotations;
    }
    let (types, first_nullable) = match type_table_bodies(body, "class declaration")? {
        Some(table) => (table.types, table.first_nullable),
        None => (Vec::new(), None),
    };
    let tables = SemanticTables {
        strings,
        qnames,
        types,
        first_nullable,
    };
    let type_parameter_bodies = message_bodies(body, 5, "class declaration")?;
    let (type_params, type_parameters) = tables.type_parameters(
        &type_parameter_bodies,
        inherited_type_parameters,
        "class declaration",
    )?;
    let mut cursor = Cursor::new(body, 0);
    let mut companion_name = None;
    let mut supertype_ids = Vec::new();
    let mut supertype_bodies = Vec::new();
    let mut constructors = Vec::new();
    let mut enum_entries = Vec::new();
    let mut sealed_subclasses = Vec::new();
    let mut inline_class_property = None;
    let mut functions = Vec::new();
    let mut properties = Vec::new();
    let mut type_aliases = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "class declaration")?;
        match (number, wire) {
            (2, 0) => supertype_ids.push(cursor.varint("class supertype id")?),
            (2, 2) => {
                let (packed, base) = cursor.length_delimited("class supertype ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    supertype_ids.push(packed.varint("class supertype id")?);
                }
            }
            (4, 0) => companion_name = Some(cursor.varint("companion name id")?),
            (7, 0) => {
                let id = cursor.varint("nested-class name id")?;
                semantic_string(strings, id, "nested-class name")?;
            }
            (7, 2) => {
                let (packed, base) = cursor.length_delimited("nested-class name ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("nested-class name id")?;
                    semantic_string(strings, id, "nested-class name")?;
                }
            }
            (16, 0) => {
                let id = cursor.varint("sealed-subclass qualified-name id")?;
                sealed_subclasses.push(semantic_qname(strings, qnames, id, "sealed subclass")?);
            }
            (16, 2) => {
                let (packed, base) = cursor.length_delimited("sealed-subclass names")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("sealed-subclass qualified-name id")?;
                    sealed_subclasses.push(semantic_qname(strings, qnames, id, "sealed subclass")?);
                }
            }
            (17, 0) => {
                let id = cursor.varint("inline-class property name id")?;
                inline_class_property =
                    Some(semantic_string(strings, id, "inline-class property")?);
            }
            (6, 2) => supertype_bodies.push(cursor.length_delimited("class supertype")?.0),
            (8, 2) => constructors.push(cursor.length_delimited("class constructor")?.0),
            (9, 2) => functions.push(cursor.length_delimited("class function")?.0),
            (10, 2) => properties.push(cursor.length_delimited("class property")?.0),
            (11, 2) => type_aliases.push(cursor.length_delimited("class type alias")?.0),
            (13, 2) => {
                let entry = cursor.length_delimited("enum entry")?.0;
                enum_entries.push(semantic_enum_entry(entry, strings, qnames)?);
            }
            (18 | 20, 2) => {
                let nested = cursor.length_delimited("class semantic type")?.0;
                tables.ty(nested, &type_parameters, 0, "class semantic type")?;
            }
            (19 | 21, 0) => {
                let id = cursor.varint("class semantic type id")?;
                tables.ty_by_id(id, &type_parameters, 0, "class semantic type")?;
            }
            (21, 2) => {
                let (packed, base) = cursor.length_delimited("class context receiver type ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("class context receiver type id")?;
                    tables.ty_by_id(id, &type_parameters, 0, "class context receiver")?;
                }
            }
            (_, wire) => cursor.skip(wire, "class declaration")?,
        }
    }
    let fq_name = semantic_qname(strings, qnames, header.fq_name, "class declaration")?;
    let companion_name = companion_name
        .map(|id| semantic_string(strings, id, "class companion"))
        .transpose()?;
    let mut supertype_tys = Vec::new();
    for body in supertype_bodies {
        supertype_tys.push(tables.ty(body, &type_parameters, 0, "class supertype")?);
    }
    for id in supertype_ids {
        supertype_tys.push(tables.ty_by_id(id, &type_parameters, 0, "class supertype")?);
    }
    let mut supertypes = Vec::with_capacity(supertype_tys.len());
    for supertype in &supertype_tys {
        supertypes.push(
            supertype
                .internal()
                .map(str::to_string)
                .ok_or_else(|| semantic_error("class supertype has no classifier identity"))?,
        );
    }
    let constructors = constructors
        .into_iter()
        .map(|body| semantic_constructor(body, &tables, &type_parameters))
        .collect::<Result<Vec<_>, _>>()?;
    let mut members = Vec::new();
    let mut nullable_member_returns = Vec::new();
    let mut function_declarations = Vec::with_capacity(functions.len());
    for body in functions {
        let (member, declaration) = semantic_function(body, &tables, &type_parameters)?;
        if member.ret_nullable {
            nullable_member_returns.push((member.name.clone(), member.params.len()));
        }
        members.push(member);
        function_declarations.push(declaration);
    }
    let mut property_declarations = Vec::with_capacity(properties.len());
    for body in properties {
        let (member, declaration) = semantic_property(body, &tables, &type_parameters)?;
        members.push(member);
        property_declarations.push(declaration);
    }
    let type_aliases = type_aliases
        .into_iter()
        .map(|body| semantic_type_alias(body, &tables, &type_parameters))
        .collect::<Result<Vec<_>, _>>()?;
    let is_nested = fq_name
        .rsplit('/')
        .next()
        .is_some_and(|tail| tail.contains('.'));
    Ok((
        fq_name,
        metadata::KotlinClass {
            supertypes,
            supertype_tys,
            members,
            functions: function_declarations,
            properties: property_declarations,
            type_aliases,
            constructors,
            companion_name,
            type_params,
            nullable_member_returns,
            enum_entries,
            has_enum_entries: header.flags & (1 << 15) != 0,
            sealed_subclasses,
            inline_class_property,
            kind: metadata::class_kind(header.flags),
            is_fun_interface: header.flags & (1 << 14) != 0,
            visibility: metadata::declaration_visibility(header.flags),
            is_expect: header.flags & (1 << 12) != 0,
            modality: metadata::declaration_modality(header.flags),
            is_nested,
            is_inner: header.flags & (1 << 9) != 0,
            metadata_flags: header.flags,
            annotations,
        },
        type_parameters,
    ))
}

pub(super) fn parse(
    decoded: DecodedPackageFragment<'_>,
) -> Result<KotlinPackage, PackageFragmentDecodeError> {
    let DecodedPackageFragment {
        strings,
        qnames,
        package,
        classes,
        file_annotations,
        class_names,
        class_annotations,
    } = decoded;
    let mut result = KotlinPackage::default();
    for annotation in file_annotations {
        validate_annotation(annotation, &strings, &qnames)?;
    }
    for class_name in class_names {
        semantic_qname(&strings, &qnames, class_name, "package-fragment class name")?;
    }
    if let Some(package) = package {
        let (types, first_nullable) = match type_table_bodies(package, "package declaration")? {
            Some(table) => (table.types, table.first_nullable),
            None => (Vec::new(), None),
        };
        let tables = SemanticTables {
            strings: &strings,
            qnames: &qnames,
            types,
            first_nullable,
        };
        for body in message_bodies(package, 3, "package declaration")? {
            let (_, function) = semantic_function(body, &tables, &TypeParameterScope::default())?;
            result.functions.push(function);
        }
        for body in message_bodies(package, 4, "package declaration")? {
            let (_, property) = semantic_property(body, &tables, &TypeParameterScope::default())?;
            result.properties.push(property);
        }
        for body in message_bodies(package, 5, "package declaration")? {
            result.type_aliases.push(semantic_type_alias(
                body,
                &tables,
                &TypeParameterScope::default(),
            )?);
        }
    }
    let class_headers = classes
        .iter()
        .map(|body| semantic_class_header(body))
        .collect::<Result<Vec<_>, _>>()?;
    let mut class_by_qname = crate::fx_hash::HashMap::default();
    for (index, header) in class_headers.iter().enumerate() {
        if class_by_qname.insert(header.fq_name, index).is_some() {
            return Err(semantic_error(format!(
                "duplicate class qualified-name id {}",
                header.fq_name
            )));
        }
    }
    let mut decoded = (0..classes.len())
        .map(|_| None)
        .collect::<Vec<Option<(String, metadata::KotlinClass, TypeParameterScope)>>>();
    let mut remaining = classes.len();
    while remaining != 0 {
        let mut progressed = false;
        for index in 0..classes.len() {
            if decoded[index].is_some() {
                continue;
            }
            let header = class_headers[index];
            let inherited = if header.flags & (1 << 9) != 0 {
                let qname_index = usize::try_from(header.fq_name)
                    .map_err(|_| semantic_error("class qualified-name id exceeds host range"))?;
                let parent = qnames
                    .get(qname_index)
                    .ok_or_else(|| {
                        semantic_error(format!(
                            "class declaration references absent qualified name {}",
                            header.fq_name
                        ))
                    })?
                    .parent;
                let parent = usize::try_from(parent)
                    .map_err(|_| semantic_error("inner class has no enclosing class identity"))?;
                let parent_index = class_by_qname.get(&(parent as u64)).ok_or_else(|| {
                    semantic_error("inner class enclosing declaration is absent from its fragment")
                })?;
                let Some((_, _, parent_scope)) = &decoded[*parent_index] else {
                    continue;
                };
                parent_scope.clone()
            } else {
                TypeParameterScope::default()
            };
            decoded[index] = Some(semantic_class(
                classes[index],
                &strings,
                &qnames,
                header,
                &inherited,
                class_annotations,
            )?);
            remaining -= 1;
            progressed = true;
        }
        if !progressed {
            return Err(semantic_error(
                "cyclic inner-class enclosing declaration chain",
            ));
        }
    }
    for decoded in decoded {
        let (name, class, _) =
            decoded.ok_or_else(|| semantic_error("class declaration was not decoded"))?;
        if result.classes.insert(name.clone(), class).is_some() {
            return Err(semantic_error(format!("duplicate class identity {name}")));
        }
        result.class_order.push(name);
    }
    Ok(result)
}

#[cfg(test)]
mod semantic_decoder_tests;
