//! Conflicting inherited interface defaults (`KT-36188`).
//!
//! A direct clash of default expressions is rejected. When one default is reached through an
//! intermediate classifier, the call stays legal and uses the `$default` of the declaration found
//! first in a left-to-right depth-first supertype walk.

use super::common;
use std::fs;

const LEFTMOST_A: &str = "interface A {\n\
    fun foo(a: String = \"OK\"): String\n\
}\n\
interface A2 : A\n\
interface B {\n\
    fun foo(a: String = \"Fail\"): String\n\
}\n\
class Impl : A2, B {\n\
    override fun foo(a: String) = a\n\
}\n\
fun box(): String = Impl().foo()\n";

const LEFTMOST_B: &str = "interface A {\n\
    fun foo(a: String = \"OK\"): String\n\
}\n\
interface A2 : A\n\
interface B {\n\
    fun foo(a: String = \"Fail\"): String\n\
}\n\
class Impl : B, A2 {\n\
    override fun foo(a: String) = a\n\
}\n\
fun box(): String = Impl().foo()\n";

const THROUGH_SUBINTERFACE: &str = "interface A {\n\
    fun foo(a: String = \"OK\"): String\n\
}\n\
interface A2 : A\n\
interface B {\n\
    fun foo(a: String = \"Fail\"): String\n\
}\n\
interface C : A2, B\n\
class Impl : C {\n\
    override fun foo(a: String) = a\n\
}\n\
fun box(): String = Impl().foo()\n";

fn box_instructions(root: &std::path::Path, class: &str, bytes: &[u8]) -> Vec<String> {
    fs::create_dir_all(root).expect("class directory");
    let class_file = root.join(format!("{class}.class"));
    fs::write(&class_file, bytes).expect("write class");
    let disassembly = common::javap(&["-c", "-p", "-v", &class_file.to_string_lossy()])
        .expect("javap unavailable");
    let instructions = common::method_instructions(&disassembly, "String box();");
    assert!(!instructions.is_empty(), "missing box:\n{disassembly}");
    instructions
}

fn assert_inherited_default(name: &str, src: &str, expected: &str) {
    assert_eq!(
        common::expect_box_run_with_stdlib(src, name),
        expected,
        "{name}"
    );

    let root = common::scratch_dir().expect("scratch directory");
    let reference_dir = root.join("ref");
    fs::create_dir_all(&reference_dir).expect("reference output");
    let source = root.join(format!("{name}.kt"));
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
    assert_eq!(code, 0, "{name}: kotlinc rejected the fixture: {stderr}");
    let facade = format!("{name}Kt");
    let reference_bytes =
        fs::read(reference_dir.join(format!("{facade}.class"))).expect("kotlinc facade");
    let krusty_bytes = common::expect_classes_with_stdlib(src, name)
        .into_iter()
        .find(|(emitted, _)| emitted == &facade)
        .map(|(_, bytes)| bytes)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {facade}"));
    let reference = box_instructions(&reference_dir, &facade, &reference_bytes);
    let krusty = box_instructions(&root.join("krusty"), &facade, &krusty_bytes);
    assert_eq!(krusty, reference, "{facade}.box");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_leftmost_intermediate_supertype_supplies_the_default() {
    assert_inherited_default("InheritedDefaultLeft", LEFTMOST_A, "OK");
}

#[test]
fn the_first_listed_supertype_supplies_the_default() {
    assert_inherited_default("InheritedDefaultRight", LEFTMOST_B, "Fail");
}

#[test]
fn a_subinterface_keeps_its_leftmost_inherited_default() {
    assert_inherited_default("InheritedDefaultSub", THROUGH_SUBINTERFACE, "OK");
}
