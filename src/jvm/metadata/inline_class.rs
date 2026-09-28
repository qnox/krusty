//! A classpath value class's underlying property and type, decoded from its `@Metadata`.

use super::*;

/// A classpath value class's underlying property and Kotlin type decoded from `@Metadata`.
#[derive(Clone, Debug)]
pub struct InlineClass {
    /// Underlying class name, or `None` when a type parameter erases to `Object`.
    pub underlying_class: Option<String>,
    /// Whether the declared underlying type admits `null`: a nullable type, or a type parameter
    /// whose bound does. `None` when metadata omitted the type shape.
    pub underlying_nullable: Option<bool>,
    /// The sole property's name (`data` for `UInt`/`Result`).
    pub property_name: Option<String>,
}

/// If `ctx` is a Kotlin `@JvmInline value class`, its decoded [`InlineClass`] (presence of the
/// `inline_class_underlying_type` proto field is the marker); `None` for an ordinary class.
pub(super) fn inline_class(ctx: &MetaCtx) -> Option<InlineClass> {
    let mut pb = Pb::new(ctx.msg);
    let mut is_value = false;
    let mut property_name = None;
    let mut underlying: Option<(&[u8], bool)> = None;
    let mut underlying_type_id: Option<u64> = None;
    let mut type_table: Option<&[u8]> = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (17, 0) => {
                // inline_class_underlying_property_name (name id in table)
                let id = pb.varint()?;
                is_value = true;
                property_name = resolve_string(ctx.records, ctx.d2, id as usize);
            }
            (18, 2) => {
                // inline_class_underlying_type (inline Type message)
                let n = pb.varint()? as usize;
                underlying = Some((pb.bytes(n)?, false));
                is_value = true;
            }
            (19, 0) => {
                // inline_class_underlying_type_id (type id in the class's TypeTable) — marks a value
                // class even when the type isn't inlined; resolved from the table after the loop.
                underlying_type_id = pb.varint();
                is_value = true;
            }
            (30, 2) => {
                // Class.typeTable — holds the referenced `Type`s when the compiler shares them by id.
                let n = pb.varint()? as usize;
                type_table = pb.bytes(n);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    if !is_value {
        return None;
    }
    // A table-carried underlying type (field 19): a type at `index >= firstNullable` is nullable
    // even without its own `nullable` flag (the table's nullability-sharing optimization).
    if underlying.is_none() {
        underlying = underlying_type_id
            .zip(type_table)
            .and_then(|(id, table)| type_table_entry(table, id as usize));
    }
    // When BOTH the inline type (18) and the table id (19) are absent, the underlying type is the
    // declared type of the underlying PROPERTY (field 17 names it; `Class.property` = field 10
    // carries it) — kotlinc omits the class-level copy as derivable.
    if underlying.is_none() {
        underlying = property_name
            .as_deref()
            .and_then(|name| property_return_type(ctx, name, type_table));
    }
    let (underlying_class, underlying_nullable) = match underlying {
        Some((body, table_nullable)) => {
            let (class, nullable) = parse_type_class_and_nullable(body);
            let admits_null = nullable
                || table_nullable
                || type_parameter_id(body)
                    .is_some_and(|id| type_parameter_admits_null(ctx, type_table, id, 0));
            (
                class.and_then(|id| resolve_class_name(ctx.records, ctx.d2, id as usize)),
                Some(admits_null),
            )
        }
        None => (None, None),
    };
    Some(InlineClass {
        underlying_class,
        underlying_nullable,
        property_name,
    })
}

/// The declared return type of the class property named `name`, with its table nullability.
/// `Property.returnType` = 3 (inline `Type`) or `returnTypeId` = 9 (a TypeTable id; 7 is the
/// RECEIVER type id).
fn property_return_type<'a>(
    ctx: &MetaCtx<'a>,
    name: &str,
    type_table: Option<&'a [u8]>,
) -> Option<(&'a [u8], bool)> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (10, 2) => {
                let n = pb.varint()?;
                let property = pb.bytes(n as usize)?;
                let Some((name_id, inline, table_id)) = parse_property_name_and_return(property)
                else {
                    continue;
                };
                if resolve_string(ctx.records, ctx.d2, name_id as usize).as_deref() != Some(name) {
                    continue;
                }
                return match inline {
                    Some(body) => Some((body, false)),
                    None => type_table_entry(type_table?, table_id? as usize),
                };
            }
            (_, w) => pb.skip(w)?,
        }
    }
    None
}

/// Whether the class type parameter `id` admits `null`: it has no upper bound (`Any?`), or one of
/// its bounds does. kotlinc's `isNullableType` reads a type parameter this way.
fn type_parameter_admits_null(
    ctx: &MetaCtx,
    type_table: Option<&[u8]>,
    id: u64,
    depth: u32,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Some(parameter) = type_param_bodies(ctx.msg, CLASS_TYPE_PARAMETER_FIELD)
        .into_iter()
        .filter_map(|body| parse_type_param(body).ok())
        .find(|parameter| parameter.id == id)
    else {
        return false;
    };
    let inline = parameter
        .upper_bound_bodies
        .iter()
        .map(|body| (body.as_slice(), false));
    let indexed = parameter
        .upper_bound_ids
        .iter()
        .filter_map(|&bound| type_table_entry(type_table?, bound as usize));
    let bounds = inline.chain(indexed).collect::<Vec<_>>();
    bounds.is_empty()
        || bounds.into_iter().any(|(body, table_nullable)| {
            table_nullable
                || parse_type_class_and_nullable(body).1
                || type_parameter_id(body).is_some_and(|bound| {
                    type_parameter_admits_null(ctx, type_table, bound, depth + 1)
                })
        })
}

/// A `Type` message's `type_parameter` (field 7): the id of the type parameter it names.
fn type_parameter_id(body: &[u8]) -> Option<u64> {
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (7, 0) => return pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    None
}

/// A property's decoded `(name id, inline returnType body, returnTypeId)`.
type PropNameAndReturn<'a> = (u64, Option<&'a [u8]>, Option<u64>);

/// A `Property` message's `name` (field 2, string id), inline `returnType` (field 3), and
/// `returnTypeId` (field 9, a TypeTable index — field 7 is the RECEIVER type id, unlike `Function`).
fn parse_property_name_and_return(body: &[u8]) -> Option<PropNameAndReturn<'_>> {
    let mut pb = Pb::new(body);
    let mut name = None;
    let mut rt: Option<&[u8]> = None;
    let mut rtid = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name = pb.varint(),
            (3, 2) => {
                let n = pb.varint()? as usize;
                rt = pb.bytes(n);
            }
            (9, 0) => rtid = pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    Some((name?, rt, rtid))
}
