//! Lambda arguments of a MEMBER extension are analyzed like a top-level callee's.
//!
//! Two gaps, both from compiling the kotlin repository's `core/compiler.common`:
//!
//! - A member extension called with a lambda from an expression-bodied member whose result type is
//!   inferred (`fun record() = scoped(1) { tally { 7L } }`). Signature evaluation shaped a bare or
//!   qualified call's postponed lambdas from members, callable properties and top-level callables
//!   only, so the lambda never got an expectation and the module's signatures did not finalize.
//! - A member extension whose later lambda's input mentions a formal an earlier lambda solves
//!   (`selector: (String) -> R` then `printer: (R, R) -> String`). Every lambda was shaped from the
//!   candidate's full instantiation, where the unsolved `R` already stood for its bound, so
//!   `printer` was typed `(Comparable<*>, Comparable<*>) -> String` and returning `value` failed.

use super::common;

const INFERRED_RESULT: &str = r#"fun <T, R> scoped(receiver: T, block: T.() -> R): R = receiver.block()

var sink = 0L

class Meter(val base: Long) {
    fun record() = scoped(1) { tally { 7L } }
    fun recordQualified() = 2.tally { 5L }
    fun total() = scoped(3) { measure { 4L } }
    fun Int.tally(selector: () -> Long) { sink += base + this + selector() }
    fun Int.measure(selector: () -> Long): Long = base + this + selector()
}

fun box(): String {
    val meter = Meter(10L)
    meter.record()
    meter.recordQualified()
    if (sink != 35L) return "tally"
    return if (meter.total() == 17L) "OK" else "measure"
}
"#;

const DEPENDENT_LAMBDAS: &str = r#"class Out {
    var text = ""
    fun add(part: String) { text += part }
}

class Seed {
    fun result(): String = "1"
}

open class Base {
    open fun value(): Any = 0
}

class Derived : Base() {
    override fun value(): String = "OK"
}

fun <T> build(block: Out.() -> T): String {
    val out = Out()
    out.block()
    return out.text
}

fun <R> render(first: String, second: String, selector: (String) -> R, printer: (R, R) -> String) =
    printer(selector(first), selector(second))

fun consume(value: Int?): Int? = value
fun uniqueSeed(value: String): Int = value.length
fun seed(value: String): Int = value.length
fun seed(value: Int): String = value.toString()
fun convert(value: Int): String = value.toString()
fun convert(value: String): Int = value.length
fun concreteSiblings(
    value: Int,
    predicate: (Int) -> Boolean,
    transform: (Int) -> Int,
): Int = if (predicate(value)) transform(value) else value
fun <T> fixedSiblings(value: T, predicate: (T) -> Boolean, transform: (T) -> T): T =
    if (predicate(value)) transform(value) else value

class Pick {
    @Suppress("UNCHECKED_CAST")
    fun <T> String.choose(block: (T) -> String): T {
        val value = length as T
        if (block(value) != (length + 1).toString()) error("block")
        return value
    }

    @Suppress("UNCHECKED_CAST")
    fun <T : Any> String.chooseNonNull(block: (T) -> String): T {
        val value = length as T
        if (block(value) != (length + 1).toString()) error("block")
        return value
    }

    fun <R> String.choose(selector: () -> R, use: (R) -> String): R {
        val value = selector()
        if (use(value) != length.toString()) error("use")
        return value
    }

    @Suppress("UNCHECKED_CAST")
    fun <T, R> String.link(first: (T) -> R, seed: (R) -> T): String {
        first(seed("x" as R))
        return "OK"
    }

    fun <T, R> String.linkReference(first: (T) -> R, seed: () -> T): R = first(seed())

    @Suppress("UNCHECKED_CAST")
    fun <T : Base, R> String.specialize(first: (T) -> R, seed: (R) -> T): R =
        first(seed("x" as R))

    fun expected(): Int = "x".choose { it.inc().toString() }
    fun safeExpected(value: String?): Int? = value?.chooseNonNull { it.inc().toString() }
    fun assertedSafe(value: String?): Int = (value?.chooseNonNull { it.inc().toString() })!!
    fun nestedSafe(value: String?) = consume(value?.chooseNonNull { it.inc().toString() })
    fun reversedOrder() = "xx".choose(use = { it.length.toString() }, selector = { "OK" })
    fun fallbackOrder() = "".link(first = { it.inc().toString() }, seed = { _: String -> 1 })
    fun referenceOrder() = "".link(first = { it.inc().toString() }, seed = ::uniqueSeed)
    fun String.postponedReferenceOrder() = linkReference(first = ::convert, seed = { 1 })
    fun referenceAfterSeed() = "".postponedReferenceOrder()
    fun failedProbeRollback() =
        "".specialize(
            first = { it.value().length },
            seed = { value: Int -> if (value > 0) Derived() else Derived() },
        )
    fun inferredLambdaResultCall() = "41".let { it.toInt() + 1 }
    fun concreteSiblingLambdas() = concreteSiblings(1, { it > 0 }, { it + 1 })
    fun <T> fixedCallerTypeParameter(value: T) =
        fixedSiblings(value, { it == value }, { it })

