//! A value never has a projected type. Reading a member of a star-projected receiver
//! (`Map<*, *>["k"]`) binds the member's type parameter to `out Any?`, and Kotlin approximates that
//! captured result to its bound — `Any?`. Leaving the projection in place produced a type the JVM
//! type mapping cannot name, which reached the comparison emitter as `Ty::Error` and panicked.

use super::common;

#[test]
fn a_star_projected_member_read_is_approximated_to_its_bound() {
    const SOURCE: &str = r#"
        class Holder<T : CharSequence>(val value: T)

        fun <T : CharSequence> Holder<T>.read(): T = value

        fun boundedIn(holder: Holder<in String>): Int = holder.read().length

        fun boundedStar(holder: Holder<*>): Int = holder.read().length

        fun mapValue(m: Map<*, *>): Any? = m["k"]

        fun mapEquals(m: Map<*, *>): Boolean = m["k"] != "v"

        fun listValue(l: List<*>): Any? = l[0]

        fun listEquals(l: List<*>): Boolean = l[0] == "a"

        fun anyMapEquals(a: Any): Boolean = (a as Map<*, *>)["k"] != "v"

        fun bareFirst(l: List<*>): Boolean = l.first() != "x"

        fun branchMerge(l: List<*>): Any? = if (l.size > 0) l.first() else null

        fun box(): String {
            val m: Map<String, String> = mapOf("k" to "v")
            val l: List<String> = listOf("a")
            if (boundedIn(Holder("bound")) != 5) return "boundedIn"
            if (boundedStar(Holder("bound")) != 5) return "boundedStar"
            if (mapValue(m) != "v") return "mapValue"
            if (mapEquals(m)) return "mapEquals"
            if (mapEquals(mapOf("k" to "other")) != true) return "mapEquals other"
            if (listValue(l) != "a") return "listValue"
            if (!listEquals(l)) return "listEquals"
            if (anyMapEquals(m)) return "anyMapEquals"
            if (bareFirst(l) != true) return "bareFirst"
            if (branchMerge(l) != "a") return "branchMerge"
            return "OK"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result("StarProjectedMemberRead", SOURCE);
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    let krusty = common::front_end_diagnostics_with_stdlib(SOURCE);
    assert!(
        krusty.is_empty(),
        "a star-projected member read must type as its bound: {krusty:?}"
    );
    let Some(output) = common::compile_and_run_with_stdlib(SOURCE, "StarProjectedMemberRead")
    else {
        panic!("expected the star-projection fixture to compile and run");
    };
    assert_eq!(output.trim(), "OK");
}
