//! JVM semantic adaptation for checked KLIB metadata fragments.
//!
//! Wire/schema validation belongs to [`crate::metadata::decode`]. This module retains the legacy
//! `Builtin*` semantic adapter until its declarations move to the common metadata model.

use super::{resolve_qname, BuiltinPackage};
use crate::metadata::decode::{
    decode_package_fragment, field, require_wire, Cursor, DecodedPackageFragment, QName,
};

pub(crate) use crate::metadata::decode::{parse_module_header, PackageFragmentDecodeError};

mod semantic;

#[derive(Default)]
pub(super) struct SemanticInventory {
    package_functions: usize,
    classes: std::collections::BTreeMap<String, ClassInventory>,
}

#[derive(Default)]
struct ClassInventory {
    supertypes: usize,
    type_parameters: usize,
    constructors: usize,
    members: usize,
}

fn count_fields(body: &[u8], fields: &[u64]) -> Result<usize, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut count = 0;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "validated declaration")?;
        if fields.contains(&number) {
            count += 1;
        }
        cursor.skip(wire, "validated declaration")?;
    }
    Ok(count)
}

fn class_inventory(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(String, ClassInventory), PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut fq_name = None;
    let mut inventory = ClassInventory::default();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "class declaration")?;
        match (number, wire) {
            (2, 0) => {
                cursor.varint("class supertype id")?;
                inventory.supertypes += 1;
            }
            (2, 2) => {
                let (packed, base) = cursor.length_delimited("class supertype ids")?;
                let mut packed = Cursor::new(packed, base);
                while !packed.at_end() {
                    packed.varint("class supertype id")?;
                    inventory.supertypes += 1;
                }
            }
            (3, 0) => fq_name = Some(cursor.varint("class qualified-name id")?),
            (5, 2) => {
                cursor.length_delimited("class type parameter")?;
                inventory.type_parameters += 1;
            }
            (6, 2) => {
                cursor.length_delimited("class supertype")?;
                inventory.supertypes += 1;
            }
            (8, 2) => {
                cursor.length_delimited("class constructor")?;
                inventory.constructors += 1;
            }
            (9 | 10, 2) => {
                cursor.length_delimited("class member")?;
                inventory.members += 1;
            }
            (_, wire) => cursor.skip(wire, "class declaration")?,
        }
    }
    let fq_name = fq_name.ok_or_else(|| PackageFragmentDecodeError {
        offset: 0,
        detail: "class declaration is missing required field 3".to_string(),
    })?;
    let fq_name_index = usize::try_from(fq_name).map_err(|_| PackageFragmentDecodeError {
        offset: 0,
        detail: format!("class qualified name {fq_name} exceeds the host index range"),
    })?;
    let fq_name = qnames
        .get(fq_name_index)
        .map(|_| resolve_qname(qnames, strings, fq_name_index as i64))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| PackageFragmentDecodeError {
            offset: 0,
            detail: format!("class references absent qualified name {fq_name}"),
        })?;
    Ok((fq_name, inventory))
}

pub(super) fn semantic_inventory(
    decoded: &DecodedPackageFragment<'_>,
) -> Result<SemanticInventory, PackageFragmentDecodeError> {
    let mut inventory = SemanticInventory {
        package_functions: decoded
            .package
            .map_or(Ok(0), |body| count_fields(body, &[3]))?,
        classes: std::collections::BTreeMap::new(),
    };
    for body in &decoded.classes {
        let (name, class) = class_inventory(body, &decoded.strings, &decoded.qnames)?;
        if inventory.classes.insert(name.clone(), class).is_some() {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!("duplicate class identity {name}"),
            });
        }
    }
    Ok(inventory)
}

pub(super) fn verify_semantic_inventory(
    expected: &SemanticInventory,
    package: &BuiltinPackage,
) -> Result<(), PackageFragmentDecodeError> {
    if package.functions.len() != expected.package_functions {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: format!(
                "package declared {} functions but {} decoded completely",
                expected.package_functions,
                package.functions.len()
            ),
        });
    }
    if package.classes.len() != expected.classes.len() {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: format!(
                "package fragment declared {} classes but {} decoded completely",
                expected.classes.len(),
                package.classes.len()
            ),
        });
    }
    for (name, expected) in &expected.classes {
        let actual = package
            .classes
            .get(name)
            .ok_or_else(|| PackageFragmentDecodeError {
                offset: 0,
                detail: format!("class {name} did not decode completely"),
            })?;
        if actual.supertype_tys.len() != expected.supertypes
            || actual.type_params.len() != expected.type_parameters
            || actual.constructors.len() != expected.constructors
            || actual.members.len() != expected.members
        {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!(
                    "class {name} contains a declaration that did not decode completely"
                ),
            });
        }
    }
    Ok(())
}

/// Decode one dependency-owned fragment atomically. Structural and semantic parsing are both
/// fallible: a nested declaration is published only after all of its names, types, parameters and
/// table references have resolved successfully.
pub(crate) fn parse_package_fragment_checked(
    bytes: &[u8],
) -> Result<BuiltinPackage, PackageFragmentDecodeError> {
    semantic::parse(decode_package_fragment(bytes)?)
}
