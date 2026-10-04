//! An anonymous object that captures a `crossinline` lambda is a different class at each inline
//! call. `typeOf` of the instance names that class.

use std::path::Path;

use super::common::{self, Fixture};

fn collect_class_names(root: &Path, directory: &Path, names: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_class_names(root, &path, names);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            names.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn class_names(source: &str) -> (Vec<String>, Vec<String>) {
    let work = common::scratch_dir().expect("allocate local-object fixture");
    let source_path = work.join("localObject.kt");
    let krusty_output = work.join("krusty");
    let reference_output = work.join("reference");
    std::fs::write(&source_path, source).expect("write local-object fixture");
    std::fs::create_dir_all(&krusty_output).expect("create krusty output");
    std::fs::create_dir_all(&reference_output).expect("create reference output");

    let krusty = std::process::Command::new(common::krusty_binary())
        .args(["-d", krusty_output.to_str().expect("UTF-8 output")])
        .arg("-no-reflect")
        .arg(&source_path)
        .output()
        .expect("run krusty");
    assert!(
        krusty.status.success(),
        "krusty failed: {}",
        String::from_utf8_lossy(&krusty.stderr)
    );
    let reference_args = vec![
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let mut krusty_names = Vec::new();
    collect_class_names(&krusty_output, &krusty_output, &mut krusty_names);
    krusty_names.sort();
    let mut reference_names = Vec::new();
    collect_class_names(&reference_output, &reference_output, &mut reference_names);
    reference_names.sort();
    let _ = std::fs::remove_dir_all(work);
    (reference_names, krusty_names)
}

const SOURCE: &str = "\
import kotlin.reflect.typeOf\n\
import kotlin.reflect.KType\n\
inline fun <reified T> typeOfX(x: T) = typeOf<T>()\n\
inline fun typeOfLocal(crossinline f: () -> Unit): Pair<Any, KType> {\n\
    val x = object { fun foo() = f() }\n\
    return x to typeOfX(x)\n\
}\n\
var seen = \"\"\n\
fun box(): String {\n\
    val first = typeOfLocal { seen += \"a\" }\n\
    val second = typeOfLocal { seen += \"b\" }\n\
    if (first.first::class != first.second.classifier) return \"FAIL 1\"\n\
    if (second.first::class != second.second.classifier) return \"FAIL 2\"\n\
    if (first.first::class == second.first::class) return \"FAIL 3\"\n\
    if (first.second.classifier == second.second.classifier) return \"FAIL 4\"\n\
    first.first.javaClass.getDeclaredMethod(\"foo\").invoke(first.first)\n\
    second.first.javaClass.getDeclaredMethod(\"foo\").invoke(second.first)\n\
    if (seen != \"ab\") return \"FAIL 5: $seen\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn crossinline_object_classes_match_kotlinc() {
    let (reference, krusty) = class_names(SOURCE);
    assert_eq!(krusty, reference);
}

#[test]
fn crossinline_object_type_of_matches_kotlinc() {
    let krusty = Fixture::new().with_reflect().run_box(SOURCE);
    let reflect = common::dist_jar("kotlin-reflect.jar").expect("kotlin-reflect");
    let reference = common::kotlinc_box_result_with_classpath(SOURCE, &[reflect]);
    assert_eq!(krusty, reference);
    assert_eq!(krusty, "OK");
}
