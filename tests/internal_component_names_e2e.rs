//! A data-class `componentN` has the constructor property's visibility.
//!
//! An `internal` property's component is public in bytecode and named `componentN$<module>`.
//! A public component stays `componentN`. A private component is `ACC_PRIVATE` and a protected
//! one is `ACC_PROTECTED`, both unmangled. `@PublishedApi` leaves the accessor on its Kotlin
//! name and still suffixes the component. `@Metadata` keeps the Kotlin name and records the
//! JVM name plus the component's visibility bits.

use super::common;

const SOURCE: &str = "\
package sample

data class File(
    internal val javaPath: String,
    val name: String,
    private val secret: String,
    protected val note: String,
)

data class Published(@PublishedApi internal val exposed: Int)

fun read(file: File): String {
    val (path, name) = file
    return path + name
}

fun box(): String {
    val file = File(\"p\", \"n\", \"s\", \"t\")
    val published = Published(1)
    val (path, name) = file
    return if (read(file) == \"pn\" && path == \"p\" && name == \"n\" && published.component1() == 1) {
        \"OK\"
    } else {
        \"fail\"
    }
}
";

const HOLDER: &str = "\
package sample

data class Holder(internal val path: String)

fun Holder.own(): String {
    val (path) = this
    return path
}
";

const USE: &str = "\
package sample

fun box(): String {
    val holder = Holder(\"ok\")
    val (path) = holder
    return if (path == \"ok\" && holder.own() == \"ok\") \"OK\" else path
}
";

fn krusty_classes(source: &str, stem: &str, module_name: &str) -> Vec<(String, Vec<u8>)> {
    let classpath = [common::stdlib_jar()];
    common::compile_in_process_metadata_cp_module(source, stem, &classpath, module_name)
        .unwrap_or_else(|| {
            let diagnostics = common::front_end_diagnostics(source, &classpath, None);
            panic!("krusty declined {stem} under {module_name}: {diagnostics:?}")
        })
}

fn kotlinc_classes(
    source: &str,
    file_name: &str,
    module_name: &str,
) -> Option<Vec<(String, Vec<u8>)>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let source_path = dir.join(file_name);
    std::fs::write(&source_path, source).ok()?;
    let args = vec![
        "-module-name".to_string(),
        module_name.to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed under {module_name}: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&out, &out, &mut classes);
    let _ = std::fs::remove_dir_all(&dir);
    Some(classes)
}

fn collect_classes(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    let entries = std::fs::read_dir(dir).expect("read kotlinc output");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_classes(root, &path, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "class") {
            let internal = path
                .strip_prefix(root)
                .expect("class stays under the output root")
                .to_string_lossy()
                .trim_end_matches(".class")
                .replace('\\', "/");
            out.push((internal, std::fs::read(&path).expect("read class")));
        }
    }
}

fn class_bytes<'a>(classes: &'a [(String, Vec<u8>)], internal: &str) -> &'a [u8] {
    classes
        .iter()
        .find(|(name, _)| name == internal)
        .map(|(_, bytes)| bytes.as_slice())
        .unwrap_or_else(|| panic!("{internal} was not emitted"))
}

#[test]
fn internal_data_component_names_match_kotlinc() {
    for module_name in ["main", "lib1"] {
        let Some(reference) = kotlinc_classes(SOURCE, "File.kt", module_name) else {
            eprintln!("skip ({module_name}: provisioned kotlinc unavailable)");
            return;
        };
        let krusty = krusty_classes(SOURCE, "File", module_name);
        let mut mismatches = Vec::new();
        // Compare the complete owning artifacts. Call-site lowering is covered separately by the
        // runtime and sibling-file regressions below; including the facade here would mix this
        // component-visibility check with unrelated local-slot allocation differences.
        for internal in ["sample/File", "sample/Published"] {
            let ours = class_bytes(&krusty, internal);
            let expected = class_bytes(&reference, internal);
            if ours != expected {
                mismatches.push(format!(
                    "module {module_name}: {}",
                    common::exact_class_difference(internal, expected, ours)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
    }
}

#[test]
fn internal_data_component_calls_run() {
    let result = common::compile_and_run_with_stdlib(SOURCE, "File");
    assert_eq!(result.as_deref(), Some("OK"));
}

#[test]
fn sibling_file_calls_the_mangled_component() {
    let jdk = common::jdk_modules();
    let result = common::compile_and_run_box_files(
        &[("Holder.kt", HOLDER), ("Use.kt", USE)],
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    );
    assert_eq!(result.as_deref(), Some("OK"));
}

#[test]
fn private_component_is_hidden_from_another_file() {
    let diagnostics = common::module_front_end_diagnostics(&[
        (
            "File",
            "package sample\n\
             data class File(private val secret: String, val name: String)\n",
        ),
        (
            "Use",
            "package sample\n\
             fun peek(file: File) = file.component1()\n",
        ),
    ])
    .expect("stdlib and JDK are provisioned");
    assert_eq!(
        diagnostics,
        ["cannot access 'component1': it is private in 'sample/File'"]
    );
}
