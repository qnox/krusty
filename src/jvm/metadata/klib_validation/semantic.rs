// `use super::super as metadata;` is not valid Rust: `super` cannot be the final segment of a
// renamed import, so the compiler looks for an ITEM named `super` in this module's parent. The
// path is spelled out instead.
use super::*;
use crate::jvm::metadata;

fn semantic_error(detail: impl Into<String>) -> PackageFragmentDecodeError {
    PackageFragmentDecodeError {
        offset: 0,
        detail: detail.into(),
    }
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
    if qnames.get(id).is_none() {
        return Err(semantic_error(format!(
            "{context} references absent qualified name {id}"
        )));
    }
    let name = metadata::resolve_qname(qnames, strings, id as i64);
    if name.is_empty() {
        return Err(semantic_error(format!(
            "{context} resolves to an empty qualified name"
        )));
    }
    Ok(name)
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

fn validate_annotation(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(), PackageFragmentDecodeError> {
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
    semantic_qname(
        strings,
        qnames,
        class_id.ok_or_else(|| semantic_error("annotation has no class identity"))?,
        "annotation",
    )?;
    Ok(())
}

fn validate_annotation_fields(
    body: &[u8],
    annotation_fields: &[u64],
    strings: &[String],
    qnames: &[QName],
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, context)?;
        if annotation_fields.contains(&number) {
            require_wire(&cursor, wire, 2, context)?;
            let annotation = cursor.length_delimited("annotation")?.0;
            validate_annotation(annotation, strings, qnames)?;
        } else {
            cursor.skip(wire, context)?;
        }
    }
    Ok(())
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

impl SemanticTables<'_> {
    fn validate_type_edges(
        &self,
        body: &[u8],
        type_parameters: &std::collections::HashMap<u64, String>,
        depth: u32,
        context: &str,
    ) -> Result<Option<metadata::BuiltinTy>, PackageFragmentDecodeError> {
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
             -> Result<Option<metadata::BuiltinTy>, PackageFragmentDecodeError> {
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
        let flexible = decode_edge(flexible_inline, flexible_ids, "flexible upper type")?;
        decode_edge(outer_inline, outer_ids, "outer type")?;
        decode_edge(abbreviated_inline, abbreviated_ids, "abbreviated type")?;
        Ok(flexible)
    }

    fn ty(
        &self,
        body: &[u8],
        type_parameters: &std::collections::HashMap<u64, String>,
        depth: u32,
        context: &str,
    ) -> Result<metadata::BuiltinTy, PackageFragmentDecodeError> {
        if depth > 16 {
            return Err(semantic_error(format!(
                "cyclic or excessively nested type in {context}"
            )));
        }
        let node = metadata::parse_type_node(body)
            .ok_or_else(|| semantic_error(format!("invalid semantic type in {context}")))?;
        let flexible_upper = self.validate_type_edges(body, type_parameters, depth, context)?;
        let mut arguments = Vec::with_capacity(node.arguments.len());
        for argument in node.arguments {
            let argument = match argument {
                metadata::ParsedTypeArgument::Inline(body, projection) => {
                    let ty = self.ty(body, type_parameters, depth + 1, context)?;
                    metadata::project_builtin_ty(projection, ty)
                }
                metadata::ParsedTypeArgument::Table(id, projection) => {
                    let ty = self.ty_by_id(id, type_parameters, depth + 1, context)?;
                    metadata::project_builtin_ty(projection, ty)
                }
                metadata::ParsedTypeArgument::Star => {
                    metadata::BuiltinTy::OutProjection(Box::new(metadata::BuiltinTy::Class {
                        internal: "kotlin/Any".to_string(),
                        args: Vec::new(),
                        nullable: true,
                    }))
                }
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
            || flexible_upper
                .as_ref()
                .is_some_and(metadata::BuiltinTy::nullable);
        let nullable = nullable && !node.definitely_non_null;
        if let Some(id) = node.class_id.or(node.type_alias_id) {
            return Ok(metadata::BuiltinTy::Class {
                internal: semantic_qname(self.strings, self.qnames, id, context)?,
                args: arguments,
                nullable,
            });
        }
        let name = if let Some(id) = node.type_parameter_id {
            type_parameters.get(&id).cloned().ok_or_else(|| {
                semantic_error(format!("{context} references absent type parameter {id}"))
            })?
        } else if let Some(id) = node.type_parameter_name_id {
            semantic_string(self.strings, id, context)?
        } else {
            return Err(semantic_error(format!(
                "{context} type has no classifier or type parameter"
            )));
        };
        Ok(metadata::BuiltinTy::Param { name, nullable })
    }

    fn ty_by_id(
        &self,
        id: u64,
        type_parameters: &std::collections::HashMap<u64, String>,
        depth: u32,
        context: &str,
    ) -> Result<metadata::BuiltinTy, PackageFragmentDecodeError> {
        let id = usize::try_from(id)
            .map_err(|_| semantic_error(format!("{context} type id exceeds host range")))?;
        let body = self
            .types
            .get(id)
            .ok_or_else(|| semantic_error(format!("{context} references absent type {id}")))?;
        let definitely_non_null = metadata::parse_type_node(body)
            .ok_or_else(|| semantic_error(format!("invalid semantic type in {context}")))?
            .definitely_non_null;
        let ty = self.ty(body, type_parameters, depth, context)?;
        if self
            .first_nullable
            .is_some_and(|first| id >= first && !definitely_non_null)
        {
            Ok(match ty {
                metadata::BuiltinTy::Class { internal, args, .. } => metadata::BuiltinTy::Class {
                    internal,
                    args,
                    nullable: true,
                },
                metadata::BuiltinTy::Param { name, .. } => metadata::BuiltinTy::Param {
                    name,
                    nullable: true,
                },
                metadata::BuiltinTy::InProjection(inner) => {
                    metadata::BuiltinTy::InProjection(inner)
                }
                metadata::BuiltinTy::OutProjection(inner) => {
                    metadata::BuiltinTy::OutProjection(inner)
                }
            })
        } else {
            Ok(ty)
        }
    }

    fn type_ref(
        &self,
        inline: Option<&[u8]>,
        table_id: Option<u64>,
        type_parameters: &std::collections::HashMap<u64, String>,
        context: &str,
    ) -> Result<metadata::BuiltinTy, PackageFragmentDecodeError> {
        match (inline, table_id) {
            (Some(body), None) => self.ty(body, type_parameters, 0, context),
            (None, Some(id)) => self.ty_by_id(id, type_parameters, 0, context),
            (None, None) => Err(semantic_error(format!("{context} has no type"))),
            (Some(_), Some(_)) => Err(semantic_error(format!(
                "{context} has conflicting inline and table type forms"
            ))),
        }
    }

    fn type_parameters(
        &self,
        bodies: &[&[u8]],
        inherited: &std::collections::HashMap<u64, String>,
        context: &str,
    ) -> Result<
        (
            Vec<metadata::BuiltinTypeParam>,
            std::collections::HashMap<u64, String>,
        ),
        PackageFragmentDecodeError,
    > {
        let mut names = inherited.clone();
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
            let parameter = metadata::parse_type_param(body).ok_or_else(|| {
                semantic_error(format!("invalid semantic type parameter in {context}"))
            })?;
            let name = semantic_string(self.strings, parameter.name_id, context)?;
            if names.insert(parameter.id, name).is_some() {
                return Err(semantic_error(format!(
                    "duplicate type-parameter id {} in {context}",
                    parameter.id
                )));
            }
            parsed.push(parameter);
        }
        let mut parameters = Vec::with_capacity(parsed.len());
        for parameter in parsed {
            let name = names
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
            parameters.push(metadata::BuiltinTypeParam {
                name,
                bounds,
                variance: parameter.variance,
                only_input,
            });
        }
        Ok((parameters, names))
    }
}

struct SemanticValueParameter {
    ty: metadata::BuiltinTy,
    name: String,
    has_default: bool,
    is_vararg: bool,
}

fn validate_contract_expression(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &std::collections::HashMap<u64, String>,
) -> Result<(), PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut inline_types = Vec::new();
    let mut type_ids = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract expression")?;
        match (number, wire) {
            (4, 2) => inline_types.push(cursor.length_delimited("contract is-instance type")?.0),
            (5, 0) => type_ids.push(cursor.varint("contract is-instance type id")?),
            (6 | 7, 2) => {
                let nested = cursor.length_delimited("nested contract expression")?.0;
                validate_contract_expression(nested, tables, type_parameters)?;
            }
            (_, wire) => cursor.skip(wire, "contract expression")?,
        }
    }
    match (inline_types.as_slice(), type_ids.as_slice()) {
        ([], []) => Ok(()),
        ([body], []) => tables
            .ty(body, type_parameters, 0, "contract is-instance type")
            .map(|_| ()),
        ([], [id]) => tables
            .ty_by_id(*id, type_parameters, 0, "contract is-instance type")
            .map(|_| ()),
        _ => Err(semantic_error(
            "contract expression has duplicate or conflicting is-instance types",
        )),
    }
}

