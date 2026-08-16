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

        // A lambda parameter shaped from a projected receiver is an ordinary value slot.
        fun lambdaEquals(l: List<*>): Boolean = l.any { it == "a" }

        fun lambdaFilter(l: List<*>): Int = l.filter { it == "a" }.size

        // `interface List<out E>` makes the use-site projection redundant, so the member's
        // in-position takes `Any?` rather than collapsing to `Nothing`.
        fun selfIndex(l: List<*>): Int = l.indexOf(l.first())

        fun foreignIndex(l: List<*>): Int = l.indexOf("a")

        fun branchMerge(l: List<*>): Any? = if (l.size > 0) l.first() else null

        class Cell<T>(val value: T)

        fun <T> unwrap(cell: Cell<T>): T = cell.value

        // The projected argument stays applicable to the parameter it inferred.
        fun projectedArgument(cell: Cell<*>): Boolean = unwrap(cell) != "x"

        fun bareFirst(l: List<*>): Boolean = l.first() != "x"

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
            if (!lambdaEquals(l)) return "lambdaEquals"
            if (lambdaFilter(l) != 1) return "lambdaFilter"
            if (selfIndex(l) != 0) return "selfIndex"
            if (foreignIndex(l) != 0) return "foreignIndex"
            if (branchMerge(l) != "a") return "branchMerge"
            if (!projectedArgument(Cell("y"))) return "projectedArgument"
            if (bareFirst(l) != true) return "bareFirst"
            return "OK"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result("StarProjectedMemberRead", SOURCE);
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty =
        common::front_end_diagnostics(SOURCE, std::slice::from_ref(&stdlib), Some(jdk.as_path()));
    assert!(
        krusty.is_empty(),
        "a star-projected member read must type as its bound: {krusty:?}"
    );
    let Some(output) = common::compile_and_run_box(
        SOURCE,
        "StarProjectedMemberRead",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    ) else {
        panic!("expected the star-projection fixture to compile and run");
    };
    assert_eq!(output.trim(), "OK");
}

/// The same rule in the other direction: a WRITE through a projected receiver is prohibited, whether
/// it is spelled as a member or as an extension. `MutableList<T>` is invariant, so `MutableList<*>`
/// keeps the projection and the parameter slot admits nothing — the extension form was accepted
/// until the read and write views came from one position-aware substitution.
#[test]
fn a_write_through_a_projected_receiver_is_rejected() {
    const SOURCE: &str = r#"
        fun <T> MutableList<T>.setFirst(value: T) {
            this[0] = value
        }

        fun member(m: MutableList<*>) {
            m.add("x")
        }

        fun extension(m: MutableList<*>) {
            m.setFirst("x")
        }
    "#;
    let (code, _) = common::kotlinc_source_result("ProjectedReceiverWrite", SOURCE);
    assert_ne!(
        code, 0,
        "kotlinc must reject a write through a star projection"
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty =
        common::front_end_diagnostics(SOURCE, std::slice::from_ref(&stdlib), Some(jdk.as_path()));
    assert_eq!(
        krusty.len(),
        2,
        "both the member and the extension write must be rejected: {krusty:?}"
    );
}
