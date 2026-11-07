//! The names a package's `Function` and `Property` messages declare, read without decoding their
//! types.
//!
//! A package's member index keys every facade static by its Kotlin source name, and a lookup of one
//! name skips every facade that declares nothing under it. Both need only the names (and, for a
//! function, its JVM name and visibility), so they read those here and leave the signatures packed
//! until a lookup selects a declaration.

use super::{
    annotation_jvm_name, parse_function, resolve_string, MetaCtx, MetadataResult, ParsedFunction,
    Pb, Rec,
};
use crate::types::Visibility;

/// One function's Kotlin source name, bytecode name, and declared visibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionName {
    pub kotlin_name: String,
    pub jvm_name: String,
    pub visibility: Visibility,
}

/// The names a package declares: `Package.function` (field 3) and `Package.property` (field 4).
#[derive(Debug, Default)]
pub struct PackageMemberNames {
    pub functions: Vec<FunctionName>,
    pub properties: Vec<String>,
}

pub(super) fn package_member_names(ctx: &MetaCtx) -> MetadataResult<PackageMemberNames> {
    Ok(PackageMemberNames {
        functions: function_names(ctx, 3)?,
        properties: property_names(ctx, 4),
    })
}

/// The names of every `Property` (proto field `property_field`) of the message, in declaration
/// order. A property whose name does not resolve is skipped, as the property decoder skips it.
fn property_names(ctx: &MetaCtx, property_field: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (field, 2) if field == property_field => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(name) = property_name_id(body)
                    .and_then(|id| resolve_string(ctx.records, ctx.d2, id as usize))
                {
                    out.push(name);
                }
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    out
}

/// `Property.name` (field 2).
fn property_name_id(body: &[u8]) -> Option<u64> {
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => return pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    None
}

/// The names of every `Function` (proto field `fn_field`) of the message, in declaration order. A
/// function whose name is not in `d2` is skipped, as [`super::decode_functions`] skips it.
fn function_names(ctx: &MetaCtx, fn_field: u64) -> MetadataResult<Vec<FunctionName>> {
    let mut out = Vec::new();
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (field, 2) if field == fn_field => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                let function = parse_function(body)?;
                let Some(kotlin_name) = ctx.d2.get(function.name_id as usize).cloned() else {
                    continue;
                };
                let (jvm_name, _) = function_jvm_name(&function, &kotlin_name, ctx.records, ctx.d2);
                out.push(FunctionName {
                    kotlin_name,
                    jvm_name,
                    visibility: function.visibility,
                });
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// The bytecode name and descriptor of a function named `kotlin_name`. The `JvmMethodSignature`
/// name and descriptor are plain string-table entries, resolved as kotlinc's `getString` resolves
/// them, never as class names. A `@kotlin.jvm.JvmName("...")` annotation is the authoritative
/// bytecode name (kotlinc emits the method under it), and the signature extension may omit it.
pub(super) fn function_jvm_name(
    function: &ParsedFunction,
    kotlin_name: &str,
    records: &[Rec],
    d2: &[String],
) -> (String, Option<String>) {
    let (jvm_name, jvm_desc) = match &function.jvm_sig {
        Some(signature) => (
            signature
                .name_id
                .and_then(|id| resolve_string(records, d2, id as usize))
                .unwrap_or_else(|| kotlin_name.to_owned()),
            signature
                .desc_id
                .and_then(|id| resolve_string(records, d2, id as usize)),
        ),
        None => (kotlin_name.to_owned(), None),
    };
    let jvm_name =
        annotation_jvm_name(&function.annotation_bodies, records, d2).unwrap_or(jvm_name);
    (jvm_name, jvm_desc)
}
