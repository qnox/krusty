//! Checked decoding of a JVM `META-INF/<module>.kotlin_module` file's optional-annotation section.
//!
//! The file is kotlinc's `ModuleMapping`: a big-endian `i32` metadata-version array (length first),
//! a flags word when that version is 1.4 or later, then a `JvmModuleProtoBuf.Module` message. Its
//! `optional_annotation_class` entries (field 16) are `@OptionalExpectation` annotation classes with
//! no JVM actual. They are ordinary `ProtoBuf.Class` messages under the module's own string table
//! (field 4) and qualified-name table (field 5), serialized with the builtins protocol, so they cross
//! the same checked class boundary as a KLIB fragment's classes.

use super::klib::{
    decode_name_tables, field, require_wire, validate_class, ClassAnnotationProtocol, Cursor,
    DecodedPackageFragment, PackageFragmentDecodeError,
};

/// The metadata-version prefix kotlinc's `ModuleMapping.readVersionNumber` accepts: a length no
/// larger than `BinaryVersion.MAX_LENGTH`.
const MAX_VERSION_LENGTH: usize = 1024;

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, PackageFragmentDecodeError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|word| word.first_chunk::<4>())
        .map(|word| i32::from_be_bytes(*word))
        .ok_or_else(|| PackageFragmentDecodeError {
            offset,
            detail: "truncated Kotlin module version header".to_string(),
        })
}

/// The `Module` message following the version header (and, from metadata 1.4, its flags word).
fn module_message(bytes: &[u8]) -> Result<(&[u8], usize), PackageFragmentDecodeError> {
    let length = read_i32(bytes, 0)?;
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length <= MAX_VERSION_LENGTH)
        .ok_or_else(|| PackageFragmentDecodeError {
            offset: 0,
            detail: format!("invalid Kotlin module version length {length}"),
        })?;
    let version = (0..length)
        .map(|index| read_i32(bytes, 4 + 4 * index))
        .collect::<Result<Vec<_>, _>>()?;
    let mut start = 4 + 4 * length;
    // `isKotlin1Dot4OrLater`: since 1.4 an integer flags word follows the version.
    let major = version.first().copied().unwrap_or(0);
    let minor = version.get(1).copied().unwrap_or(0);
    if major > 1 || (major == 1 && minor >= 4) {
        read_i32(bytes, start)?;
        start += 4;
    }
    Ok((&bytes[start..], start))
}

/// Decode the optional annotation classes of one `.kotlin_module` file as a package-fragment-shaped
/// class set over the module's name tables.
pub(crate) fn decode_module_optional_annotations(
    bytes: &[u8],
) -> Result<DecodedPackageFragment<'_>, PackageFragmentDecodeError> {
    let (module, base) = module_message(bytes)?;
    let mut cursor = Cursor::new(module, base);
    let mut strings = None;
    let mut qualified_names = None;
    let mut classes = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "Kotlin module")?;
        match number {
            4 => {
                require_wire(&cursor, wire, 2, "Kotlin module")?;
                if strings.is_some() {
                    return Err(cursor.error("duplicate Kotlin module string table"));
                }
                strings = Some(cursor.length_delimited("Kotlin module string table")?);
            }
            5 => {
                require_wire(&cursor, wire, 2, "Kotlin module")?;
                if qualified_names.is_some() {
                    return Err(cursor.error("duplicate Kotlin module qualified-name table"));
                }
                qualified_names =
                    Some(cursor.length_delimited("Kotlin module qualified-name table")?);
            }
            16 => {
                require_wire(&cursor, wire, 2, "Kotlin module")?;
                let (class, class_base) = cursor.length_delimited("optional annotation class")?;
                validate_class(class, class_base)?;
                classes.push(class);
            }
            _ => cursor.skip(wire, "Kotlin module")?,
        }
    }
    let (strings, qnames) = decode_name_tables(strings, qualified_names)?;
    Ok(DecodedPackageFragment {
        strings,
        qnames,
        package: None,
        classes,
        file_annotations: Vec::new(),
        class_names: Vec::new(),
        class_annotations: ClassAnnotationProtocol::BuiltIns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(version: &[i32], flags: Option<i32>) -> Vec<u8> {
        let mut bytes = (version.len() as i32).to_be_bytes().to_vec();
        for word in version.iter().copied().chain(flags) {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn the_flags_word_follows_a_version_from_kotlin_1_4() {
        let mut bytes = header(&[2, 4, 0], Some(0));
        bytes.extend_from_slice(&[0x22, 0x00]);
        assert_eq!(module_message(&bytes), Ok((&[0x22, 0x00][..], 20)));
        let mut legacy = header(&[1, 3, 0], None);
        legacy.extend_from_slice(&[0x22, 0x00]);
        assert_eq!(module_message(&legacy), Ok((&[0x22, 0x00][..], 16)));
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        assert_eq!(
            module_message(&header(&[2, 4, 0], None)),
            Err(PackageFragmentDecodeError {
                offset: 16,
                detail: "truncated Kotlin module version header".to_string(),
            })
        );
        assert_eq!(
            module_message(&(-1i32).to_be_bytes()),
            Err(PackageFragmentDecodeError {
                offset: 0,
                detail: "invalid Kotlin module version length -1".to_string(),
            })
        );
    }

    #[test]
    fn a_module_without_optional_annotations_decodes_to_no_classes() {
        let mut bytes = header(&[2, 4, 0], Some(0));
        bytes.extend_from_slice(&[0x22, 0x00, 0x2a, 0x00]);
        let decoded = decode_module_optional_annotations(&bytes).expect("empty module");
        assert!(decoded.classes.is_empty());
        assert!(decoded.strings.is_empty());
    }
}