fn validate_contract(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &std::collections::HashMap<u64, String>,
) -> Result<(), PackageFragmentDecodeError> {
    for effect in message_bodies(body, 1, "contract")? {
        for argument in message_bodies(effect, 2, "contract effect")? {
            validate_contract_expression(argument, tables, type_parameters)?;
        }
        for conclusion in message_bodies(effect, 3, "contract effect")? {
            validate_contract_expression(conclusion, tables, type_parameters)?;
        }
    }
    Ok(())
}

fn semantic_value_parameter(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &std::collections::HashMap<u64, String>,
    context: &str,
) -> Result<SemanticValueParameter, PackageFragmentDecodeError> {
    validate_annotation_fields(body, &[7, 170], tables.strings, tables.qnames, context)?;
    for value in message_bodies(body, 8, context)? {
        validate_annotation_value(value, tables.strings, tables.qnames)?;
    }
    let parameter = metadata::parse_value_parameter(body)
        .ok_or_else(|| semantic_error(format!("invalid value parameter in {context}")))?;
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
    Ok(SemanticValueParameter {
        ty,
        name,
        has_default: parameter.has_default,
        is_vararg: parameter.vararg_elem_body.is_some() || parameter.vararg_elem_id.is_some(),
    })
}

fn semantic_function(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &std::collections::HashMap<u64, String>,
    top_level: bool,
) -> Result<(metadata::BuiltinMember, Option<metadata::BuiltinFunction>), PackageFragmentDecodeError>
{
    let contract_type_table = type_table_bodies(body, "function declaration")?;
    validate_annotation_fields(
        body,
        &[12, 34, 170, 171],
        tables.strings,
        tables.qnames,
        "function declaration",
    )?;
    let function = metadata::parse_function(body)
        .ok_or_else(|| semantic_error("invalid semantic function declaration"))?;
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
    if let Some(contract) = contracts.first() {
        let (types, first_nullable) = match contract_type_table {
            Some(table) => (table.types, table.first_nullable),
            None => (Vec::new(), None),
        };
        let contract_tables = SemanticTables {
            strings: tables.strings,
            qnames: tables.qnames,
            types,
            first_nullable,
        };
        validate_contract(contract, &contract_tables, &type_parameters)?;
    }
    let mut extras = Cursor::new(body, 0);
    while !extras.at_end() {
        let (number, wire) = field(&mut extras, "function declaration")?;
        match (number, wire) {
            (14, 2) => {
                let body = extras
                    .length_delimited("function companion extension receiver")?
                    .0;
                tables.ty(
                    body,
                    &type_parameters,
                    0,
                    "function companion extension receiver",
                )?;
            }
            (15, 0) => {
                let id = extras.varint("function companion extension receiver type id")?;
                tables.ty_by_id(
                    id,
                    &type_parameters,
                    0,
                    "function companion extension receiver",
                )?;
            }
            (_, wire) => extras.skip(wire, "function declaration")?,
        }
    }
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
    let mut vararg = None;
    for body in &function.context_receiver_bodies {
        top_params.push(tables.ty(body, &type_parameters, 0, "context receiver")?);
        param_names.push(String::new());
        param_defaults.push(false);
    }
    for id in &function.context_receiver_type_ids {
        top_params.push(tables.ty_by_id(*id, &type_parameters, 0, "context receiver")?);
        param_names.push(String::new());
        param_defaults.push(false);
    }
    for body in context_parameter_bodies {
        let parameter =
            semantic_value_parameter(body, tables, &type_parameters, "context parameter")?;
        top_params.push(parameter.ty);
        param_names.push(parameter.name);
        param_defaults.push(parameter.has_default);
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
    }
    let receiver = if function.has_receiver {
        Some(tables.type_ref(
            function.receiver_body.as_deref(),
            function.receiver_type_id,
            &type_parameters,
            "function receiver",
        )?)
    } else {
        None
    };
    let member = metadata::BuiltinMember {
        name: name.clone(),
        params: member_params,
        ret: ret.clone(),
        is_property: false,
        is_operator: function.is_operator,
        is_infix: function.is_infix,
        is_abstract: function.is_abstract,
        formals: formals.clone(),
        ret_nullable,
    };
    let top = top_level.then_some(metadata::BuiltinFunction {
        name,
        receiver,
        params: top_params,
        ret,
        formals,
        param_names,
        param_defaults,
        vararg,
        visibility: function.visibility,
        is_inline: function.is_inline,
        has_reified_type_params: function
            .type_params
            .iter()
            .any(|parameter| parameter.reified),
        is_suspend: function.is_suspend,
        is_operator: function.is_operator,
        is_infix: function.is_infix,
        context_count: function.context_receiver_bodies.len()
            + function.context_receiver_type_ids.len()
            + function.context_params.len(),
    });
    Ok((member, top))
}

