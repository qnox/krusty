//! Public declaration identities stored in a KLIB serialized-IR signature table.
//!
//! Kotlin serializes a public declaration as `IdSignature.public_sig`, containing a
//! `CommonIdSignature`. Package and declaration names are paths of string-table indices, not one
//! spelling to split and later re-intern. This module preserves those paths structurally and keeps
//! the member id and semantic mask in the identity used to join metadata declarations to IR.

use super::decode::{field, require_wire, Cursor, PackageFragmentDecodeError};

/// One exact qualified path in a public KLIB signature.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibNamePath {
    segments: Box<[String]>,
}

impl KlibNamePath {
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

/// Kotlin's public `CommonIdSignature`, before any provider or target representation.
///
/// Both qualified paths, the member id, and the semantic mask participate in equality and hashing.
/// A member-id collision therefore cannot alias two declarations with different exact paths.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibPublicIdSignature {
    package: KlibNamePath,
    declaration: KlibNamePath,
    member_id: Option<u64>,
    mask: u64,
}

impl KlibPublicIdSignature {
    pub fn package(&self) -> &KlibNamePath {
        &self.package
    }

    pub fn declaration(&self) -> &KlibNamePath {
        &self.declaration
    }

    pub fn member_id(&self) -> Option<u64> {
        self.member_id
    }

    pub fn mask(&self) -> u64 {
        self.mask
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibIdSignatureDecodeError {
    offset: usize,
    detail: String,
}

impl KlibIdSignatureDecodeError {
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for KlibIdSignatureDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} at byte {}", self.detail, self.offset)
    }
}

impl std::error::Error for KlibIdSignatureDecodeError {}

impl From<PackageFragmentDecodeError> for KlibIdSignatureDecodeError {
    fn from(error: PackageFragmentDecodeError) -> Self {
        let (offset, detail) = error.into_parts();
        Self { offset, detail }
    }
}

#[derive(Clone, Copy)]
struct StringIndex {
    value: u64,
    offset: usize,
}

fn signature_error(cursor: &Cursor<'_>, detail: impl Into<String>) -> KlibIdSignatureDecodeError {
    KlibIdSignatureDecodeError {
        offset: cursor.position(),
        detail: detail.into(),
    }
}

fn push_string_index(
    indices: &mut Vec<StringIndex>,
    cursor: &mut Cursor<'_>,
    wire: u64,
    context: &str,
) -> Result<(), KlibIdSignatureDecodeError> {
    match wire {
        0 => {
            let offset = cursor.position();
            indices.push(StringIndex {
                value: cursor.varint(context)?,
                offset,
            });
        }
        2 => {
            let (bytes, base) = cursor.length_delimited(context)?;
            let mut packed = Cursor::new(bytes, base);
            while !packed.at_end() {
                let offset = packed.position();
                indices.push(StringIndex {
                    value: packed.varint(context)?,
                    offset,
                });
            }
        }
        _ => {
            return Err(signature_error(
                cursor,
                format!("field in {context} has wire type {wire}, expected 0 or 2"),
            ));
        }
    }
    Ok(())
}

fn path(
    indices: Vec<StringIndex>,
    strings: &[String],
    context: &str,
) -> Result<KlibNamePath, KlibIdSignatureDecodeError> {
    let mut segments = Vec::with_capacity(indices.len());
    for index in indices {
        if index.value > i32::MAX as u64 {
            return Err(KlibIdSignatureDecodeError {
                offset: index.offset,
                detail: format!(
                    "{context} string index {} is outside the non-negative int32 range",
                    index.value
                ),
            });
        }
        let value = usize::try_from(index.value).map_err(|_| KlibIdSignatureDecodeError {
            offset: index.offset,
            detail: format!(
                "{context} string index {} exceeds the host range",
                index.value
            ),
        })?;
        let segment = strings
            .get(value)
            .cloned()
            .ok_or_else(|| KlibIdSignatureDecodeError {
                offset: index.offset,
                detail: format!("{context} references absent string {value}"),
            })?;
        segments.push(segment);
    }
    Ok(KlibNamePath {
        segments: segments.into_boxed_slice(),
    })
}

