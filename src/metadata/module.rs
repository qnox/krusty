//! Emit the `META-INF/<name>.kotlin_module` file. kotlinc needs this to discover which file-facade
//! class holds a package's top-level declarations (without it, a Kotlin consumer can't resolve
//! `demo.greet` even though the facade carries correct `@Metadata`).
//!
//! Format: a header of int32s `[len=3, major, minor, patch, flags=0]` (the metadata version + a
//! flags word), then a `JvmModuleProtoBuf.Module` protobuf:
//!   Module { package_parts = field 1 (repeated) }
//!   PackageParts { package_fq_name = field 1, short_class_name = field 2 (repeated) }
//! The version must match the `@Metadata` `mv` the classes carry — the reference kotlinc (2.4.0)
//! stamps `[2,4,0]` (decoded from its output; the 1.9.24-era `[1,9,0]` bytes read fine but
//! byte-diverge from the pinned toolchain). kotlinc also writes the file for a CLASS-ONLY module
//! (an empty parts list), so an all-classes lib still carries `META-INF/<module>.kotlin_module`.

use crate::metadata::protobuf::Pb;

/// The `META-INF/<name>.kotlin_module` path kotlinc writes for `-module-name <name>`: characters a
/// file name cannot carry (`\ / : * ? " < > |`) become `_`; everything else — including `.`,
/// `-`, space and the other punctuation — is kept. Measured on kotlinc 2.4.10. Only the FILE name
/// is sanitised; the module name recorded in `@Metadata` keeps its `:`.
pub fn kotlin_module_file_name(module_name: &str) -> String {
    let sanitised = module_name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect::<String>();
    format!("META-INF/{sanitised}.kotlin_module")
}

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

    /// kotlinc names the module file after `-module-name` with the characters a file name cannot
    /// carry replaced by `_` — measured on 2.4.10: `com.x:y-z w/q.r` → `com.x_y-z w_q.r`, and the
    /// full punctuation probe kept `, ; = + @ # $ % & ( ) [ ] { } ~ ! ^` and space. The name INSIDE
    /// `@Metadata` keeps its `:`; only the file name is sanitised.
    #[test]
    fn module_file_name_sanitises_like_kotlinc() {
        assert_eq!(
            kotlin_module_file_name("com.x:y-z w/q.r"),
            "META-INF/com.x_y-z w_q.r.kotlin_module"
        );
        assert_eq!(
            kotlin_module_file_name(r#"a*b?c"d<e>f|g\h,i;j=k+l@m#n$o%p&q(r)s[t]u{v}w~x!y^z"#),
            "META-INF/a_b_c_d_e_f_g_h,i;j=k+l@m#n$o%p&q(r)s[t]u{v}w~x!y^z.kotlin_module"
        );
        assert_eq!(
            kotlin_module_file_name("main"),
            "META-INF/main.kotlin_module"
        );
    }

    #[test]
    fn matches_kotlinc_reference_module() {
        // Exact 39 bytes kotlinc 2.4.0 writes for `package demo` with facade `LibKt`
        // (version words [2,4,0], one PackageParts, then the trailing empty f4/f5).
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x0d, 0x0a, 0x04, 0x64, 0x65, 0x6d, 0x6f,
            0x12, 0x05, 0x4c, 0x69, 0x62, 0x4b, 0x74, 0x22, 0x00, 0x2a, 0x00,
        ];
        let got = build_kotlin_module(&[("demo".into(), vec!["LibKt".into()])]);
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }

    #[test]
    fn class_only_module_matches_kotlinc_empty_parts() {
        // kotlinc writes the module file even when the module has NO file facades (a class-only
        // lib): version words + the trailing empty f4/f5, no PackageParts. Decoded from 2.4.0.
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x22, 0x00, 0x2a, 0x00,
        ];
        let got = build_kotlin_module(&[]);
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }
}
