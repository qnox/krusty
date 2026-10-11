//! Interop libraries built from the headers in `testdata`, compared with what Kotlin/Native 2.4.20's
//! `cinterop -Xccall-mode direct -no-default-libs` writes for the same `.def` file and headers.
//!
//! Each `<case>.expected` is the reference library rendered by [`render_library`]: its layout, its
//! manifest, its module header, and every package fragment as [`render_fragment`] prints it, so a
//! difference names the declaration and field that differ.

use std::path::{Path, PathBuf};

use super::super::def_file::DefFile;
use super::super::preprocessor::IncludeDir;
use super::{interop_library, InteropRequest};
use crate::klib::write::KlibStamp;
use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;
use crate::metadata::decode::Pb;
use crate::metadata::fragment_text::render_fragment;
use crate::native::target::{Arch, NativeTarget, Os};

const HEADERS: &[(&str, &str)] = &[
    ("basics.h", include_str!("testdata/basics.h")),
    ("defcode.h", include_str!("testdata/defcode.h")),
    ("layouts.h", include_str!("testdata/layouts.h")),
    ("types.h", include_str!("testdata/types.h")),
];

/// The library a `.def` file describes, built for x86-64 Linux and written below a fresh
/// directory named after it.
fn build(name: &str, def: &str) -> PathBuf {
    let def = DefFile::parse(def);
    let writer = interop_library(InteropRequest {
        def: &def,
        unique_name: name,
        default_package: name,
        target: NativeTarget::new(Arch::X86_64, Os::Linux),
        system_dirs: vec![IncludeDir::Builtin(HEADERS)],
        sysroot: None,
        stamp: KlibStamp::new(KotlinVersion::V2_4_20, LanguageVersion::V2_4),
    })
    .expect("builds the library");
    let root = std::env::temp_dir().join(format!("cinterop-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    writer.write_directory(&root).expect("writes the library");
    root
}

/// Every directory and file of the library at `root`, in path order, with the manifest and
/// module header decoded and each fragment rendered.
fn render_library(root: &Path) -> String {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("a library directory") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    let mut out = String::new();
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .expect("below the root")
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            out.push_str(&format!("{relative}/\n"));
            continue;
        }
        let bytes = std::fs::read(&path).expect("a library file");
        out.push_str(&format!("=== {relative}\n"));
        if relative.ends_with("manifest") {
            out.push_str(&String::from_utf8(bytes).expect("a UTF-8 manifest"));
        } else if relative.ends_with("module") {
            out.push_str(&render_module(&bytes));
        } else {
            out.push_str(&render_fragment(&bytes));
        }
    }
    out
}

/// The module header's name and package lists.
fn render_module(bytes: &[u8]) -> String {
    let mut cursor = Pb::new(bytes);
    let mut out = String::new();
    while !cursor.at_end() {
        let tag = cursor.varint().expect("a field tag");
        assert_eq!(tag & 7, 2, "the module header holds only strings");
        let length = cursor.varint().expect("a length") as usize;
        let text = String::from_utf8_lossy(cursor.bytes(length).expect("the string")).into_owned();
        let field = match tag >> 3 {
            1 => "module_name",
            7 => "package",
            8 => "empty_package",
            other => panic!("module header field {other}"),
        };
        out.push_str(&format!("{field}: {text}\n"));
    }
    out
}

fn assert_matches_reference(name: &str, def: &str, expected: &str) {
    let root = build(name, def);
    let actual = render_library(&root);
    std::fs::remove_dir_all(&root).expect("removes the library");
    if actual != expected {
        let dump = std::env::temp_dir().join(format!("cinterop-{name}.actual"));
        std::fs::write(&dump, &actual).expect("writes the rendering");
        panic!(
            "the `{name}` library differs from the reference; krusty's is in {}",
            dump.display()
        );
    }
}

#[test]
fn structs_enums_typedefs_functions_globals_and_macros() {
    assert_matches_reference(
        "basics",
        include_str!("testdata/basics.def"),
        include_str!("testdata/basics.expected"),
    );
}

#[test]
fn type_mapping_struct_spelling_and_constant_expressions() {
    assert_matches_reference(
        "types",
        include_str!("testdata/types.def"),
        include_str!("testdata/types.expected"),
    );
}

#[test]
fn def_file_code_asm_labels_bitfield_enums_and_name_clashes() {
    assert_matches_reference(
        "defcode",
        include_str!("testdata/defcode.def"),
        include_str!("testdata/defcode.expected"),
    );
}

#[test]
fn layouts_anonymous_members_enum_styles_and_wrapped_macros() {
    assert_matches_reference(
        "layouts",
        include_str!("testdata/layouts.def"),
        include_str!("testdata/layouts.expected"),
    );
}
