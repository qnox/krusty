//! Emit the `META-INF/<name>.kotlin_module` file. kotlinc needs this to discover which file-facade
//! class holds a package's top-level declarations (without it, a Kotlin consumer can't resolve
//! `demo.greet` even though the facade carries correct `@Metadata`).
//!
//! Format: a header of int32s `[len=3, major, minor, patch, flags=0]` (the metadata version + a
//! flags word), then a `JvmModuleProtoBuf.Module` protobuf:
//!   Module { package_parts = field 1 (repeated) }
//!   PackageParts { package_fq_name = field 1, short_class_name = field 2 (repeated) }
//! The version must match the `@Metadata` `mv` the classes carry: the reference kotlinc stamps its
//! default language version (`[2,4,0]` on the pinned 2.4.x toolchain; the 1.9.24-era `[1,9,0]`
//! bytes read fine but byte-diverge) and, under `-language-version X.Y`, `[X,Y,0]`. kotlinc also
//! writes the file for a CLASS-ONLY module (an empty parts list), so an all-classes lib still
//! carries `META-INF/<module>.kotlin_module`.

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

/// One package part a `.kotlin_module` lists: a file class's short name, and the short name of
/// the multifile facade it is a part of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagePart {
    pub name: String,
    pub multifile_facade: Option<String>,
}

impl From<&str> for PackagePart {
    fn from(name: &str) -> Self {
        PackagePart::from(name.to_string())
    }
}

impl From<String> for PackagePart {
    fn from(name: String) -> Self {
        PackagePart {
            name,
            multifile_facade: None,
        }
    }
}

