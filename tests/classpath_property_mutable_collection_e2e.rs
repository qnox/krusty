//! A classpath PROPERTY's Kotlin collection identity must come from `@Metadata`, not the JVM
//! `Signature` attribute: both `List` and `MutableList` erase to `java/util/List`, so the
//! getter-signature path canonicalized every property type to the READ-ONLY form and
//! `repo.bag.add("x")` reported "unresolved reference 'add'" — a false positive on any
//! property declared with a mutable collection type. Member FUNCTIONS already recover the
//! exact classifier from the `@Metadata` return class (guarded to the same-JVM-internal
//! sibling); properties must apply the same rule.
use super::common;

const LIB: &str = "package lib\n\
    class Repo3 {\n\
    \x20 val bag: MutableList<String> = mutableListOf(\"a\")\n\
    \x20 val tags: List<String> = listOf(\"t\")\n\
    \x20 var box: MutableSet<Int> = mutableSetOf(1)\n\
    \x20 val grid: MutableList<MutableList<String>> = mutableListOf(mutableListOf(\"g\"))\n\
    }\n";

#[test]
fn mutable_property_keeps_mutability() {
    const MAIN: &str = "import lib.Repo3\n\
        fun box(): String {\n\
        \x20 val r = Repo3()\n\
        \x20 r.bag.add(\"b\")\n\
        \x20 r.box.add(2)\n\
        \x20 return if (r.bag.size == 2 && r.box.size == 2 && r.tags.size == 1) \"OK\"\n\
        \x20 else \"F:\" + r.bag.size + \":\" + r.box.size\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("property_mutable_ty", LIB, MAIN) {
        assert_eq!(
            out, "OK",
            "MutableList/MutableSet property must keep .add()"
        );
    }
}

#[test]
fn nested_mutable_property_keeps_inner_mutability() {
    // The INNER classifier erases too: `MutableList<MutableList<String>>`'s getter signature spells
    // `List<List<String>>`, so the element read `[0]` typed a read-only `List` and `.add` failed.
    // Each argument's classifier must be recovered from the metadata property type under the same
    // same-JVM-internal guard, level by level.
    const MAIN: &str = "import lib.Repo3\n\
        fun box(): String {\n\
        \x20 val r = Repo3()\n\
        \x20 r.grid[0].add(\"x\")\n\
        \x20 return if (r.grid[0].size == 2) \"OK\" else \"F:\" + r.grid[0].size\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("property_nested_mutable_ty", LIB, MAIN) {
        assert_eq!(
            out, "OK",
            "MutableList element of a MutableList property must keep .add()"
        );
    }
}
