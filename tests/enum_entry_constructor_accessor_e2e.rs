//! An enum's constructors are private. An entry with a body becomes a subclass that reaches the
//! constructor its entry selected through a public synthetic `(String, int, …,
//! DefaultConstructorMarker)` accessor, as kotlinc compiles it. The subclass's own constructor
//! carries the enum's `Signature` convention, its entry's line and the `$enum$` locals.

use super::common;

const ENUMS: &str = "enum class Plain { A { override fun g() = 1 }, B { override fun g() = 2 }; abstract fun g(): Int }
enum class Labeled(val label: String) {
    A { override fun f() = 1 },
    B(\"y\");
    constructor() : this(\"x\")
    open fun f(): Int = 0
}
fun box(): String {
    if (Plain.A.g() + Plain.B.g() != 3) return \"plain\"
    if (Labeled.A.label != \"x\" || Labeled.A.f() != 1 || Labeled.B.f() != 0) return \"labeled\"
    return \"OK\"
}
";

#[test]
fn enum_entry_subclasses_reach_private_constructors_like_kotlinc() {
    let sources = [("Enums.kt", ENUMS)];
    for class in [
        "Plain",
        "Plain$A",
        "Plain$B",
        "Labeled",
        "Labeled$A",
        "EnumsKt",
    ] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
}

#[test]
fn enum_entry_subclasses_run() {
    assert_eq!(
        common::expect_box_run_with_stdlib(ENUMS, "EnumEntryConstructorAccessor"),
        "OK"
    );
}