fn decode_common_signature(
    bytes: &[u8],
    base: usize,
    strings: &[String],
) -> Result<KlibPublicIdSignature, KlibIdSignatureDecodeError> {
    let mut cursor = Cursor::new(bytes, base);
    let mut package = Vec::new();
    let mut declaration = Vec::new();
    let mut old_member_id = None;
    let mut member_id = None;
    let mut mask = None;
    let mut debug_info = None;
    while !cursor.at_end() {
        let (field, wire) = field(&mut cursor, "CommonIdSignature")?;
        match field {
            1 => push_string_index(&mut package, &mut cursor, wire, "package path")?,
            2 => push_string_index(&mut declaration, &mut cursor, wire, "declaration path")?,
            3 => {
                require_wire(&cursor, wire, 0, "pre-2.4 member id")?;
                if old_member_id
                    .replace(cursor.varint("pre-2.4 member id")?)
                    .is_some()
                {
                    return Err(signature_error(&cursor, "duplicate pre-2.4 member id"));
                }
            }
            4 => {
                require_wire(&cursor, wire, 0, "signature mask")?;
                if mask.replace(cursor.varint("signature mask")?).is_some() {
                    return Err(signature_error(&cursor, "duplicate signature mask"));
                }
            }
            5 => {
                require_wire(&cursor, wire, 0, "signature debug-info id")?;
                if debug_info
                    .replace(cursor.varint("signature debug-info id")?)
                    .is_some()
                {
                    return Err(signature_error(
                        &cursor,
                        "duplicate signature debug-info id",
                    ));
                }
            }
            6 => {
                require_wire(&cursor, wire, 1, "member id")?;
                if member_id.replace(cursor.fixed64("member id")?).is_some() {
                    return Err(signature_error(&cursor, "duplicate member id"));
                }
            }
            _ => cursor.skip(wire, "CommonIdSignature field")?,
        }
    }
    // Kotlin 2.4 moved the id from int64 field 3 to fixed64 field 6. Its authoritative
    // deserializer chooses the current field when both occur and consults the old one only when
    // field 6 is absent.
    let member_id = member_id.or(old_member_id);
    Ok(KlibPublicIdSignature {
        package: path(package, strings, "package path")?,
        declaration: path(declaration, strings, "declaration path")?,
        member_id,
        mask: mask.unwrap_or(0),
    })
}

