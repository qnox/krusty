//! A builder variable whose only evidence comes from a nested lambda keeps that evidence. The nested
//! lambda's result is the builder member's own `V?`, so once the nested call's result variable is
//! replaced, the lambda is related to a parameter of the identical type. That relation is trivially
//! true and must not join `V` into its own solution: the builder call still fixes `V` to `Int`
//! when it is the receiver of another call or initializes an unannotated local.

use super::common;

const SOURCE: &str = r#"fun <T> T.same(): T = this
fun <R> runPlain(block: () -> R): R = block()

class Buildee<T> {
    var item: T? = null
    fun yield(value: T): T? {
        item = value
        return null
    }
}

fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> {
    val result = Buildee<T>()
    result.block()
    return result
}

fun letReceiver(x: String?) = build { x?.let { yield(1) } }.same()
fun plainLambdaReceiver() = build { runPlain { yield(2) } }.same()

fun localInitializer(): Buildee<Int> {
    val built = build { runPlain { yield(4) } }
    return built
}

// One stdlib integration case verifies that the same generic rule reaches library declarations.
fun libraryReceiver(x: String?): Map<String, Int>? = buildMap { x?.let { put("a", 1) } }.ifEmpty { null }

// The recursive call reuses `fill`'s own type-parameter identity: inside it, `T` is the enclosing
// declaration's fixed parameter, so `() -> T?` against `() -> T?` is real evidence there.
fun <T> fill(seed: T, depth: Int, block: Buildee<T>.() -> Unit): Buildee<T> {
    val result = Buildee<T>()
    result.block()
    if (depth > 0) {
        val nested: Buildee<T> = fill(seed, depth - 1) { runPlain { yield(seed) } }
        result.yield(nested.item!!)
    }
    return result
}

fun box(): String {
    val a: Buildee<Int> = letReceiver("x")
    if (a.item != 1) return "let"
    val b: Buildee<Int> = plainLambdaReceiver()
    if (b.item != 2) return "plain"
    if (localInitializer().item != 4) return "local"
    if (libraryReceiver("x")?.get("a") != 1) return "library"
    if (libraryReceiver(null) != null) return "library-empty"
    if (fill("s", 1) { yield("t") }.item != "s") return "recursive"
    return "OK"
}
"#;

/// Repository-owned builder calls in receiver and local-initializer position whose value variable
/// is constrained only through a nested lambda, plus one stdlib integration case and a recursive
/// call whose fresh variable receives its enclosing declaration's fixed `T` as evidence.
#[test]
fn nested_lambda_result_keeps_builder_value_evidence() {
    common::assert_accepted_like_kotlinc(SOURCE);
    common::expect_box_same_as_kotlinc(SOURCE, "BuilderNestedLambdaResultRun");
}

const SAME_BUILDER_SOURCE: &str = r#"
class Built<T>(val value: T?)

class Scope<T> {
    fun accept(value: T) {}
    fun <X> take(value: Built<X>) {}
}

fun <T> build(block: Scope<T>.() -> Unit): Built<T> {
    Scope<T>().block()
    return Built(null)
}

val nested: Built<Int> = build {
    accept(1)
    take(build {
        accept("inner")
    })
}

fun box(): String = "OK"
"#;

/// Two simultaneously active invocations of the same generic builder own different postponed
/// variables: the outer `T = Int` must not specialize the inner receiver before it records
/// `T = String`.
#[test]
fn nested_calls_to_the_same_builder_keep_distinct_variables() {
    common::assert_accepted_like_kotlinc(SAME_BUILDER_SOURCE);
    common::expect_box_same_as_kotlinc(SAME_BUILDER_SOURCE, "NestedSameBuilderVariables");
}
