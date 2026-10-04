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