/// `packages`: `(package fq-name, [package parts])`. `version` is the `@Metadata` `mv` the
/// module's classes carry (`[X, Y, 0]` for `-language-version X.Y`, the default stamp
/// otherwise). kotlinc's `PackageParts.addTo` groups the parts by multifile facade, facades in
/// name order and plain parts last, and sorts the parts of each group alphabetically (the
/// caller's insertion order is source-file order). Each multifile part then records its facade
/// as a 1-based index into the facade names that follow.
pub fn build_kotlin_module(packages: &[(String, Vec<PackagePart>)], version: [i32; 3]) -> Vec<u8> {
    // ModuleMapping uses the JVM's String ordering. Rust's `str` order agrees for BMP text but
    // not when a name contains a supplementary-plane character: Java compares the leading
    // surrogate before a later BMP code unit, while Rust compares Unicode scalar values.
    fn jvm_order(left: &str, right: &str) -> std::cmp::Ordering {
        left.encode_utf16().cmp(right.encode_utf16())
    }
    let mut out = Vec::new();
    for v in [3i32, version[0], version[1], version[2], 0] {
        out.extend_from_slice(&v.to_be_bytes()); // version length-prefixed + flags=0
    }
    let mut module = Pb::new();
    for (pkg, parts) in packages {
        let mut pp = Pb::new();
        pp.field_bytes(1, pkg.as_bytes()); // package_fq_name
        let mut parts: Vec<&PackagePart> = parts.iter().collect();
        parts.sort_by(|left, right| {
            match (&left.multifile_facade, &right.multifile_facade) {
                (Some(left), Some(right)) => jvm_order(left, right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| jvm_order(&left.name, &right.name))
        });
        let mut facades: Vec<&str> = Vec::new();
        let mut facade_ids = Vec::new();
        for part in &parts {
            pp.field_bytes(2, part.name.as_bytes()); // short_class_name
            if let Some(facade) = part.multifile_facade.as_deref() {
                let id = match facades.iter().position(|known| *known == facade) {
                    Some(id) => id,
                    None => {
                        facades.push(facade);
                        facades.len() - 1
                    }
                };
                facade_ids.push(id as u64 + 1);
            }
        }
        if !facade_ids.is_empty() {
            pp.field_packed_varints(3, &facade_ids); // multifile_facade_short_name_id
        }
        for facade in facades {
            pp.field_bytes(4, facade.as_bytes()); // multifile_facade_short_name
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
        let got = build_kotlin_module(&[("demo".into(), vec!["LibKt".into()])], [2, 4, 0]);
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
        let got = build_kotlin_module(&[], [2, 4, 0]);
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }

    /// Under `-language-version X.Y` kotlinc stamps the header with `[X, Y, 0]` — measured on
    /// 2.4.20 with `-language-version 2.2` (`[3, 2, 2, 0, 0]`), payload unchanged.
    #[test]
    fn language_version_stamps_the_header() {
        let got = build_kotlin_module(&[("demo".into(), vec!["LibKt".into()])], [2, 2, 0]);
        assert_eq!(
            &got[..20],
            &[
                0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ][..]
        );
        assert_eq!(
            &got[20..],
            &[
                0x0a, 0x0d, 0x0a, 0x04, 0x64, 0x65, 0x6d, 0x6f, 0x12, 0x05, 0x4c, 0x69, 0x62, 0x4b,
                0x74, 0x22, 0x00, 0x2a, 0x00,
            ][..]
        );
    }

    /// kotlinc writes each package's facade parts alphabetically regardless of compilation order:
    /// measured on 2.4.20 with the Kotlin monorepo's util.runtime module, whose
    /// `org.jetbrains.kotlin.utils` parts are `CollectionUtilKt, CollectionsKt, CoreLibKt, …`
    /// even though `collections.kt`/`coreLib.kt` compile after `IndentingPrinter.kt`.
    #[test]
    fn package_parts_are_sorted() {
        // Exact bytes kotlinc 2.4.0 writes for `package demo` with facades written in the order
        // Z.kt, A.kt, B.kt: the part list comes out AKt, BKt, ZKt regardless.
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x15, 0x0a, 0x04, 0x64, 0x65, 0x6d, 0x6f,
            0x12, 0x03, 0x41, 0x4b, 0x74, 0x12, 0x03, 0x42, 0x4b, 0x74, 0x12, 0x03, 0x5a, 0x4b,
            0x74, 0x22, 0x00, 0x2a, 0x00,
        ];
        let got = build_kotlin_module(
            &[(
                "demo".into(),
                vec!["ZKt".into(), "BKt".into(), "AKt".into()],
            )],
            [2, 4, 0],
        );
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }

    #[test]
    fn multifile_parts_are_grouped_by_facade() {
        // Exact bytes kotlinc 2.4.21 writes for `package p` with multifile parts `V__ZKt`,
        // `U__YKt`, `U__BKt` and the plain part `MKt`: facades in order, each one's parts sorted,
        // the plain part last, then the packed 1-based facade ids and the facade names.
        let reference: &[u8] = &[
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x2b, 0x0a, 0x01, 0x70, 0x12, 0x06, 0x55,
            0x5f, 0x5f, 0x42, 0x4b, 0x74, 0x12, 0x06, 0x55, 0x5f, 0x5f, 0x59, 0x4b, 0x74, 0x12,
            0x06, 0x56, 0x5f, 0x5f, 0x5a, 0x4b, 0x74, 0x12, 0x03, 0x4d, 0x4b, 0x74, 0x1a, 0x03,
            0x01, 0x01, 0x02, 0x22, 0x01, 0x55, 0x22, 0x01, 0x56, 0x22, 0x00, 0x2a, 0x00,
        ];
        let part = |name: &str, facade: &str| PackagePart {
            name: name.to_string(),
            multifile_facade: Some(facade.to_string()),
        };
        let got = build_kotlin_module(
            &[(
                "p".into(),
                vec![
                    part("V__ZKt", "V"),
                    part("U__YKt", "U"),
                    part("U__BKt", "U"),
                    "MKt".into(),
                ],
            )],
            [2, 4, 0],
        );
        assert_eq!(
            got, reference,
            "\n got: {:02x?}\n ref: {:02x?}",
            got, reference
        );
    }

    #[test]
    fn package_parts_use_jvm_utf16_order() {
        let astral = "\u{10000}Kt";
        let later_bmp = "\u{e000}Kt";
        let got = build_kotlin_module(
            &[("demo".into(), vec![later_bmp.into(), astral.into()])],
            [2, 4, 0],
        );
        let position = |name: &str| {
            got.windows(name.len())
                .position(|window| window == name.as_bytes())
                .expect("facade in encoded package-parts table")
        };
        assert!(
            position(astral) < position(later_bmp),
            "JVM String order compares the astral character's leading surrogate before U+E000"
        );
    }
}
