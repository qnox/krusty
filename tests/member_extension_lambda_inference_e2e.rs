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

fun <T> build(block: Out.() -> T): String {
    val out = Out()
    out.block()
    return out.text
}

fun <R> render(first: String, second: String, selector: (String) -> R, printer: (R, R) -> String) =
    printer(selector(first), selector(second))

class Pick {
    @Suppress("UNCHECKED_CAST")
    fun <T> String.choose(block: (T) -> String): T {
        val value = length as T
        if (block(value) != (length + 1).toString()) error("block")
        return value
    }

    fun expected(): Int = "x".choose { it.inc().toString() }
    fun safeExpected(value: String?): Int? = value?.choose { it.inc().toString() }

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
    val pick = Pick()
    if (pick.expected() != 1) return "expected"
    if (pick.safeExpected("xx") != 2 || pick.safeExpected(null) != null) return "safeExpected"
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

#[test]
fn a_member_extension_lambda_in_an_inferred_result_compiles_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[("Infer.kt", INFERRED_RESULT)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_member_extension_lambda_in_an_inferred_result_runs() {
    common::expect_box_ok_with_stdlib(INFERRED_RESULT, "member extension lambda inferred result");
}

#[test]
fn a_member_extension_lambda_sees_a_formal_an_earlier_lambda_solved_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[("Shape.kt", DEPENDENT_LAMBDAS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_member_extension_lambda_sees_a_formal_an_earlier_lambda_solved() {
    common::expect_box_ok_with_stdlib(DEPENDENT_LAMBDAS, "member extension dependent lambdas");
}

#[test]
fn a_member_extension_projects_an_earlier_lambda_result_as_kotlinc_does() {
    let classes =
        common::classes_against_kotlinc_module(&[("Projected.kt", PROJECTED_LAMBDA_RESULT)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_member_extension_projects_an_earlier_lambda_result() {
    common::expect_box_ok_with_stdlib(
        PROJECTED_LAMBDA_RESULT,
        "member extension projected lambda result",
    );
}

#[test]
fn a_contextual_member_extension_lambda_in_an_inferred_result_compiles_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[(
        "ContextualMemberExtension.kt",
        CONTEXTUAL_MEMBER_EXTENSION,
    )]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_contextual_member_extension_lambda_in_an_inferred_result_runs() {
    common::expect_box_ok_with_stdlib(
        CONTEXTUAL_MEMBER_EXTENSION,
        "contextual member extension lambda inferred result",
    );
}