    fun <R : Comparable<R>> Out.pair(
        first: String,
        second: String,
        selector: (String) -> R,
        printer: (R, R) -> String,
    ) {
        add(render(first, second, selector, printer))
    }

    fun first(): String = build { pair("O", "x", selector = { it }, printer = { value, _ -> value }) }

    fun joined(): String =
        build { pair("O", "K", selector = { it }, printer = { value, other -> value + other }) }

    fun reversed(): String =
        build {
            pair(
                "OK",
                "unused",
                printer = { value, _ -> value.length.toString() },
                selector = { it },
            )
        }

    fun inferred() = build { pair("x", "K", selector = { it }, printer = { _, other -> other }) }

    fun qualified(out: Out): Unit =
        out.pair("K", "K", selector = { it }, printer = { value, other -> other + value })

    fun <R> Out.variance(
        produce: () -> R,
        handler: () -> (R) -> Unit,
        use: (R) -> String,
    ) {
        handler()(produce())
        add(use(produce()))
    }

    fun variance(): String = build {
        variance(
            produce = { 1 },
            handler = { { _: Number -> } },
            use = { it.inc().toString() },
        )
    }

    fun <T, R> Out.chain(
        transform: (T) -> R,
        seed: () -> T,
        use: (R) -> String,
    ) {
        add(use(transform(seed())))
    }

    fun chain(): String = build {
        chain(
            transform = { it.length },
            seed = { "OK" },
            use = { it.inc().toString() },
        )
    }

    fun <T, R> Out.revise(
        first: () -> R,
        transform: (T) -> R,
        seed: () -> T,
        use: (R) -> String,
    ) {
        add(use(first()))
        add(use(transform(seed())))
    }

    fun revised(): String {
        return build {
            revise(
                first = { 1 },
                transform = { it.result() },
                seed = { Seed() },
                use = { it.toString() },
            )
        }
    }
}

fun box(): String {
    val pick = Pick()
    if (pick.expected() != 1) return "expected"
    if (pick.safeExpected("xx") != 2 || pick.safeExpected(null) != null) return "safeExpected"
    if (pick.assertedSafe("xxx") != 3) return "assertedSafe"
    if (pick.nestedSafe("xxx") != 3 || pick.nestedSafe(null) != null) return "nestedSafe"
    if (pick.reversedOrder() != "OK") return "reversedOrder"
    if (pick.fallbackOrder() != "OK") return "fallbackOrder"
    if (pick.referenceOrder() != "OK") return "referenceOrder"
    if (pick.referenceAfterSeed() != "1") return "postponedReferenceOrder"
    if (pick.inferredLambdaResultCall() != 42) return "inferredLambdaResultCall"
    if (pick.concreteSiblingLambdas() != 2) return "concreteSiblingLambdas"
    if (pick.fixedCallerTypeParameter("OK") != "OK") return "fixedCallerTypeParameter"
    if (pick.first() != "O") return "first"
    if (pick.inferred() != "K") return "inferred"
    if (pick.reversed() != "2") return "reversed"
    if (pick.variance() != "2") return "variance"
    if (pick.chain() != "3") return "chain"
    if (pick.revised() != "11") return "revised"
    val out = Out()
    pick.qualified(out)
    if (out.text != "KK") return "qualified"
    return pick.joined()
}
"#;

const PROJECTED_LAMBDA_RESULT: &str = r#"open class Base<T>(val value: T)
class Derived<T>(value: T) : Base<T>(value)

class Out {
    var text = ""
    fun add(part: String) { text += part }
}

fun <T> build(block: Out.() -> T): String {
    val out = Out()
    out.block()
    return out.text
}

class Pick {
    fun <R> Out.project(selector: () -> Base<R>, printer: (R) -> String) {
        add(printer(selector().value))
    }

    fun run(): String = build {
        project(selector = { Derived("OK") }, printer = { value -> value })
    }
}

fun box(): String = Pick().run()
"#;

const NESTED_SAFE_EXPECTATION: &str = r#"fun consume(value: Int?): Int? = value

class Pick {
    @Suppress("UNCHECKED_CAST")
    fun <T : Any> String.choose(block: (T) -> String): T {
        val value = length as T
        block(value)
        return value
    }

    fun nested(value: String?) = consume(value?.choose { it.inc().toString() })
}

fun box(): String {
    val pick = Pick()
    return if (pick.nested("xx") == 2 && pick.nested(null) == null) "OK" else "nested"
}
"#;

const POSTPONED_REFERENCE_ORDER: &str = r#"fun convert(value: Int): String = value.toString()
fun convert(value: String): Int = value.length

class Pick {
    fun <T, R> String.link(first: (T) -> R, seed: () -> T): R = first(seed())
    fun String.postponed() = link(first = ::convert, seed = { 1 })
    fun result() = "".postponed()
}

fun box(): String = if (Pick().result() == "1") "OK" else "reference"
"#;

const FAILED_PROBE_ROLLBACK: &str = r#"open class Base {
    open fun value(): Any = 0
}

