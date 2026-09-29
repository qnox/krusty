//! `super.size` on an `ArrayList` subclass calls `java.util.ArrayList.size()I`.
//!
//! The private `size` field is JVM storage, not the Kotlin property. Reading it with `getfield`
//! is an `IllegalAccessError` (`specialBuiltins/explicitSuperCall.kt`).

use super::common;
use std::fs;

const SRC: &str = "class A : ArrayList<String>() {\n\
    override val size: Int get() = super.size + 56\n\
}\n\
fun box(): String {\n\
    val a = A()\n\
    if (a.size != 56) return \"fail: ${a.size}\"\n\
    return \"OK\"\n\
}\n";

fn getter_instructions(root: &std::path::Path, class_bytes: &[u8]) -> Vec<String> {
    let class_file = root.join("A.class");
    fs::write(&class_file, class_bytes).expect("write A.class");
    let disassembly =
        common::javap(&["-c", "-p", &class_file.to_string_lossy()]).expect("javap unavailable");
    let instructions = common::method_instructions(&disassembly, "int getSize();");
    assert!(!instructions.is_empty(), "missing getSize:\n{disassembly}");
    instructions
}

#[test]
fn array_list_super_size_calls_size() {
    common::expect_box_ok_with_stdlib(SRC, "SuperCollectionSize");

    let root = common::scratch_dir().expect("scratch directory");
    let reference_dir = root.join("ref");
    fs::create_dir_all(&reference_dir).expect("reference output");
    let source = root.join("SuperCollectionSize.kt");
    fs::write(&source, SRC).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let reference_bytes = fs::read(reference_dir.join("A.class")).expect("kotlinc A.class");
    let krusty_bytes = common::expect_classes_with_stdlib(SRC, "SuperCollectionSize")
        .into_iter()
        .find(|(name, _)| name == "A")
        .map(|(_, bytes)| bytes)
        .expect("krusty emits A");

    let krusty_dir = root.join("krusty");
    fs::create_dir_all(&krusty_dir).expect("krusty output");
    let reference = getter_instructions(&reference_dir, &reference_bytes);
    let krusty = getter_instructions(&krusty_dir, &krusty_bytes);
    assert_eq!(krusty, reference, "A.getSize");
    let _ = fs::remove_dir_all(&root);
}
