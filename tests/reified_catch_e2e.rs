//! A reified `catch (e: E)` uses the inline call's type argument as its JVM catch type.
//!
//! The clause erases to `E`'s bound while the function is still generic. After inlining,
//! kotlinc catches that argument's class, so a thrown supertype falls through to the outer
//! handler. The classpath case is the same contract for a body that was compiled ahead of its
//! caller: the dependency keeps a mode-7 marker, and a later compilation specializes the
//! exception table, including a forwarded `<T>` that renames the marker to the outer parameter.
//! The same library built here is also run by kotlinc, which specializes the marker this
//! compiler wrote.

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
         fun box(): String {\n\
         \x20   val log = evalCatch<ParentFailure> { throw Throwable() } +\n\
         \x20       evalCatch<ParentFailure> { throw ParentFailure() } +\n\
         \x20       evalCatch<ChildFailure> { throw ParentFailure() } +\n\
         \x20       evalCatch<ChildFailure> { throw ChildFailure() }\n\
         \x20   return if (log == \"NYNY\") \"OK\" else log\n\
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
";

const MAIN: &str = "\
import lib.eval
import lib.forward

open class ParentFailure : Throwable()
class ChildFailure : ParentFailure()

fun box(): String {
    val log = eval<ChildFailure> { throw ParentFailure() } +
        eval<ChildFailure> { throw ChildFailure() } +
        forward<ChildFailure> { throw ParentFailure() } +
        forward<ChildFailure> { throw ChildFailure() }
    return if (log == \"NYNY\") \"OK\" else log
}
";

#[test]
fn a_classpath_reified_catch_specializes_including_a_forwarded_parameter() {
    assert_eq!(
        common::expect_box_run_against_ref("reified_catch_forward", LIB, MAIN).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_kotlinc_reified_catch_specializes_including_a_forwarded_parameter() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).as_deref(),
        Some("OK")
    );
}

#[test]
fn kotlinc_specializes_a_krusty_reified_catch_including_a_forwarded_parameter() {
    let library = common::compile_libs("reified_catch_for_kotlinc", &[("Lib.kt", LIB)])
        .expect("krusty builds the reified catch library");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(MAIN, std::slice::from_ref(&library)),
        "OK"
    );
}
