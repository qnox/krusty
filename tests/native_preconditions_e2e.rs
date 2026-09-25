//! `require`, `check`, `requireNotNull`, `checkNotNull` and `error` in the native backend.
//!
//! Kotlin declares all five `inline`, so a provider holding their bodies splices them and nothing
//! reaches a backend. A klib publishes no body to splice, and the call arrives whole — which is
//! what made every program written with the idiom decline by name.
//!
//! Three facts are Kotlin's and are checked here rather than assumed: the exception each raises
//! (`IllegalArgumentException` for the two `require` forms, `IllegalStateException` for the rest),
//! the wording each uses when the call writes no message, and that the `lazyMessage` block runs
//! ONLY when the check fails. Every expectation below is kotlinc's, taken by running the same
//! program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The passing half, and the value `requireNotNull` answers.
#[test]
fn a_precondition_that_holds_answers_and_runs_no_message() {
    let source = "fun box(): String {\n\
         \x20   var evaluated = 0\n\
         \x20   require(true) { evaluated++; \"unused\" }\n\
         \x20   check(true) { evaluated++; \"unused\" }\n\
         \x20   if (evaluated != 0) return \"fail: a message ran for a check that held\"\n\
         \x20   val maybe: String? = \"value\"\n\
         \x20   val text: String = requireNotNull(maybe)\n\
         \x20   if (text != \"value\") return \"fail requireNotNull answer\"\n\
         \x20   val number: Int = checkNotNull(7 as Int?)\n\
         \x20   if (number != 7) return \"fail checkNotNull answer\"\n\
         \x20   var calls = 0\n\
         \x20   fun once(): String? { calls++; return \"x\" }\n\
         \x20   requireNotNull(once())\n\
         \x20   if (calls != 1) return \"fail: the checked value was evaluated twice\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "PreconditionHolds");
    expect_native_box(source, "PreconditionHolds", "OK");
}

/// The failing half: which exception, and the wording Kotlin fixes for a call that writes no
/// message. A program can read both, so neither is this backend's to choose.
#[test]
fn a_precondition_that_fails_raises_kotlins_own_exception_and_wording() {
    let source = "fun box(): String {\n\
         \x20   try {\n\
         \x20       require(false)\n\
         \x20       return \"fail: require(false) returned\"\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       if (e.message != \"Failed requirement.\") return \"fail require message: \" + e.message\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       check(false)\n\
         \x20       return \"fail: check(false) returned\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message != \"Check failed.\") return \"fail check message: \" + e.message\n\
         \x20   }\n\
         \x20   val absent: String? = null\n\
         \x20   try {\n\
         \x20       requireNotNull(absent)\n\
         \x20       return \"fail: requireNotNull(null) returned\"\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       if (e.message != \"Required value was null.\") return \"fail requireNotNull message: \" + e.message\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       checkNotNull(absent)\n\
         \x20       return \"fail: checkNotNull(null) returned\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message != \"Required value was null.\") return \"fail checkNotNull message: \" + e.message\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "PreconditionFails");
    expect_native_box(source, "PreconditionFails", "OK");
}

/// A written message, rendered the way Kotlin renders it: `toString()` on whatever the block
/// answers, so a number reaches the exception as its decimal spelling.
///
/// And the ordering that makes the block lazy worth stating: it runs after the check, once.
#[test]
fn a_written_message_is_rendered_and_runs_once_after_the_check() {
    let source = "fun box(): String {\n\
         \x20   var ran = 0\n\
         \x20   val n = 41\n\
         \x20   try {\n\
         \x20       require(n > 100) { ran++; n + 1 }\n\
         \x20       return \"fail: require returned\"\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       if (e.message != \"42\") return \"fail rendered message: \" + e.message\n\
         \x20   }\n\
         \x20   if (ran != 1) return \"fail: the message ran \" + ran + \" times\"\n\
         \x20   try {\n\
         \x20       check(false) { \"state \" + n }\n\
         \x20       return \"fail: check returned\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message != \"state 41\") return \"fail check message: \" + e.message\n\
         \x20   }\n\
         \x20   val absent: Int? = null\n\
         \x20   try {\n\
         \x20       checkNotNull(absent) { \"missing \" + n }\n\
         \x20       return \"fail: checkNotNull returned\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message != \"missing 41\") return \"fail checkNotNull message: \" + e.message\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "PreconditionMessage");
    expect_native_box(source, "PreconditionMessage", "OK");
}

/// `error(message)` — no check, an `IllegalStateException`, and the message rendered rather than
/// deferred: it is an ordinary argument, which is what separates it from the four above.
#[test]
fn error_raises_an_illegal_state_exception_around_its_rendered_argument() {
    let source = "fun box(): String {\n\
         \x20   try {\n\
         \x20       error(\"boom \" + 7)\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message != \"boom 7\") return \"fail error message: \" + e.message\n\
         \x20       return \"OK\"\n\
         \x20   }\n\
         \x20   return \"fail: error returned\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ErrorRaises");
    expect_native_box(source, "ErrorRaises", "OK");
}

/// A precondition inside a `try` propagates to that `try`'s handler, not out of the frame: the
/// raise goes through the same store-and-jump an explicit `throw` makes, so the innermost
/// enclosing handler is where it lands.
#[test]
fn a_precondition_inside_a_try_lands_in_that_trys_handler() {
    let source = "fun run(n: Int): String {\n\
         \x20   try {\n\
         \x20       require(n > 0) { \"not positive: \" + n }\n\
         \x20       return \"ok \" + n\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       return \"caught \" + e.message\n\
         \x20   } finally {\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   if (run(1) != \"ok 1\") return \"fail positive\"\n\
         \x20   if (run(-1) != \"caught not positive: -1\") return \"fail negative: \" + run(-1)\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "PreconditionInTry");
    expect_native_box(source, "PreconditionInTry", "OK");
}
