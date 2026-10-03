//! A reified `catch (e: E)` uses the inline call's type argument as its JVM catch type.
//!
//! The clause erases to `E`'s bound while the function is still generic. After inlining,
//! kotlinc catches that argument's class, so a thrown supertype falls through to the outer
//! handler. The classpath case is the same contract for a body that was compiled ahead of its
//! caller: the dependency keeps a mode-7 marker, and a later compilation specializes the
//! exception table, including a forwarded `<T>` that renames the marker to the outer parameter.
//! Declaration-owned default expressions use the same plan. The same library built here is also
//! run by kotlinc, which specializes the markers this compiler wrote.

use super::common;

#[test]
fn reified_catch_uses_the_call_type_argument() {
    common::expect_box_same_as_kotlinc(
        "// LANGUAGE: +AllowReifiedTypeInCatchClause\n\
         open class ParentFailure : Throwable()\n\
         class ChildFailure : ParentFailure()\n\
         inline fun <reified E : Throwable> evalCatch(block: () -> Nothing): String {\n\
         \x20   return try {\n\
         \x20       try {\n\
         \x20           block()\n\
         \x20       } catch (ignore: E) {\n\
         \x20       }\n\
         \x20       \"Y\"\n\
         \x20   } catch (throwable: Throwable) {\n\
         \x20       \"N\"\n\
         \x20   }\n\
         }\n\
         inline fun <reified E : Throwable> defaultCatch(\n\
         \x20   result: String = try {\n\
         \x20       throw ParentFailure()\n\
         \x20   } catch (ignore: E) {\n\
         \x20       \"Y\"\n\
         \x20   } catch (throwable: Throwable) {\n\
         \x20       \"N\"\n\
         \x20   }\n\
         ): String = result\n\
         fun box(): String {\n\
         \x20   val log = evalCatch<ParentFailure> { throw Throwable() } +\n\
         \x20       evalCatch<ParentFailure> { throw ParentFailure() } +\n\
         \x20       evalCatch<ChildFailure> { throw ParentFailure() } +\n\
         \x20       evalCatch<ChildFailure> { throw ChildFailure() } +\n\
         \x20       defaultCatch<ParentFailure>() +\n\
         \x20       defaultCatch<ChildFailure>()\n\
         \x20   return if (log == \"NYNYYN\") \"OK\" else log\n\
         }\n",
        "ReifiedCatchType",
    );
}

const LIB: &str = "\
// LANGUAGE: +AllowReifiedTypeInCatchClause
package lib

inline fun <reified E : Throwable> eval(block: () -> Nothing): String {
    return try {
        try {
            block()
        } catch (ignore: E) {
        }
        \"Y\"
    } catch (throwable: Throwable) {
        \"N\"
    }
}

inline fun <reified T : Throwable> forward(block: () -> Nothing): String = eval<T>(block)

open class DefaultParentFailure : Throwable()
class DefaultChildFailure : DefaultParentFailure()

inline fun <reified E : Throwable> defaultEval(
    result: String = try {
        throw DefaultParentFailure()
    } catch (ignore: E) {
        \"Y\"
    } catch (throwable: Throwable) {
        \"N\"
    }
): String = result
";

const MAIN: &str = "\
import lib.DefaultChildFailure
import lib.DefaultParentFailure
import lib.defaultEval
import lib.eval
import lib.forward

open class ParentFailure : Throwable()
class ChildFailure : ParentFailure()

fun box(): String {
    val log = eval<ChildFailure> { throw ParentFailure() } +
        eval<ChildFailure> { throw ChildFailure() } +
        forward<ChildFailure> { throw ParentFailure() } +
        forward<ChildFailure> { throw ChildFailure() } +
        defaultEval<DefaultParentFailure>() +
        defaultEval<DefaultChildFailure>()
    return if (log == \"NYNYYN\") \"OK\" else log
}
";

#[test]
fn a_classpath_reified_catch_specializes_forwarding_and_defaults() {
    assert_eq!(
        common::expect_box_run_against_ref("reified_catch_forward", LIB, MAIN).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_kotlinc_reified_catch_specializes_forwarding_and_defaults() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).as_deref(),
        Some("OK")
    );
}

