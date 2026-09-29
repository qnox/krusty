//! Delegated properties that share a source name store distinct delegate fields.
//!
//! `val O.prop by …` and `val K.prop by …` both lower `{name}$delegate`. kotlinc keeps that
//! spelling for the first and suffixes each later field `$1`, `$2`, … in declaration order.
//! The same numbering applies to a member and a member extension on one class.

use super::common;
use std::fs;

const TOP_LEVEL: &str = "open class C\n\
\n\
object O : C()\n\
\n\
object K : C()\n\
\n\
class D(val value: String) {\n\
    operator fun getValue(thisRef: C, property: Any): String = value\n\
}\n\
\n\
class E(val value: String) {\n\
    operator fun getValue(thisRef: C, property: Any): String = value\n\
}\n\
\n\
val O.prop by D(\"O\")\n\
val K.prop by E(\"K\")\n\
\n\
fun box() = O.prop + K.prop\n";

const MEMBER: &str = "class D(val value: String) {\n\
    operator fun getValue(thisRef: Any?, property: Any?): String = value\n\
}\n\
\n\
class Owner {\n\
    val prop: String by D(\"O\")\n\
    val String.prop: String by D(\"K\")\n\
    val Int.prop: String by D(\"!\")\n\
}\n\
\n\
fun Owner.read(text: String, number: Int) = text.prop + number.prop\n\
\n\
fun box(): String {\n\
    val owner = Owner()\n\
    return owner.prop + owner.read(\"x\", 1)\n\
}\n";

fn method_instructions(
    root: &std::path::Path,
    class: &str,
    bytes: &[u8],
    marker: &str,
) -> Vec<String> {
    fs::create_dir_all(root).expect("class directory");
    let class_file = root.join(format!("{class}.class"));
    fs::write(&class_file, bytes).expect("write class");
    let disassembly = common::javap(&["-c", "-p", "-v", &class_file.to_string_lossy()])
        .expect("javap unavailable");
    let instructions = common::method_instructions(&disassembly, marker);
    assert!(
        !instructions.is_empty(),
        "{class} missing {marker}:\n{disassembly}"
    );
    instructions
}

fn assert_methods_match(stem: &str, src: &str, class: &str, markers: &[&str]) {
    let root = common::scratch_dir().expect("scratch directory");
    let reference_dir = root.join("ref");
    fs::create_dir_all(&reference_dir).expect("reference output");
    let source = root.join(format!("{stem}.kt"));
    fs::write(&source, src).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc rejected the fixture: {stderr}");
    let reference_bytes =
        fs::read(reference_dir.join(format!("{class}.class"))).expect("kotlinc class");
    let krusty_bytes = common::expect_classes_with_stdlib(src, stem)
        .into_iter()
        .find(|(emitted, _)| emitted == class)
        .map(|(_, bytes)| bytes)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
    for marker in markers {
        let reference = method_instructions(&reference_dir, class, &reference_bytes, marker);
        let krusty = method_instructions(&root.join("krusty"), class, &krusty_bytes, marker);
        assert_eq!(krusty, reference, "{class} {marker}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn extension_delegates_of_one_name_use_distinct_fields() {
    assert_eq!(
        common::expect_box_run_with_stdlib(TOP_LEVEL, "ExtensionDelegateFields"),
        "OK"
    );
    assert_methods_match(
        "ExtensionDelegateFields",
        TOP_LEVEL,
        "ExtensionDelegateFieldsKt",
        &["String getProp(O);", "String getProp(K);"],
    );
}

#[test]
fn a_member_and_its_extension_delegates_use_distinct_fields() {
    assert_eq!(
        common::expect_box_run_with_stdlib(MEMBER, "MemberDelegateFields"),
        "OK!"
    );
    assert_methods_match(
        "MemberDelegateFields",
        MEMBER,
        "Owner",
        &[
            "String getProp();",
            "String getProp(java.lang.String);",
            "String getProp(int);",
        ],
    );
}
