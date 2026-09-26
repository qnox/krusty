//! An override matches its candidate only when their type parameters' bounds agree.
//!
//! `Brush.apply` overrides `fun <T : ShapeB> apply(T)` and not its `T : ShapeA` sibling: the two
//! differ only in the bound, so kotlinc emits no `apply(ShapeA)` bridge in `Brush`.

use super::common;

const BOUNDS: &str = "open class ShapeA\n\
open class ShapeB\n\
open class Painter {\n\
    open fun <T : ShapeA> apply(shape: T): Int = 1\n\
    open fun <T : ShapeB> apply(shape: T): Int = 2\n\
}\n\
class Brush : Painter() {\n\
    override fun <T : ShapeB> apply(shape: T): Int = 3\n\
}\n\
fun box(): String {\n\
    val brush: Painter = Brush()\n\
    val total = brush.apply(ShapeA()) + brush.apply(ShapeB())\n\
    return if (total == 4) \"OK\" else \"fail: \" + total\n\
}\n";

#[test]
fn bound_distinguished_override_runs() {
    common::expect_box_ok_with_stdlib(BOUNDS, "Bounds");
}

#[test]
fn bound_distinguished_override_surface_matches_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(BOUNDS, "Bounds");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("Bounds.kt");
    std::fs::write(&path, BOUNDS).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in ["Painter", "Brush"] {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            method_surface(emitted),
            method_surface(&reference),
            "{class}: method surface"
        );
    }
}

/// Each method's name, descriptor and access flags, in classfile order.
fn method_surface(bytes: &[u8]) -> Vec<(String, String, u16)> {
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
