//! `try` / `catch` through krusty's own code generator and runtime.
//!
//! An exception propagates through a PENDING SLOT rather than an unwinder: a `throw` records the
//! exception and returns, and every call site checks the slot and branches. `docs/BUILD_AND_NATIVE_PLAN.md`
//! records why — unwind tables need `.eh_frame` and a DWARF interpreter before a single `try` runs,
//! and `setjmp`/`longjmp` returns twice, which Cranelift's SSA cannot express without silently
//! stale registers.
//!
//! What that design has to earn is that a throw is observed at EVERY boundary it crosses, not just
//! the convenient ones. So most of these programs put the throw somewhere the check is easy to
//! forget: behind a call, behind two, inside a constructor, inside a lambda, inside a loop.

use super::common::expect_native_box;

#[test]
fn a_catch_of_the_thrown_type_takes_it() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   return try {\n\
         \x20       throw IllegalStateException(\"boom\")\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message == \"boom\") \"OK\" else \"fail message: ${e.message}\"\n\
         \x20   }\n\
         }\n",
        "CatchExact",
        "OK",
    );
}

#[test]
fn a_try_that_does_not_throw_answers_its_body() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val answer = try { \"OK\" } catch (e: Throwable) { \"fail: caught\" }\n\
         \x20   return answer\n\
         }\n",
        "TryNoThrow",
        "OK",
    );
}

#[test]
fn a_catch_of_a_supertype_takes_it() {
    // The clause matches by `is`, walking the same super chain every other type test here walks.
    expect_native_box(
        "fun box(): String {\n\
         \x20   return try {\n\
         \x20       throw IllegalArgumentException(\"x\")\n\
         \x20   } catch (e: RuntimeException) {\n\
         \x20       \"OK\"\n\
         \x20   }\n\
         }\n",
        "CatchSupertype",
        "OK",
    );
}

#[test]
fn clauses_are_tried_in_source_order() {
    // Kotlin's order and the JVM's: the FIRST clause whose type matches takes it, even when a
    // later one names the exception's own class exactly.
    expect_native_box(
        "fun box(): String {\n\
         \x20   return try {\n\
         \x20       throw IllegalStateException(\"x\")\n\
         \x20   } catch (e: RuntimeException) {\n\
         \x20       \"OK\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       \"fail: later clause\"\n\
         \x20   }\n\
         }\n",
        "ClauseOrder",
        "OK",
    );
}

#[test]
fn a_clause_that_does_not_match_leaves_the_exception_in_flight() {
    // The inner `try` names a type the exception is not, so it does not participate: the exception
    // keeps going to the outer one, which is exactly what "falls through to where it was going"
    // has to mean.
    expect_native_box(
        "fun box(): String {\n\
         \x20   return try {\n\
         \x20       try {\n\
         \x20           throw IllegalStateException(\"x\")\n\
         \x20       } catch (e: NumberFormatException) {\n\
         \x20           \"fail: wrong clause\"\n\
         \x20       }\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       \"OK\"\n\
         \x20   }\n\
         }\n",
        "NoClauseMatches",
        "OK",
    );
}

#[test]
fn a_throw_two_calls_deep_is_caught_at_the_top() {
    // Every frame between the throw and the handler has to observe the pending slot and leave.
    // Miss the check in either and `inner` answers 0 and the program says "fail".
    expect_native_box(
        "fun inner(): Int = throw IllegalStateException(\"deep\")\n\
         fun middle(): Int = inner() + 1\n\
         fun outer(): Int = middle() * 2\n\
         fun box(): String {\n\
         \x20   return try {\n\
         \x20       val n = outer()\n\
         \x20       \"fail: returned $n\"\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       if (e.message == \"deep\") \"OK\" else \"fail message\"\n\
         \x20   }\n\
         }\n",
        "ThrowTwoDeep",
        "OK",
    );
}

