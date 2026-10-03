//! `@SinceKotlin` on a decoded declaration. Kotlinc publishes the version as an annotation
//! argument and drops the callable from resolution when it is newer than `-api-version`.

use crate::language_version::LanguageVersion;
use crate::metadata::decode::Pb;
use crate::types::{type_name, TypeName};

use super::string_table::{resolve_class_name, resolve_string, Rec};

pub(super) fn annotation_names(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
) -> Vec<TypeName> {
    bodies
        .iter()
        .filter_map(|body| {
            let mut pb = Pb::new(body);
            let mut id = None;
            while !pb.at_end() {
                let tag = pb.varint()?;
                match (tag >> 3, tag & 7) {
                    (1, 0) => id = pb.varint(),
                    (_, wire) => pb.skip(wire)?,
                }
            }
            id.and_then(|id| resolve_class_name(records, d2, id as usize))
                .map(|name| type_name(&name))
        })
        .collect()
}

/// The `version` argument of `@kotlin.SinceKotlin`, when this declaration carries that annotation.
pub(super) fn version(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
) -> Option<LanguageVersion> {
    for body in bodies {
        let mut pb = Pb::new(body);
        let mut id = None;
        let mut since = None;
        while !pb.at_end() {
            let Some(tag) = pb.varint() else {
                break;
            };
            match (tag >> 3, tag & 7) {
                (1, 0) => id = pb.varint(),
                (2, 2) => {
                    let Some(len) = pb.varint() else { break };
                    let Some(argument) = pb.bytes(len as usize) else {
                        break;
                    };
                    if let Some(text) = version_argument(argument, records, d2) {
                        since = LanguageVersion::from_major_minor_text(&text);
                    }
                }
                (_, wire) => {
                    if pb.skip(wire).is_none() {
                        break;
                    }
                }
            }
        }
        let is_since = id
            .and_then(|id| resolve_class_name(records, d2, id as usize))
            .as_deref()
            == Some("kotlin/SinceKotlin");
        if is_since {
            return since;
        }
    }
    None
}

/// `Argument { name_id = 1, value = 2: Value { string_value = 5 } }` when the name is `version`.
fn version_argument(argument: &[u8], records: &[Rec], d2: &[String]) -> Option<String> {
    let mut pb = Pb::new(argument);
    let mut name = None;
    let mut value = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                let id = pb.varint()?;
                name = resolve_string(records, d2, id as usize);
            }
            (2, 2) => {
                let len = pb.varint()? as usize;
                let body = pb.bytes(len)?;
                value = string_value(body);
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    (name.as_deref() == Some("version"))
        .then(|| value.and_then(|id| resolve_string(records, d2, id as usize)))
        .flatten()
}

fn string_value(body: &[u8]) -> Option<u64> {
    let mut pb = Pb::new(body);
    let mut value = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (5, 0) => value = pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    value
}
