//! A data class's synthesized `equals` compares an enum-typed property by reference.
//!
//! kotlinc builds the member from `irNotEquals(this.p, other.p)`, and its `Equals` lowering compares
//! any two operands of which one has an enum type with `if_acmpeq`: an enum constant is a singleton
//! and `Enum.equals` is identity. krusty realized every reference property's comparison as
//! `Intrinsics.areEqual(Object, Object)`, so each data class holding an enum differed from
//! kotlinc's in its `equals` alone. Source `==` on enum operands already took kotlinc's form.

use super::common;

/// The enum property may be nullable, and the enum may come from another file of the module.
const ITEM: &str = "package demo\n\
                    data class Item(val kind: Kind, val name: String, val other: Kind?, val side: Side)\n\
                    enum class Kind { A, B }\n";
const SIDE: &str = "package demo\n\
                    enum class Side { LEFT, RIGHT }\n";
const MAIN: &str = "package demo\n\
                    fun box(): String {\n\
                    \x20   val a = Item(Kind.A, \"x\", null, Side.LEFT)\n\
                    \x20   if (a != Item(Kind.A, \"x\", null, Side.LEFT)) return \"equal items differ\"\n\
                    \x20   if (a == Item(Kind.B, \"x\", null, Side.LEFT)) return \"kind ignored\"\n\
                    \x20   if (a == Item(Kind.A, \"x\", Kind.A, Side.LEFT)) return \"nullable kind ignored\"\n\
                    \x20   if (a == Item(Kind.A, \"x\", null, Side.RIGHT)) return \"side ignored\"\n\
                    \x20   return \"OK\"\n\
                    }\n";

#[test]
fn a_data_class_compares_its_enum_properties_by_reference() {
    let pair = common::ModuleClassPair::compile(
        &[("Item.kt", ITEM), ("Side.kt", SIDE), ("Main.kt", MAIN)],
        "demo/Item",
    );
    let (kotlinc, krusty) = pair.method_code("demo/Item", "equals");
    assert_eq!(krusty, kotlinc, "equals");
    assert!(
        pair.krusty == pair.kotlinc,
        "demo/Item differs from kotlinc's outside equals"
    );
}

#[test]
fn a_data_class_with_enum_properties_keeps_kotlin_equality() {
    let sources = [("Item.kt", ITEM), ("Side.kt", SIDE), ("Main.kt", MAIN)];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "demo.MainKt"),
        "OK"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}
