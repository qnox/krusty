//! Emit the `META-INF/<name>.kotlin_module` file. kotlinc needs this to discover which file-facade
//! class holds a package's top-level declarations (without it, a Kotlin consumer can't resolve
//! `demo.greet` even though the facade carries correct `@Metadata`).
//!
//! Format (decoded from kotlinc 2.4.0): a header of int32s `[len=3, 2, 4, 0, flags=0]` (the
//! metadata version + a flags word), then a `JvmModuleProtoBuf.Module` protobuf:
//!   Module { package_parts = field 1 (repeated) }
//!   PackageParts { package_fq_name = field 1, short_class_name = field 2 (repeated) }

use crate::metadata::protobuf::Pb;

/// `packages`: `(package fq-name, [file-facade short class names])`.
pub fn build_kotlin_module(packages: &[(String, Vec<String>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [3i32, 2, 4, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes()); // version [2,4,0] length-prefixed + flags=0
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

    #[test]
    fn matches_kotlinc_reference_module() {
        // Exact 40 bytes kotlinc 2.4.0 writes for `package demo` with facade `Lib1Kt`.
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
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
}