/// Decode an `IdSignature` table entry when it is Kotlin's public `CommonIdSignature` form.
///
/// `Ok(None)` means the entry contains exactly one well-framed non-public signature kind. Any
/// missing, duplicated, truncated, or ill-typed identity is an error; callers must not
/// recover by matching declaration spellings or parameter tuples.
pub fn decode_public_id_signature(
    bytes: &[u8],
    strings: &[String],
) -> Result<Option<KlibPublicIdSignature>, KlibIdSignatureDecodeError> {
    let mut cursor = Cursor::new(bytes, 0);
    let mut public = None;
    let mut kind = None;
    while !cursor.at_end() {
        let (field, wire) = field(&mut cursor, "IdSignature")?;
        if (1..=7).contains(&field) {
            if kind.replace(field).is_some() {
                return Err(signature_error(
                    &cursor,
                    "IdSignature contains more than one identity kind",
                ));
            }
            let expected = if field == 4 { 0 } else { 2 };
            require_wire(&cursor, wire, expected, "IdSignature kind")?;
            if field == 1 {
                let (bytes, base) = cursor.length_delimited("public IdSignature")?;
                public = Some(decode_common_signature(bytes, base, strings)?);
            } else if wire == 0 {
                cursor.varint("non-public IdSignature")?;
            } else {
                cursor.length_delimited("non-public IdSignature")?;
            }
        } else {
            cursor.skip(wire, "IdSignature field")?;
        }
    }
    if kind.is_none() {
        return Err(KlibIdSignatureDecodeError {
            offset: 0,
            detail: "IdSignature has no identity kind".to_string(),
        });
    }
    Ok(public)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            bytes.push(if value == 0 { byte } else { byte | 0x80 });
            if value == 0 {
                return bytes;
            }
        }
    }

    fn varint_field(into: &mut Vec<u8>, field: u64, value: u64) {
        into.extend(varint(field << 3));
        into.extend(varint(value));
    }

    fn bytes_field(into: &mut Vec<u8>, field: u64, value: &[u8]) {
        into.extend(varint(field << 3 | 2));
        into.extend(varint(value.len() as u64));
        into.extend(value);
    }

    fn fixed64_field(into: &mut Vec<u8>, field: u64, value: u64) {
        into.extend(varint(field << 3 | 1));
        into.extend(value.to_le_bytes());
    }

    fn public_signature(
        package: &[u64],
        declaration: &[u64],
        member_id: u64,
        mask: u64,
    ) -> Vec<u8> {
        let mut common = Vec::new();
        let package = package
            .iter()
            .flat_map(|index| varint(*index))
            .collect::<Vec<_>>();
        bytes_field(&mut common, 1, &package);
        for index in declaration {
            varint_field(&mut common, 2, *index);
        }
        fixed64_field(&mut common, 6, member_id);
        varint_field(&mut common, 4, mask);
        let mut signature = Vec::new();
        bytes_field(&mut signature, 1, &common);
        signature
    }

    #[test]
    fn public_signature_preserves_exact_segmented_identity() {
        let strings = ["fixture", "api", "Widget", "evaluate"]
            .map(str::to_string)
            .to_vec();
        let bytes = public_signature(&[0, 1], &[2, 3], 0xfedc_ba98_7654_3210, 5);
        let signature = decode_public_id_signature(&bytes, &strings)
            .expect("valid public signature")
            .expect("public identity");

        assert_eq!(signature.package().segments(), ["fixture", "api"]);
        assert_eq!(signature.declaration().segments(), ["Widget", "evaluate"]);
        assert_eq!(signature.member_id(), Some(0xfedc_ba98_7654_3210));
        assert_eq!(signature.mask(), 5);
    }

    #[test]
    fn colliding_member_ids_do_not_alias_distinct_qualified_declarations() {
        let strings = ["alpha_fixture", "beta_fixture", "Node"]
            .map(str::to_string)
            .to_vec();
        let first = decode_public_id_signature(&public_signature(&[0], &[2], 0x55, 0), &strings)
            .expect("valid first signature")
            .expect("public first signature");
        let second = decode_public_id_signature(&public_signature(&[1], &[2], 0x55, 0), &strings)
            .expect("valid second signature")
            .expect("public second signature");

        assert_ne!(first, second);
        assert_eq!(HashSet::from([first, second]).len(), 2);
    }

    #[test]
    fn malformed_public_signatures_report_exact_failures() {
        let strings = ["fixture".to_string()];

        let missing = public_signature(&[0], &[3], 7, 0);
        let error = decode_public_id_signature(&missing, &strings).expect_err("absent string");
        assert_eq!(error.offset(), 6);
        assert_eq!(
            error.detail(),
            "declaration path references absent string 3"
        );

        let outside_int32 = public_signature(&[0], &[i32::MAX as u64 + 1], 7, 0);
        let error = decode_public_id_signature(&outside_int32, &strings)
            .expect_err("out-of-range string index");
        assert_eq!(error.offset(), 6);
        assert_eq!(
            error.detail(),
            "declaration path string index 2147483648 is outside the non-negative int32 range"
        );

        let truncated = vec![0x0a, 0x09, 0x31];
        let error =
            decode_public_id_signature(&truncated, &strings).expect_err("truncated public payload");
        assert_eq!(error.offset(), 2);
        assert_eq!(error.detail(), "truncated public IdSignature");
    }

    #[test]
    fn current_member_id_takes_precedence_over_the_pre_2_4_field() {
        let mut common = Vec::new();
        varint_field(&mut common, 3, 7);
        fixed64_field(&mut common, 6, 8);
        let mut bytes = Vec::new();
        bytes_field(&mut bytes, 1, &common);

        let signature = decode_public_id_signature(&bytes, &[])
            .expect("both schema generations are well formed")
            .expect("public signature");
        assert_eq!(signature.member_id(), Some(8));
    }

    #[test]
    fn non_public_and_ambiguous_identity_kinds_are_distinct() {
        let strings = Vec::new();
        let mut private = Vec::new();
        bytes_field(&mut private, 2, &[]);
        assert_eq!(
            decode_public_id_signature(&private, &strings).expect("well-framed private identity"),
            None
        );

        let mut ambiguous = private;
        varint_field(&mut ambiguous, 4, 1);
        let error = decode_public_id_signature(&ambiguous, &strings)
            .expect_err("oneof collision must not pick a spelling path");
        assert_eq!(
            error.detail(),
            "IdSignature contains more than one identity kind"
        );
    }
}