class Derived : Base() {
    override fun value(): String = "OK"
}

class Pick {
    @Suppress("UNCHECKED_CAST")
    fun <T : Base, R> String.specialize(first: (T) -> R, seed: (R) -> T): R =
        first(seed("x" as R))

    fun compileOnly() =
        "".specialize(
            first = { it.value().length },
            seed = { value: Int -> if (value > 0) Derived() else Derived() },
        )
}
"#;

const CONTEXTUAL_MEMBER_EXTENSION: &str = r#"class Ctx

class Host {
    context(ctx: Ctx)
    fun String.runIt(block: () -> Long): Long = length + block()

    context(ctx: Ctx)
    fun inferred() = "x".runIt { 1L }
}

fun box(): String = context(Ctx()) {
    if (Host().inferred() == 2L) "OK" else "wrong"
}
"#;

const NESTED_SCOPE_EFFECT: &str = r#"class Ctx
class Extended

class Host {
    context(_: Ctx)
    fun Extended.touch() {}
}

fun box(): String {
    with(Host()) {
        with(Ctx()) {
            Extended().touch()
        }
    }
    return "OK"
}
"#;

const REVISED_RESULT_BOUND: &str = r#"class Seed {
    fun result(): String = "1"
}

class Out {
    var text = ""
    fun add(part: String) { text += part }
}

fun <T> build(block: Out.() -> T): String {
    val out = Out()
    out.block()
    return out.text
}

class Pick {
    fun <T, R> Out.revise(
        first: () -> R,
        transform: (T) -> R,
        seed: () -> T,
        use: (R) -> String,
    ) {
        add(use(first()))
        add(use(transform(seed())))
    }

    fun revised(): String = build {
        revise(
            first = { 1 },
            transform = { it.result() },
            seed = { Seed() },
            use = { it.toString() },
        )
    }
}

fun box(): String {
    val result = Pick().revised()
    return if (result == "11") "OK" else result
}
"#;

#[test]
fn a_member_extension_lambda_in_an_inferred_result_compiles_as_kotlinc_does() {
    common::assert_accepted_like_kotlinc(INFERRED_RESULT);
}

#[test]
fn a_member_extension_lambda_in_an_inferred_result_runs() {
    common::expect_box_same_as_kotlinc(INFERRED_RESULT, "member extension lambda inferred result");
}

#[test]
fn a_member_extension_lambda_sees_a_formal_an_earlier_lambda_solved_as_kotlinc_does() {
    common::assert_accepted_like_kotlinc(DEPENDENT_LAMBDAS);
}

#[test]
fn a_member_extension_lambda_sees_a_formal_an_earlier_lambda_solved() {
    common::expect_box_same_as_kotlinc(DEPENDENT_LAMBDAS, "member extension dependent lambdas");
}

#[test]
fn a_safe_call_carries_an_outer_parameter_expectation_to_its_member_extension() {
    common::expect_box_same_as_kotlinc(NESTED_SAFE_EXPECTATION, "nested safe expectation");
}

#[test]
fn an_independent_seed_disambiguates_an_earlier_callable_reference() {
    common::expect_box_same_as_kotlinc(POSTPONED_REFERENCE_ORDER, "postponed reference order");
}

#[test]
fn a_failed_postponed_probe_keeps_the_productive_siblings_constraints() {
    common::assert_accepted_like_kotlinc(FAILED_PROBE_ROLLBACK);
}

#[test]
fn a_member_extension_projects_an_earlier_lambda_result_as_kotlinc_does() {
    common::assert_accepted_like_kotlinc(PROJECTED_LAMBDA_RESULT);
}

#[test]
fn a_member_extension_projects_an_earlier_lambda_result() {
    common::expect_box_same_as_kotlinc(
        PROJECTED_LAMBDA_RESULT,
        "member extension projected lambda result",
    );
}

#[test]
fn a_contextual_member_extension_lambda_in_an_inferred_result_compiles_as_kotlinc_does() {
    common::assert_accepted_like_kotlinc(CONTEXTUAL_MEMBER_EXTENSION);
}

#[test]
fn a_contextual_member_extension_lambda_in_an_inferred_result_runs() {
    common::expect_box_same_as_kotlinc(
        CONTEXTUAL_MEMBER_EXTENSION,
        "contextual member extension lambda inferred result",
    );
}

#[test]
fn a_nested_scope_lambda_is_not_rechecked_from_its_own_result_as_in_kotlinc() {
    common::assert_accepted_like_kotlinc(NESTED_SCOPE_EFFECT);
}

#[test]
fn a_nested_scope_lambda_is_not_rechecked_from_its_own_result() {
    common::expect_box_same_as_kotlinc(NESTED_SCOPE_EFFECT, "nested scope effect result");
}

#[test]
fn a_productive_recheck_revises_an_already_solved_result_as_in_kotlinc() {
    common::assert_accepted_like_kotlinc(REVISED_RESULT_BOUND);
}

#[test]
fn a_productive_recheck_revises_an_already_solved_result() {
    common::expect_box_same_as_kotlinc(REVISED_RESULT_BOUND, "revised result bound");
}
