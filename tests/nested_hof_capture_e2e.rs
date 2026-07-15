//! Two closure-lowering fixes exercised together:
//!
//! 1. A collection HOF nested inside another HOF's BLOCK body, where the inner lambda captures an
//!    enclosing name (`users.mapNotNull { u -> val rs = bindings[u]?.mapNotNull { b -> roles[b] } … }`).
//!    The deep capture scan must descend into a lambda that lives inside a `val` statement, so the
//!    outer (inline-splice) lambda captures `roles` and the nested closure can resolve it. This is the
//!    shape of `UserListService.listMembers`.
//! 2. A mutable `var` captured+mutated by a closure is `Ref`-boxed; its `boxed_elem` entry must NOT
//!    leak into a later same-named plain local in another function (a stale entry made an ordinary
//!    `var x` read as a `Ref` → VerifyError, corpus `regressions/kt344.kt`).
//!
//! Needs the JVM toolchain + kotlin-stdlib + real kotlinc; skips otherwise.
use super::common;

#[test]
fn nested_hof_capture_and_boxed_var_no_leak() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    // members: users=[1,2,3]; bindings {1:[10,20], 2:[30]}; roles {10:a,20:b,30:c}
    //   u=1 -> "1:a,b"; u=2 -> "2:c"; u=3 -> bindings null -> empty -> dropped  => ["1:a,b","2:c"]
    // boxer: var s mutated twice by a closure => "start!!"
    // plain: plain `var x` (same name as a boxed var elsewhere) counting to 5 => 5
    const MAIN: &str = "\
        fun members(users: List<Int>, bindings: Map<Int, List<Int>>, roles: Map<Int, String>): List<String> =\n\
            users.mapNotNull { u ->\n\
                val rs = bindings[u]?.mapNotNull { b -> roles[b] } ?: emptyList()\n\
                if (rs.isEmpty()) null else \"$u:${rs.joinToString(\",\")}\"\n\
            }\n\
        fun boxer(): String { var x = \"start\"; val f = { x = x + \"!\" }; f(); f(); return x }\n\
        fun plain(x0: Int): Int { var x = x0; while (x < 5) { x++ }; return x }\n\
        fun box(): String {\n\
            val m = members(listOf(1, 2, 3), mapOf(1 to listOf(10, 20), 2 to listOf(30)), mapOf(10 to \"a\", 20 to \"b\", 30 to \"c\"))\n\
            val ok = m == listOf(\"1:a,b\", \"2:c\") && boxer() == \"start!!\" && plain(0) == 5\n\
            return if (ok) \"OK\" else \"F m=$m boxer=${boxer()} plain=${plain(0)}\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "nested-HOF block-body capture + no boxed_elem cross-function leak"
    );
}
