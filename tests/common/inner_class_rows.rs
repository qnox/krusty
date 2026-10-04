//! Differential comparison of classes, and of their `InnerClasses` rows, with kotlinc's.

use krusty::jvm::classreader::{parse_class, InnerClassRef};
use std::path::PathBuf;

/// Compile `source` with kotlinc and krusty against `classpath` and require each of `classes` to
/// carry kotlinc's `InnerClasses` rows, in kotlinc's order.
pub fn assert_same_inner_classes(
    stem: &str,
    source: &str,
    classpath: &[PathBuf],
    classes: &[&str],
) {
    let rows = |bytes: &[u8]| -> Vec<InnerClassRef> {
        parse_class(bytes).expect("a parseable class").inner_classes
    };
    for (class, (expected, actual)) in classes
        .iter()
        .zip(compile_with_kotlinc(stem, source, classpath, classes))
    {
        assert_eq!(
            rows(&actual),
            rows(&expected),
            "{stem}: {class}'s InnerClasses rows"
        );
    }
}

/// Compile `source` with kotlinc and krusty against `classpath` and return each of `classes` as
/// `(kotlinc's bytes, krusty's bytes)`.
pub fn compile_with_kotlinc(
    stem: &str,
    source: &str,
    classpath: &[PathBuf],
    classes: &[&str],
) -> Vec<(Vec<u8>, Vec<u8>)> {
    compile_pairs(stem, source, classpath, None, classes)
}

/// [`compile_with_kotlinc`] for kotlinc's `-jvm-target` `jvm_target`, against the standard library.
pub fn compile_with_kotlinc_for_target(
    stem: &str,
    source: &str,
    jvm_target: u16,
    classes: &[&str],
) -> Vec<(Vec<u8>, Vec<u8>)> {
    compile_pairs(stem, source, &[], Some(jvm_target), classes)
}

fn compile_pairs(
    stem: &str,
    source: &str,
    classpath: &[PathBuf],
    jvm_target: Option<u16>,
    classes: &[&str],
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let target = jvm_target
        .map(super::common_core::kotlinc_jvm_target_argument)
        .unwrap_or_else(|| "default".to_string());
    let language_args = super::common_core::language_directives::kotlinc_args(source);
    let inputs = super::common_core::byte_dump::class_dump_inputs(
        source,
        &target,
        &language_args,
        classpath,
    );
    let expected = super::common_core::byte_dump::kotlinc_class_dumps(
        stem,
        &target,
        &inputs.variant,
        inputs.fingerprint,
        classes,
        || {
            let dir = super::common_core::scratch_dir().expect("scratch directory");
            let reference = dir.join("ref");
            std::fs::create_dir_all(&reference).expect("reference output directory");
            let source_path = dir.join(format!("{stem}.kt"));
            std::fs::write(&source_path, source).expect("fixture source");
            let mut args = vec!["-d".to_string(), reference.to_string_lossy().into_owned()];
            if !classpath.is_empty() {
                args.push("-cp".to_string());
                args.push(
                    std::env::join_paths(classpath)
                        .expect("a joinable classpath")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            if jvm_target.is_some() {
                args.push("-jvm-target".to_string());
                args.push(target.clone());
            }
            args.extend(language_args.iter().cloned());
            args.push(source_path.to_string_lossy().into_owned());
            let (code, stderr) = super::common_core::kotlinc_compile(&args)
                .expect("reference kotlinc is provisioned");
            assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");
            let mut produced = std::collections::BTreeMap::new();
            for class in classes {
                let bytes = std::fs::read(reference.join(format!("{class}.class")))
                    .unwrap_or_else(|_| panic!("{stem}: kotlinc emits {class}"));
                produced.insert((*class).to_string(), bytes);
            }
            let _ = std::fs::remove_dir_all(&dir);
            Some(produced)
        },
    )
    .unwrap_or_else(|| panic!("{stem}: kotlinc did not produce the requested classes"));

    let mut krusty_classpath = classpath.to_vec();
    krusty_classpath.push(super::common_core::stdlib_jar());
    let jdk = super::common_core::jdk_modules();
    let compiled = match jvm_target {
        // A class file's major version is its JVM target plus 44.
        Some(target) => super::common_core::compile_in_process_metadata_cp_module_target(
            source,
            stem,
            &krusty_classpath,
            "main",
            Some(target + 44),
        ),
        None => super::common_core::compile_in_process(
            source,
            stem,
            &krusty_classpath,
            Some(jdk.as_path()),
        ),
    }
    .unwrap_or_else(|| {
        panic!(
            "{stem}: krusty rejected the fixture: {:?}",
            super::common_core::front_end_diagnostics(
                source,
                &krusty_classpath,
                Some(jdk.as_path())
            )
        )
    });
    classes
        .iter()
        .zip(expected)
        .map(|(class, expected)| {
            let (_, actual) = compiled
                .iter()
                .find(|(name, _)| name == class)
                .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
            (expected, actual.clone())
        })
        .collect()
}