fn semantic_property(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &std::collections::HashMap<u64, String>,
) -> Result<metadata::BuiltinMember, PackageFragmentDecodeError> {
    validate_annotation_fields(
        body,
        &[14, 15, 16, 33, 34, 35, 170, 177, 178, 181, 182, 183],
        tables.strings,
        tables.qnames,
        "property declaration",
    )?;
    let type_parameter_bodies = message_bodies(body, 4, "property declaration")?;
    let (formals, type_parameters) =
        tables.type_parameters(&type_parameter_bodies, inherited, "property declaration")?;
    let mut cursor = Cursor::new(body, 0);
    let mut name = None;
    let mut return_body = None;
    let mut return_id = None;
    let mut legacy_flags = None;
    let mut modern_flags = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "property declaration")?;
        match (number, wire) {
            (1, 0) => legacy_flags = Some(cursor.varint("property flags")?),
            (2, 0) => name = Some(cursor.varint("property name")?),
            (3, 2) => return_body = Some(cursor.length_delimited("property return type")?.0),
            (9, 0) => return_id = Some(cursor.varint("property return type id")?),
            (5 | 12 | 18, 2) => {
                let nested = cursor.length_delimited("property semantic type")?.0;
                tables.ty(nested, &type_parameters, 0, "property semantic type")?;
            }
            (10 | 19, 0) => {
                let id = cursor.varint("property semantic type id")?;
                tables.ty_by_id(id, &type_parameters, 0, "property receiver")?;
            }
            (13, 0) => {
                let id = cursor.varint("property context receiver type id")?;
                tables.ty_by_id(id, &type_parameters, 0, "property context receiver")?;
            }
            (13, 2) => {
                let (packed, base) =
                    cursor.length_delimited("property context receiver type ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("property context receiver type id")?;
                    tables.ty_by_id(id, &type_parameters, 0, "property context receiver")?;
                }
            }
            (173, 2) => {
                let value = cursor.length_delimited("property compile-time value")?.0;
                validate_annotation_value(value, tables.strings, tables.qnames)?;
            }
            (11, 0) => modern_flags = Some(cursor.varint("property flags")?),
            (6 | 17, 2) => {
                let parameter = cursor.length_delimited("property value parameter")?.0;
                semantic_value_parameter(
                    parameter,
                    tables,
                    &type_parameters,
                    "property value parameter",
                )?;
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
        .or(legacy_flags)
        .unwrap_or(crate::metadata::property_flags::DEFAULT);
    Ok(metadata::BuiltinMember {
        name,
        params: Vec::new(),
        ret: ret.clone(),
        is_property: true,
        is_operator: false,
        is_infix: false,
        is_abstract: flags & crate::metadata::property_flags::MODALITY_MASK
            == crate::metadata::property_flags::MODALITY_ABSTRACT,
        formals,
        ret_nullable: ret.nullable(),
    })
}

fn validate_type_alias(
    body: &[u8],
    tables: &SemanticTables<'_>,
    inherited: &std::collections::HashMap<u64, String>,
) -> Result<(), PackageFragmentDecodeError> {
    validate_annotation_fields(
        body,
        &[8],
        tables.strings,
        tables.qnames,
        "type-alias declaration",
    )?;
    let parameter_bodies = message_bodies(body, 3, "type-alias declaration")?;
    let (_, type_parameters) =
        tables.type_parameters(&parameter_bodies, inherited, "type-alias declaration")?;
    let mut cursor = Cursor::new(body, 0);
    let mut name = None;
    let mut underlying_body = None;
    let mut underlying_id = None;
    let mut expanded_body = None;
    let mut expanded_id = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "type-alias declaration")?;
        match (number, wire) {
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
    semantic_string(
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
    tables.type_ref(
        expanded_body,
        expanded_id,
        &type_parameters,
        "type-alias expanded type",
    )?;
    Ok(())
}

fn validate_enum_entry(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(), PackageFragmentDecodeError> {
    validate_annotation_fields(body, &[2, 170], strings, qnames, "enum entry")?;
    let mut cursor = Cursor::new(body, 0);
    let mut names = 0;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "enum entry")?;
        if number == 1 {
            require_wire(&cursor, wire, 0, "enum entry")?;
            let id = cursor.varint("enum-entry name id")?;
            semantic_string(strings, id, "enum entry")?;
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
    Ok(())
}

fn semantic_constructor(
    body: &[u8],
    tables: &SemanticTables<'_>,
    type_parameters: &std::collections::HashMap<u64, String>,
) -> Result<metadata::BuiltinConstructor, PackageFragmentDecodeError> {
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
    Ok(metadata::BuiltinConstructor {
        params,
        param_names: names,
        param_defaults: defaults,
        vararg,
        visibility: crate::types::Visibility::from_metadata(metadata::flags_visibility(flags)),
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
    inherited_type_parameters: &std::collections::HashMap<u64, String>,
) -> Result<
    (
        String,
        metadata::BuiltinClass,
        std::collections::HashMap<u64, String>,
    ),
    PackageFragmentDecodeError,
> {
    validate_annotation_fields(body, &[25, 170], strings, qnames, "class declaration")?;
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
                semantic_qname(strings, qnames, id, "sealed subclass")?;
            }
            (16, 2) => {
                let (packed, base) = cursor.length_delimited("sealed-subclass names")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    let id = packed.varint("sealed-subclass qualified-name id")?;
                    semantic_qname(strings, qnames, id, "sealed subclass")?;
                }
            }
            (17, 0) => {
                let id = cursor.varint("inline-class property name id")?;
                semantic_string(strings, id, "inline-class property")?;
            }
            (6, 2) => supertype_bodies.push(cursor.length_delimited("class supertype")?.0),
            (8, 2) => constructors.push(cursor.length_delimited("class constructor")?.0),
            (9, 2) => functions.push(cursor.length_delimited("class function")?.0),
            (10, 2) => properties.push(cursor.length_delimited("class property")?.0),
            (11, 2) => type_aliases.push(cursor.length_delimited("class type alias")?.0),
            (13, 2) => {
                let entry = cursor.length_delimited("enum entry")?.0;
                validate_enum_entry(entry, strings, qnames)?;
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
    for body in functions {
        let (member, _) = semantic_function(body, &tables, &type_parameters, false)?;
        if member.ret_nullable {
            nullable_member_returns.push((member.name.clone(), member.params.len()));
        }
        members.push(member);
    }
    for body in properties {
        members.push(semantic_property(body, &tables, &type_parameters)?);
    }
    for body in type_aliases {
        validate_type_alias(body, &tables, &type_parameters)?;
    }
    let is_nested = fq_name
        .rsplit('/')
        .next()
        .is_some_and(|tail| tail.contains('.'));
    Ok((
        fq_name,
        metadata::BuiltinClass {
            supertypes,
            supertype_tys,
            members,
            constructors,
            companion_name,
            type_params,
            nullable_member_returns,
            kind: metadata::builtin_class_kind(header.flags),
            visibility: metadata::builtin_class_visibility(header.flags),
            is_expect: header.flags & (1 << 12) != 0,
            is_nested,
            access: metadata::builtin_class_access(header.flags),
        },
        type_parameters,
    ))
}

pub(super) fn parse(
    decoded: DecodedPackageFragment<'_>,
) -> Result<BuiltinPackage, PackageFragmentDecodeError> {
    let DecodedPackageFragment {
        strings,
        qnames,
        package,
        classes,
        file_annotations,
        class_names,
    } = decoded;
    let mut result = BuiltinPackage::default();
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
            let (_, function) =
                semantic_function(body, &tables, &std::collections::HashMap::new(), true)?;
            result.functions.push(
                function.ok_or_else(|| semantic_error("top-level function lost its identity"))?,
            );
        }
        for body in message_bodies(package, 4, "package declaration")? {
            semantic_property(body, &tables, &std::collections::HashMap::new())?;
        }
        for body in message_bodies(package, 5, "package declaration")? {
            validate_type_alias(body, &tables, &std::collections::HashMap::new())?;
        }
    }
    let class_headers = classes
        .iter()
        .map(|body| semantic_class_header(body))
        .collect::<Result<Vec<_>, _>>()?;
    let mut class_by_qname = std::collections::HashMap::new();
    for (index, header) in class_headers.iter().enumerate() {
        if class_by_qname.insert(header.fq_name, index).is_some() {
            return Err(semantic_error(format!(
                "duplicate class qualified-name id {}",
                header.fq_name
            )));
        }
    }
    let mut decoded = (0..classes.len()).map(|_| None).collect::<Vec<
        Option<(
            String,
            metadata::BuiltinClass,
            std::collections::HashMap<u64, String>,
        )>,
    >>();
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
                std::collections::HashMap::new()
            };
            decoded[index] = Some(semantic_class(
                classes[index],
                &strings,
                &qnames,
                header,
                &inherited,
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
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::parse_package_fragment_checked;

    fn varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            out.push(if value == 0 { byte } else { byte | 0x80 });
            if value == 0 {
                return;
            }
        }
    }

    fn int_field(out: &mut Vec<u8>, number: u64, value: u64) {
        varint(out, number << 3);
        varint(out, value);
    }

    fn bytes_field(out: &mut Vec<u8>, number: u64, value: &[u8]) {
        varint(out, number << 3 | 2);
        varint(out, value.len() as u64);
        out.extend_from_slice(value);
    }

    fn class_type(qname: u64) -> Vec<u8> {
        let mut ty = Vec::new();
        int_field(&mut ty, 6, qname);
        ty
    }

    fn type_table(types: &[Vec<u8>], first_nullable: Option<u64>) -> Vec<u8> {
        let mut table = Vec::new();
        for ty in types {
            bytes_field(&mut table, 1, ty);
        }
        if let Some(first_nullable) = first_nullable {
            int_field(&mut table, 2, first_nullable);
        }
        table
    }

    fn qname(short: u64, parent: Option<u64>, kind: u64) -> Vec<u8> {
        let mut name = Vec::new();
        if let Some(parent) = parent {
            int_field(&mut name, 1, parent);
        }
        int_field(&mut name, 2, short);
        int_field(&mut name, 3, kind);
        name
    }

    fn fragment(strings: &[&str], qnames: &[Vec<u8>], package: &[u8]) -> Vec<u8> {
        let mut string_table = Vec::new();
        for string in strings {
            bytes_field(&mut string_table, 1, string.as_bytes());
        }
        let mut qname_table = Vec::new();
        for qname in qnames {
            bytes_field(&mut qname_table, 1, qname);
        }
        let mut fragment = Vec::new();
        bytes_field(&mut fragment, 1, &string_table);
        bytes_field(&mut fragment, 2, &qname_table);
        bytes_field(&mut fragment, 3, package);
        fragment
    }

    fn fragment_with_classes(strings: &[&str], qnames: &[Vec<u8>], classes: &[Vec<u8>]) -> Vec<u8> {
        let mut fragment = fragment(strings, qnames, &[]);
        for class in classes {
            bytes_field(&mut fragment, 4, class);
        }
        fragment
    }

    fn kotlin_types(names: &[&'static str]) -> (Vec<&'static str>, Vec<Vec<u8>>) {
        let mut strings = vec!["f", "p", "kotlin"];
        let mut qnames = vec![qname(2, None, 1)];
        for name in names {
            let short = strings.len() as u64;
            strings.push(*name);
            qnames.push(qname(short, Some(0), 0));
        }
        (strings, qnames)
    }

    fn function_with_return(return_body: Option<&[u8]>, return_id: Option<u64>) -> Vec<u8> {
        let mut function = Vec::new();
        int_field(&mut function, 2, 0);
        if let Some(return_body) = return_body {
            bytes_field(&mut function, 3, return_body);
        }
        if let Some(return_id) = return_id {
            int_field(&mut function, 7, return_id);
        }
        function
    }

    fn package_with_function(function: &[u8], table: Option<&[u8]>) -> Vec<u8> {
        let mut package = Vec::new();
        bytes_field(&mut package, 3, function);
        if let Some(table) = table {
            bytes_field(&mut package, 30, table);
        }
        package
    }

    fn exact_error(bytes: &[u8]) -> (usize, String) {
        let error = parse_package_fragment_checked(bytes)
            .err()
            .expect("fixed invalid fragment must fail");
        (error.offset, error.detail)
    }

    #[test]
    fn first_nullable_is_part_of_the_selected_semantic_type_table() {
        let (strings, qnames) = kotlin_types(&["String"]);
        let table = type_table(&[class_type(1)], Some(0));
        let function = function_with_return(None, Some(0));
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&function, Some(&table)),
        );

        let package = parse_package_fragment_checked(&bytes).expect("valid nullable table fixture");
        assert_eq!(package.functions.len(), 1);
        assert_eq!(package.functions[0].ret.internal(), Some("kotlin/String"));
        assert!(package.functions[0].ret.nullable());
    }

    #[test]
    fn inner_class_supertype_resolves_the_enclosing_class_type_parameter() {
        let strings = ["p", "Outer", "Inner", "T", "Base", "StaticNested"];
        let qnames = [
            qname(0, None, 1),
            qname(1, Some(0), 0),
            qname(2, Some(1), 0),
            qname(4, Some(0), 0),
            qname(5, Some(1), 0),
        ];

        let mut type_parameter = Vec::new();
        int_field(&mut type_parameter, 1, 0);
        int_field(&mut type_parameter, 2, 3);
        let mut outer = Vec::new();
        int_field(&mut outer, 3, 1);
        bytes_field(&mut outer, 5, &type_parameter);

        let mut parameter_type = Vec::new();
        int_field(&mut parameter_type, 7, 0);
        let mut argument = Vec::new();
        int_field(&mut argument, 3, 0);
        let mut base_of_parameter = Vec::new();
        bytes_field(&mut base_of_parameter, 2, &argument);
        int_field(&mut base_of_parameter, 6, 3);
        let table = type_table(&[parameter_type, base_of_parameter], None);
        let mut inner = Vec::new();
        int_field(&mut inner, 1, 530);
        int_field(&mut inner, 2, 1);
        int_field(&mut inner, 3, 2);
        bytes_field(&mut inner, 30, &table);

        // KLIB fragments do not promise enclosing-before-nested declaration order. The inner class
        // still resolves Type.type_parameter = 0 through its enclosing class's declaration scope.
        let bytes = fragment_with_classes(&strings, &qnames, &[inner, outer.clone()]);
        let package = parse_package_fragment_checked(&bytes).expect("valid inner-class type scope");
        assert_eq!(package.classes["p/Outer"].type_params[0].name, "T");
        assert!(package.classes["p/Outer.Inner"].type_params.is_empty());
        assert_eq!(
            package.classes["p/Outer.Inner"].supertype_tys[0].render(),
            "p/Base<T>"
        );

        let mut static_nested = Vec::new();
        int_field(&mut static_nested, 1, 2);
        int_field(&mut static_nested, 2, 1);
        int_field(&mut static_nested, 3, 4);
        bytes_field(&mut static_nested, 30, &table);
        let bytes = fragment_with_classes(&strings, &qnames, &[outer, static_nested]);
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "class supertype references absent type parameter 0".to_string()
            )
        );
    }

    #[test]
    fn dangling_alias_equality_context_and_annotation_references_are_exact_errors() {
        let (strings, qnames) = kotlin_types(&["Unit"]);
        let unit = class_type(1);

        let mut alias = Vec::new();
        int_field(&mut alias, 12, 99);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&function_with_return(Some(&alias), None), None),
        );
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "function return type references absent qualified name 99".to_string()
            )
        );

        let mut equality_parameter = Vec::new();
        int_field(&mut equality_parameter, 2, 1);
        bytes_field(&mut equality_parameter, 3, &unit);
        int_field(&mut equality_parameter, 10, 9);
        let mut equality_function = function_with_return(Some(&unit), None);
        bytes_field(&mut equality_function, 6, &equality_parameter);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&equality_function, None),
        );
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "value-parameter equality bound references absent type 9".to_string()
            )
        );

        let mut context_parameter = Vec::new();
        int_field(&mut context_parameter, 2, 1);
        int_field(&mut context_parameter, 5, 9);
        let mut context_function = function_with_return(Some(&unit), None);
        bytes_field(&mut context_function, 13, &context_parameter);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&context_function, None),
        );
        assert_eq!(
            exact_error(&bytes),
            (0, "context parameter references absent type 9".to_string())
        );

        let mut annotation = Vec::new();
        int_field(&mut annotation, 1, 99);
        let mut annotated_function = function_with_return(Some(&unit), None);
        bytes_field(&mut annotated_function, 170, &annotation);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&annotated_function, None),
        );
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "annotation references absent qualified name 99".to_string()
            )
        );
    }

    #[test]
    fn contract_table_is_isolated_from_the_enclosing_signature_table() {
        let (strings, qnames) = kotlin_types(&["Unit", "String"]);
        let package_table = type_table(&[class_type(1)], None);
        let contract_table = type_table(&[class_type(2)], None);
        let mut expression = Vec::new();
        int_field(&mut expression, 5, 0);
        let mut effect = Vec::new();
        bytes_field(&mut effect, 3, &expression);
        let mut contract = Vec::new();
        bytes_field(&mut contract, 1, &effect);
        let mut function = function_with_return(None, Some(0));
        bytes_field(&mut function, 30, &contract_table);
        bytes_field(&mut function, 32, &contract);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&function, Some(&package_table)),
        );

        let package = parse_package_fragment_checked(&bytes).expect("valid isolated tables");
        assert_eq!(package.functions[0].ret.internal(), Some("kotlin/Unit"));
    }

    #[test]
    fn conflicting_type_forms_invalid_qname_kind_and_variance_are_exact_errors() {
        let (strings, qnames) = kotlin_types(&["Unit"]);
        let unit = class_type(1);
        let table = type_table(std::slice::from_ref(&unit), None);
        let bytes = fragment(
            &strings,
            &qnames,
            &package_with_function(&function_with_return(Some(&unit), Some(0)), Some(&table)),
        );
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "function return type has conflicting inline and table type forms".to_string()
            )
        );

        let invalid_qnames = vec![qname(1, None, 3)];
        let bytes = fragment(
            &["f", "Broken"],
            &invalid_qnames,
            &package_with_function(&function_with_return(Some(&class_type(0)), None), None),
        );
        assert_eq!(
            exact_error(&bytes),
            (0, "qualified-name entry 0 has invalid kind 3".to_string())
        );

        let mut parameter = Vec::new();
        int_field(&mut parameter, 1, 0);
        int_field(&mut parameter, 2, 1);
        int_field(&mut parameter, 4, 3);
        let mut function = function_with_return(Some(&unit), None);
        bytes_field(&mut function, 4, &parameter);
        let bytes = fragment(&strings, &qnames, &package_with_function(&function, None));
        assert_eq!(
            exact_error(&bytes),
            (
                0,
                "invalid type-parameter variance 3 in function declaration".to_string()
            )
        );
    }
}
