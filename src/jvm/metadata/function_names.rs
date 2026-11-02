//! The names a `Function` message declares, read without decoding its types.
//!
//! A package's member index keys every facade static by its Kotlin source name. That needs only
//! each function's Kotlin name, its JVM name, and its visibility, so the index reads those three
//! facts here and leaves the signatures packed until a lookup selects the function.

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

/// The names of every `Function` (proto field `fn_field`) of the message, in declaration order. A
/// function whose name is not in `d2` is skipped, as [`super::decode_functions`] skips it.
pub(super) fn function_names(ctx: &MetaCtx, fn_field: u64) -> MetadataResult<Vec<FunctionName>> {
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
