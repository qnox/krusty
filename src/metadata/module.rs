//! Emit the `META-INF/<name>.kotlin_module` file. kotlinc needs this to discover which file-facade
//! class holds a package's top-level declarations (without it, a Kotlin consumer can't resolve
//! `demo.greet` even though the facade carries correct `@Metadata`).
//!
//! Format (decoded from kotlinc 1.9.24): a header of int32s `[len=3, 1, 9, 0, flags=0]` (the
//! metadata version + a flags word), then a `JvmModuleProtoBuf.Module` protobuf:
//!   Module { package_parts = field 1 (repeated) }
//!   PackageParts { package_fq_name = field 1, short_class_name = field 2 (repeated) }

use crate::metadata::protobuf::Pb;

/// `packages`: `(package fq-name, [file-facade short class names])`.
pub fn build_kotlin_module(packages: &[(String, Vec<String>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [3i32, 1, 9, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes()); // version [1,9,0] length-prefixed + flags=0
    }
    let mut module = Pb::new();
    for (pkg, facades) in packages {
        let mut pp = Pb::new();
        pp.field_bytes(1, pkg.as_bytes()); // package_fq_name
        for f in facades {
            pp.field_bytes(2, f.as_bytes()); // short_class_name
        }
        module.repeated_message(1, &pp); // Module.package_parts
    }
    // Trailing empty fields kotlinc always emits (metadata_parts / string table placeholders).
    module.field_bytes(4, &[]);
    module.field_bytes(5, &[]);
    out.extend_from_slice(module.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed 20-byte header (`[len=3,1,9,0, flags=0]` as big-endian int32s) every module file opens
    /// with, followed immediately by the `Module` protobuf.
    const HEADER: &[u8] = &[
        0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn matches_kotlinc_reference_module() {
        // Exact 40 bytes kotlinc 1.9.24 writes for `package demo` with facade `Lib1Kt`.
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x0e, 0x0a, 0x04, 0x64, 0x65, 0x6d, 0x6f,
            0x12, 0x06, 0x4c, 0x69, 0x62, 0x31, 0x4b, 0x74, 0x22, 0x00, 0x2a, 0x00,
        ];
        let got = build_kotlin_module(&[("demo".into(), vec!["Lib1Kt".into()])]);
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }

    #[test]
    fn empty_package_list_is_header_plus_two_trailing_empty_fields() {
        // No packages: just the header, then the two empty trailing fields kotlinc always writes
        // (field 4 / field 5, both zero-length): tags 0x22, 0x2a with a length of 0.
        let got = build_kotlin_module(&[]);
        let mut expected = HEADER.to_vec();
        expected.extend_from_slice(&[0x22, 0x00, 0x2a, 0x00]);
        assert_eq!(got, expected, "\n got: {got:02x?}");
        assert_eq!(got.len(), 24);
    }

    #[test]
    fn header_prefix_is_always_emitted() {
        // Every variant opens with the same 20-byte header regardless of contents.
        let got = build_kotlin_module(&[("a".into(), vec!["AKt".into()])]);
        assert_eq!(&got[..20], HEADER);
    }

    #[test]
    fn multiple_facades_in_one_package_are_repeated_short_class_name_fields() {
        // `PackageParts { package_fq_name=1, short_class_name=2 (repeated) }` — one field-2 entry per facade.
        let got = build_kotlin_module(&[("p".into(), vec!["Ak".into(), "Bk".into()])]);
        // The PackageParts message body: pkg "p" (0a 01 70) then two facades (12 02 41 6b / 12 02 42 6b).
        let pp: &[u8] = &[
            0x0a, 0x01, 0x70, 0x12, 0x02, 0x41, 0x6b, 0x12, 0x02, 0x42, 0x6b,
        ];
        let mut expected = HEADER.to_vec();
        expected.push(0x0a); // Module.package_parts tag (field 1, wire 2)
        expected.push(pp.len() as u8);
        expected.extend_from_slice(pp);
        expected.extend_from_slice(&[0x22, 0x00, 0x2a, 0x00]); // trailing empties
        assert_eq!(got, expected, "\n got: {got:02x?}");
    }

    #[test]
    fn two_packages_emit_two_package_parts_messages() {
        let got = build_kotlin_module(&[
            ("a".into(), vec!["AKt".into()]),
            ("b".into(), vec!["BKt".into()]),
        ]);
        // Two Module.package_parts entries — the field-1 tag (0x0a) appears at least twice.
        assert!(got.iter().filter(|&&b| b == 0x0a).count() >= 2);
        // Both package short names round-trip as raw bytes.
        assert!(contains(&got, b"AKt"));
        assert!(contains(&got, b"BKt"));
        assert!(got.ends_with(&[0x22, 0x00, 0x2a, 0x00]));
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