#[test]
fn a_stdlib_thrower_is_caught_like_a_written_throw() {
    // `error(m)` IS `throw IllegalStateException(m)` in Kotlin, so a `catch` must not be able to
    // tell them apart. One object through one path is what keeps that true.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var seen = \"\"\n\
         \x20   try { error(\"e\") } catch (e: IllegalStateException) { seen += \"a\" }\n\
         \x20   try { require(false) } catch (e: IllegalArgumentException) { seen += \"b\" }\n\
         \x20   try { TODO() } catch (e: NotImplementedError) { seen += \"c\" }\n\
         \x20   return if (seen == \"abc\") \"OK\" else \"fail: $seen\"\n\
         }\n",
        "StdlibThrowersCaught",
        "OK",
    );
}

#[test]
fn a_caught_exception_does_not_stay_in_flight() {
    // The slot is cleared BEFORE the handler runs. If it were not, the handler's own calls would
    // each see it pending and leave immediately, so nothing after the first would happen -- and
    // the allocation below would be the first thing to notice.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val text = try {\n\
         \x20       throw IllegalStateException(\"x\")\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       var built = \"\"\n\
         \x20       for (i in 1..3) built += \"$i\"\n\
         \x20       built\n\
         \x20   }\n\
         \x20   if (text != \"123\") return \"fail handler: $text\"\n\
         \x20   // And the program carries on normally afterwards.\n\
         \x20   var after = 0\n\
         \x20   for (i in 1..4) after += i\n\
         \x20   return if (after == 10) \"OK\" else \"fail after: $after\"\n\
         }\n",
        "PendingCleared",
        "OK",
    );
}

#[test]
fn a_try_inside_a_loop_catches_on_every_turn() {
    expect_native_box(
        "fun risky(i: Int): Int = if (i % 2 == 0) throw IllegalStateException(\"even\") else i\n\
         fun box(): String {\n\
         \x20   var caught = 0\n\
         \x20   var total = 0\n\
         \x20   for (i in 1..6) {\n\
         \x20       try { total += risky(i) } catch (e: IllegalStateException) { caught += 1 }\n\
         \x20   }\n\
         \x20   return if (caught == 3 && total == 9) \"OK\" else \"fail: $caught/$total\"\n\
         }\n",
        "TryInLoop",
        "OK",
    );
}

#[test]
fn an_uncaught_throw_past_a_try_still_ends_the_program() {
    // A `try` whose clauses do not match must not swallow it: the exception leaves `box` with the
    // slot still set, and the entry reports it exactly as it did before any `try` existed.
    super::common::expect_native_exit(
        "fun box(): String {\n\
         \x20   try {\n\
         \x20       throw IllegalStateException(\"unhandled\")\n\
         \x20   } catch (e: NumberFormatException) {\n\
         \x20       return \"fail: wrong clause\"\n\
         \x20   }\n\
         }\n",
        "UncaughtPastTry",
        134,
        "IllegalStateException: unhandled",
    );
}

#[test]
fn a_null_cast_is_a_null_pointer_exception_naming_the_target_type() {
    // `null as T` and `x!!` are both NullPointerException and differ in their MESSAGE: the cast
    // names the type it could not reach, and `!!` says nothing at all. kotlinc 2.4.10 confirms
    // both, and conflating them is the easy mistake -- a null is not an instance of anything, so
    // there is no class to report as the source of a cast.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val nothing: Any? = null\n\
         \x20   val cast = try { nothing as String; \"fail: no throw\" }\n\
         \x20              catch (e: NullPointerException) { e.message }\n\
         \x20   if (cast != \"null cannot be cast to non-null type kotlin.String\") return \"fail cast: $cast\"\n\
         \x20   val bang = try { nothing!!; \"fail: no throw\" }\n\
         \x20              catch (e: NullPointerException) { e.message ?: \"none\" }\n\
         \x20   return if (bang == \"none\") \"OK\" else \"fail bang: $bang\"\n\
         }\n",
        "NullCastMessage",
        "OK",
    );
}

// ---- `finally` ---------------------------------------------------------------------------------
//
// `finally` runs on EVERY way out of a `try`: normal completion, each handler, an exception nobody
// caught, and a `return`, `break` or `continue` written inside the body. Every expectation below
// was taken from kotlinc 2.4.10 first — several are not what reading the construct suggests.

#[test]
fn a_finally_that_returns_takes_over_from_the_body() {
    expect_native_box(
        "fun f(): String { try { return \"body\" } finally { return \"finally\" } }\n\
         fun box(): String = if (f() == \"finally\") \"OK\" else \"fail: ${f()}\"\n",
        "FinallyReturnWins",
        "OK",
    );
}

#[test]
fn a_finally_that_returns_swallows_the_exception() {
    expect_native_box(
        "fun f(): String { try { throw IllegalStateException(\"x\") } finally { return \"swallowed\" } }\n\
         fun box(): String = if (f() == \"swallowed\") \"OK\" else \"fail: ${f()}\"\n",
        "FinallySwallows",
        "OK",
    );
}

#[test]
fn a_finally_that_throws_replaces_the_exception_in_flight() {
    expect_native_box(
        "fun f() { try { throw IllegalStateException(\"first\") }\n\
         \x20        finally { throw NumberFormatException(\"second\") } }\n\
         fun box(): String {\n\
         \x20   return try { f(); \"fail: no throw\" }\n\
         \x20          catch (e: NumberFormatException) { if (e.message == \"second\") \"OK\" else \"fail: ${e.message}\" }\n\
         \x20          catch (e: IllegalStateException) { \"fail: the first one survived\" }\n\
         }\n",
        "FinallyThrowWins",
        "OK",
    );
}

/// A `try` whose value is a 64-bit scalar, landing where a reference is required. The `try` types
/// itself by its checked result, so the value is boxed on the way — a `Long` passed through as a
/// pointer would be dereferenced as one. kotlinc answers `got 5`.
#[test]
fn a_long_try_in_a_reference_position_is_boxed() {
    expect_native_box(
        "fun f(): Any = try { 5L } catch (e: Exception) { 3L }\n\
         fun g(): Long = 7L\n\
         fun h(): Any {\n\
         \x20   val v: Any = try { g() } catch (e: Exception) { 3L }\n\
         \x20   return v\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = \"got ${f()} ${h()}\"\n\
         \x20   return if (answer == \"got 5 7\") \"OK\" else answer\n\
         }\n",
        "TryLongAsReference",
        "OK",
    );
}
