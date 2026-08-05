//! A classpath member's Kotlin collection identity must come from `@Metadata`, not the JVM
//! `Signature` attribute: both `List` and `MutableList` erase to `java/util/List`, so the
//! bytecode-signature path canonicalized every member return to the READ-ONLY form and
//! `repo.items().add("x")` reported "unresolved reference 'add'" — a false positive on any
//! member returning a mutable collection. The suspend member path already recovered the exact
//! classifier from metadata; the plain member path must apply the same rule.
use super::common;

const LIB: &str = "package lib\n\
    class Repo {\n\
    \x20 val store = mutableListOf(\"a\")\n\
    \x20 fun items(): MutableList<String> = store\n\
    \x20 fun tags(): List<String> = listOf(\"t\")\n\
    \x20 val grid = mutableListOf(mutableSetOf(\"n\"))\n\
    \x20 fun nested(): MutableList<MutableSet<String>> = grid\n\
    }\n";

#[test]
fn mutable_member_return_keeps_mutability() {
    const MAIN: &str = "import lib.Repo\n\
        fun box(): String {\n\
        \x20 val r = Repo()\n\
        \x20 r.items().add(\"b\")\n\
        \x20 return if (r.items().size == 2 && r.tags().size == 1) \"OK\"\n\
        \x20 else \"F:\" + r.items().size\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("member_mutable_ret", LIB, MAIN) {
        assert_eq!(out, "OK", "MutableList member return must keep .add()");
    }
}

#[test]
fn nested_mutable_member_return_keeps_inner_mutability() {
    // The INNER classifier erases too: `MutableList<MutableSet<String>>`'s JVM signature spells
    // `List<Set<String>>`, so the element read `.get(0)` typed a read-only `Set` and `.add` failed.
    // Each argument's classifier must be recovered from the metadata return type under the same
    // same-JVM-internal guard, level by level.
    const MAIN: &str = "import lib.Repo\n\
        fun box(): String {\n\
        \x20 val r = Repo()\n\
        \x20 r.nested().get(0).add(\"x\")\n\
        \x20 return if (r.nested().get(0).size == 2) \"OK\" else \"F:\" + r.nested().get(0).size\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("member_nested_mutable_ret", LIB, MAIN) {
        assert_eq!(
            out, "OK",
            "MutableSet element of a MutableList return must keep .add()"
        );
    }
}
