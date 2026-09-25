//! Method access flags kotlinc derives from a declaration's shape. `ACC_VARARGS` marks a method
//! whose LAST physical parameter is its declared `vararg`, so Java can pass it in element form.

use super::common;

/// Every placement of a `vararg`: last or not, behind a receiver or captures, on constructors, and
/// ahead of a suspend function's continuation.
const VARARGS: &str = "class Crate(vararg val slots: Int) {\n\
    constructor(label: String, vararg extra: String) : this(label.length)\n\
    fun pick(vararg picks: Int): Int = picks[0]\n\
    fun pickThen(vararg picks: Int, tail: Int): Int = picks[0] + tail\n\
    fun Crate.nested(vararg picks: Int): Int = picks[0]\n\
}\n\
enum class Tier(vararg val marks: Int) { LOW(1), HIGH(2, 3) }\n\
class Shelf(vararg val rows: Int, val depth: Int = 1)\n\
fun first(vararg values: Int): Int = values[0]\n\
fun Crate.stacked(vararg values: Int): Int = values[0]\n\
fun defaulted(vararg values: Int, bias: Int = 2): Int = values[0] + bias\n\
fun leading(bias: Int = 2, vararg values: Int): Int = values[0] + bias\n\
suspend fun waiting(vararg values: Int): Int = values[0]\n\
fun box(): String {\n\
    val base = 3\n\
    fun offset(vararg values: Int): Int = values[0] + base\n\
    val total = Crate(1).pick(1) + first(1) + defaulted(1) + leading(4, 1) + offset(1)\n\
    return if (total == 14) \"OK\" else \"fail: \" + total\n\
}\n";

#[test]
fn vararg_methods_run() {
    common::expect_box_ok_with_stdlib(VARARGS, "Varargs");
}

#[test]
fn vararg_method_flags_match_kotlinc() {
    assert_method_flags_match_kotlinc("Varargs", VARARGS, &["Crate", "Tier", "Shelf", "VarargsKt"]);
}

fn assert_method_flags_match_kotlinc(name: &str, src: &str, classes: &[&str]) {
    let krusty = common::expect_classes_with_stdlib(src, name);
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join(format!("{name}.kt"));
    std::fs::write(&path, src).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    for class in classes {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            method_flags(emitted),
            method_flags(&reference),
            "{class}: method access flags"
        );
    }
}

/// Each method's name, descriptor and access flags, in classfile order.
fn method_flags(bytes: &[u8]) -> Vec<(String, String, u16)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect()
}
