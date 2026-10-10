//! Nested generic calls under an expected type must be checked in time linear in the nesting.
//!
//! After a callable is selected, every argument is re-entered under the selected parameter type
//! so the expectation can reach a nested generic call's result variables (`"OK" to emptySet()`
//! under `Pair<Any, Set<Any>>`). Re-entering an argument whose inferred type already EQUALS that
//! expectation is idempotent, yet each level of `mapOf("k" to mapOf("k" to …))` used to redo the
//! whole subtree — `to` and `mapOf` each re-entering theirs — so a six-level literal in a real
//! test file took minutes and stalled the editor's analysis worker.

use super::common;

fn nested(depth: usize) -> String {
    let mut inner = String::from("1");
    for level in 0..depth {
        inner = format!("mapOf(\"k{level}\" to {inner})");
    }
    format!("package sample\nval root: Map<String, Any?> = {inner}\nfun n(): Int = root.size\n")
}

#[test]
fn a_deeply_nested_map_literal_checks_in_linear_time() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let started = std::time::Instant::now();
    let d = common::front_end_diagnostics(
        &nested(9),
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    let elapsed = started.elapsed();
    assert_eq!(d, Vec::<String>::new());
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "nine nested generic calls took {elapsed:?}; the argument re-check is exponential again"
    );
}

#[test]
fn an_expectation_still_reaches_a_nested_generic_call_result() {
    // The re-entry this test's sibling bounds must still happen when it matters: an
    // unconstrained inner call is typed by the outer parameter.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let d = common::front_end_diagnostics(
        "package sample\nfun take(p: Pair<Any, Set<Any>>): Int = p.second.size\nfun n(): Int = take(\"OK\" to emptySet())\n",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn a_property_qualified_member_call_hands_its_parameter_to_a_nested_generic_call() {
    // `service.create(emptyCrate())` starts with a top-level property, while `local.create(...)`
    // starts with a member property. The parser shape is retained until the root is bound, so both
    // calls use the member's `Crate<String>` parameter to type the nested repository-owned generic
    // call. The classifier-qualified call exercises the non-value interpretation of the same graph
    // node and must not be rerouted through member lookup.
    const MAIN: &str = "class Crate<T>\n\
        fun <T> emptyCrate(): Crate<T> = Crate()\n\
        class Service {\n\
        fun create(name: String, members: Crate<String>): String = name\n\
        }\n\
        fun <T> exec(block: () -> T): T = block()\n\
        fun use(value: String): String = value\n\
        class Factory {\n\
            companion object { fun create(members: Crate<String>): String = \"c\" }\n\
        }\n\
        val service = Service()\n\
        class Holder {\n\
            private val local = Service()\n\
            fun member() =\n\
                exec<String> {\n\
                    val created = local.create(\"b\", emptyCrate())\n\
                    use(created)\n\
                }\n\
        }\n\
        fun topLevel() =\n\
            exec<String> {\n\
                val created = service.create(\"a\", emptyCrate())\n\
                use(created)\n\
            }\n\
        fun qualified() = Factory.create(emptyCrate())\n\
        fun box(): String =\n\
            if (topLevel() == \"a\" && Holder().member() == \"b\" && qualified() == \"c\") \"OK\" else \"FAIL\"\n";
    common::assert_accepted_like_kotlinc(MAIN);
    common::expect_box_same_as_kotlinc(MAIN, "QualifiedNestedGenericCall");
}