#[test]
fn kotlinc_specializes_a_krusty_reified_catch_forwarding_and_defaults() {
    let library = common::compile_libs("reified_catch_for_kotlinc", &[("Lib.kt", LIB)])
        .expect("krusty builds the reified catch library");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(MAIN, std::slice::from_ref(&library)),
        "OK"
    );
}

const SPLIT_LIB: &str = "\
// LANGUAGE: +AllowReifiedTypeInCatchClause
package lib

var cleanup = -1

inline fun <reified E : Throwable> eval(which: Int, block: () -> Unit): String {
    try {
        if (which == 1) block()
        if (which == 0) return \"S\"
        if (which == 2) block()
        return \"Y\"
    } catch (ignore: E) {
        return \"C\"
    } finally {
        cleanup = which
    }
}
";

const SPLIT_MAIN: &str = "\
import lib.cleanup
import lib.eval

open class ParentFailure : Throwable()
class ChildFailure : ParentFailure()

fun box(): String {
    fun run(which: Int, child: Boolean): String {
        return try {
            eval<ChildFailure>(which) {
                if (child) throw ChildFailure() else throw ParentFailure()
            }
        } catch (throwable: Throwable) {
            \"N\"
        }
    }
    val log = run(1, false) + run(1, true) + run(2, false) + run(2, true)
    return if (cleanup == 2) log else \"cleanup:$cleanup\"
}
";

const SPLIT_SOURCE: &str = concat!(
    "// LANGUAGE: +AllowReifiedTypeInCatchClause\n",
    "open class ParentFailure : Throwable()\n",
    "class ChildFailure : ParentFailure()\n",
    "var cleanup = -1\n",
    "inline fun <reified E : Throwable> eval(which: Int, block: () -> Unit): String {\n",
    "    try {\n",
    "        if (which == 1) block()\n",
    "        if (which == 0) return \"S\"\n",
    "        if (which == 2) block()\n",
    "        return \"Y\"\n",
    "    } catch (ignore: E) {\n",
    "        return \"C\"\n",
    "    } finally {\n",
    "        cleanup = which\n",
    "    }\n",
    "}\n",
    "fun box(): String {\n",
    "    fun run(which: Int, child: Boolean): String {\n",
    "        return try {\n",
    "            eval<ChildFailure>(which) {\n",
    "                if (child) throw ChildFailure() else throw ParentFailure()\n",
    "            }\n",
    "        } catch (throwable: Throwable) {\n",
    "            \"N\"\n",
    "        }\n",
    "    }\n",
    "    val log = run(1, false) + run(1, true) + run(2, false) + run(2, true)\n",
    "    return if (cleanup == 2) log else \"cleanup:$cleanup\"\n",
    "}\n",
);

/// `which == 0` returns from the middle of the `try`, so the protected region is two ranges of
/// one handler. A caller compiled here rewrites both. Kotlinc's inliner rewrites the first typed
/// entry only, so a parent thrown from the second range is still caught as the erasure.
#[test]
fn a_finally_splits_a_reified_catch_across_two_ranges() {
    assert_eq!(
        common::kotlinc_box_result(SPLIT_SOURCE),
        "NCCC",
        "kotlinc rewrites the first split range"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(SPLIT_SOURCE, "ReifiedCatchFinallySplit"),
        "NCNC",
        "every agreeing range is the specialized class"
    );
}

#[test]
fn a_classpath_finally_split_retargets_every_range() {
    assert_eq!(
        common::expect_box_run_against_ref("reified_catch_finally_split", SPLIT_LIB, SPLIT_MAIN)
            .as_deref(),
        Some("NCNC")
    );
}

#[test]
fn kotlinc_leaves_the_later_range_of_a_krusty_finally_split() {
    let library = common::compile_libs("reified_catch_finally_split", &[("Lib.kt", SPLIT_LIB)])
        .expect("krusty builds the split reified catch library");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(SPLIT_MAIN, std::slice::from_ref(&library)),
        "NCCC"
    );
}
