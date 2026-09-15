//! The throws a Kotlin program writes on purpose, on a target that has no exceptions.
//!
//! `TODO()`, `error(message)` and a failed `require`/`check` all throw in Kotlin. There is nothing
//! here to catch one — `try` declines whole — so the honest realization is the diagnosable exit
//! this runtime already gives for `!!` on null, a failed cast and an out-of-bounds index. No
//! program that could OBSERVE the difference compiles on this target, which is what makes the
//! substitution sound rather than convenient.
//!
//! Two halves, and both matter: the path that does not throw must run normally, and the one that
//! does must stop the program rather than carry on with a value it does not have.

use super::common::{expect_native_box, expect_native_decline};

#[test]
fn an_unreached_todo_leaves_the_program_alone() {
    expect_native_box(
        "fun pick(n: Int): String = if (n > 0) \"OK\" else TODO()\n\
         fun withReason(n: Int): String = if (n > 0) \"OK\" else TODO(\"not related\")\n\
         fun box(): String = if (pick(1) == \"OK\" && withReason(1) == \"OK\") \"OK\" else \"fail\"\n",
        "UnreachedTodo",
        "OK",
    );
}

#[test]
fn an_unreached_error_leaves_the_program_alone() {
    expect_native_box(
        "fun pick(s: String): String = if (s == \"\") error(\"empty\") else s\n\
         fun box(): String = pick(\"OK\")\n",
        "UnreachedError",
        "OK",
    );
}

#[test]
fn a_satisfied_requirement_is_not_a_failure() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var checks = 0\n\
         \x20   require(1 > 0)\n\
         \x20   checks++\n\
         \x20   check(2 > 1)\n\
         \x20   checks++\n\
         \x20   return if (checks == 2) \"OK\" else \"fail: $checks\"\n\
         }\n",
        "SatisfiedRequirement",
        "OK",
    );
}

#[test]
fn a_todo_is_a_bottom_value_the_caller_never_reads() {
    // `TODO()` is `Nothing`, so a branch that takes it produces no value at all — an `if` whose
    // other branch does still types, and the whole of the diverging branch is unreachable after.
    expect_native_box(
        "fun describe(n: Int): String {\n\
         \x20   val text: String = if (n > 0) \"positive\" else TODO()\n\
         \x20   return text\n\
         }\n\
         fun box(): String = if (describe(3) == \"positive\") \"OK\" else \"fail\"\n",
        "TodoIsABottomValue",
        "OK",
    );
}

#[test]
fn a_message_lambda_still_declines() {
    // `lazyMessage` is a lambda of an `inline` declaration whose body is not here to splice, and
    // Kotlin lets such a lambda return from the enclosing function. Invoking it as an ordinary
    // function value would be a miscompile rather than a slower answer.
    expect_native_decline(
        "fun box(): String {\n\
         \x20   require(1 > 0) { \"never\" }\n\
         \x20   return \"OK\"\n\
         }\n",
        "RequireWithAMessage",
        "require",
    );
}
