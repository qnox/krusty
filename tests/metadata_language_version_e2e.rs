//! `-language-version X.Y` stamps every emitted `@kotlin.Metadata` `mv` and the
//! `META-INF/<module>.kotlin_module` version header with `[X, Y, 0]` — measured on the reference
//! kotlinc, whose no-flag stamp is its default language version `[2, 4, 0]` on the pinned 2.4.x
//! toolchain.

use super::common;

const SRC: &str = "package app\n\
    \n\
    class Holder(val value: Int) {\n\
    \x20   fun doubled() = value * 2\n\
    }\n\
    \n\
    fun topLevel(x: Int): Int = Holder(x).doubled()\n";

/// The `mv` of a class's `@kotlin.Metadata`, or `None` for a class without one.
fn metadata_mv(bytes: &[u8]) -> Option<Vec<i32>> {
    common::kotlin_metadata_ints(bytes)?
        .into_iter()
        .find(|(name, _)| name == "mv")
        .map(|(_, ints)| ints)
}

/// A `.kotlin_module` header: five big-endian i32s, `[len=3, major, minor, patch, flags]`.
fn module_header(bytes: &[u8]) -> Vec<i32> {
    bytes[..20]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| i32::from_be_bytes(*word))
        .collect()
}

/// The fixture compiled with the given `-language-version` stamp (`None` = no flag).
fn stamped(metadata_version: Option<[i32; 3]>) -> Vec<(String, Vec<u8>)> {
    common::source_set_compile::compile_in_process_files_metadata_version(
        &[("Holder.kt", SRC)],
        &[common::stdlib_jar()],
        None,
        metadata_version,
    )
    .expect("krusty compiles the fixture")
}

/// Every emitted class that carries `@Metadata` must stamp `expected`, and so must the module
/// file's version header.
fn assert_stamped(metadata_version: Option<[i32; 3]>, expected: [i32; 3]) {
    let outputs = stamped(metadata_version);
    let mut stamped_classes = 0;
    let mut module_files = 0;
    for (name, bytes) in &outputs {
        if name.ends_with(".kotlin_module") {
            module_files += 1;
            assert_eq!(
                module_header(bytes),
                vec![3, expected[0], expected[1], expected[2], 0],
                "{name}: the module header version follows -language-version"
            );
        } else if let Some(mv) = metadata_mv(bytes) {
            stamped_classes += 1;
            assert_eq!(mv, expected.to_vec(), "{name}: @Metadata mv");
        }
    }
    assert!(
        stamped_classes >= 2,
        "the fixture's facade and class both carry @Metadata: {outputs:?}"
    );
    assert_eq!(module_files, 1, "one module file: {outputs:?}");
}

#[test]
fn no_flag_keeps_the_default_language_version_stamp() {
    assert_stamped(None, [2, 4, 0]);
}

#[test]
fn language_version_stamps_every_metadata_and_the_module_file() {
    assert_stamped(Some([2, 2, 0]), [2, 2, 0]);
}

/// The same fixture through kotlinc `-language-version 2.2`: every class's `mv` and the module
/// file are byte-compared against krusty's stamps.
#[test]
fn language_version_2_2_matches_kotlinc() {
    let Some(dir) = common::scratch_dir() else {
        return;
    };
    let out = dir.join("ref");
    std::fs::create_dir_all(&out).unwrap();
    let src_path = dir.join("Holder.kt");
    std::fs::write(&src_path, SRC).unwrap();
    let args = vec![
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-language-version".to_string(),
        "2.2".to_string(),
        src_path.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let mut reference: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![out.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "class") {
                let internal = path
                    .strip_prefix(&out)
                    .unwrap()
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .to_string();
                reference.push((internal, std::fs::read(&path).unwrap()));
            }
        }
    }
    let reference_module = std::fs::read(out.join("META-INF/main.kotlin_module"))
        .expect("kotlinc writes the module file");

    let actual = stamped(Some([2, 2, 0]));
    for (name, reference_bytes) in &reference {
        let (_, actual_bytes) = actual
            .iter()
            .find(|(emitted, _)| emitted == name)
            .unwrap_or_else(|| panic!("krusty did not emit {name}"));
        assert_eq!(
            metadata_mv(actual_bytes),
            metadata_mv(reference_bytes),
            "{name}: @Metadata mv under -language-version 2.2"
        );
    }
    let (_, actual_module) = actual
        .iter()
        .find(|(name, _)| name.ends_with(".kotlin_module"))
        .expect("krusty writes the module file");
    assert_eq!(
        module_header(actual_module),
        module_header(&reference_module),
        "the .kotlin_module version header under -language-version 2.2"
    );
}
