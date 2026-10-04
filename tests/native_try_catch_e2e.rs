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
fn a_throw_from_a_constructor_is_caught() {
    // A constructor is a generated function body like any other, and it propagates the same way.
    expect_native_box(
        "class Checked(val n: Int) {\n\
         \x20   init { if (n < 0) throw IllegalArgumentException(\"negative\") }\n\
         }\n\
         fun box(): String {\n\
         \x20   return try {\n\
         \x20       Checked(-1)\n\
         \x20       \"fail: constructed\"\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       if (e.message == \"negative\") \"OK\" else \"fail message\"\n\
         \x20   }\n\
         }\n",
        "ThrowFromConstructor",
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
fn a_failed_cast_names_both_classes_the_way_kotlin_native_does() {
    expect_native_box(
        "class MyObject\n\
         fun box(): String {\n\
         \x20   try {\n\
         \x20       MyObject() as String\n\
         \x20   } catch (e: ClassCastException) {\n\
         \x20       val said = e.message\n\
         \x20       return if (said == \"class MyObject cannot be cast to class kotlin.String\") \"OK\"\n\
         \x20              else \"fail: $said\"\n\
         \x20   }\n\
         \x20   return \"fail: no throw\"\n\
         }\n",
        "CastMessage",
        "OK",
    );
}

#[test]
fn a_failed_cast_names_a_local_or_anonymous_class_the_way_kotlin_native_does() {
    // The frontend hands a local classifier over with an opaque identity and the naming
    // provenance each target spells for itself. Kotlin/Native has no facade class, so a class
    // local to `box` is `box$MyLocalObject` and the first anonymous object in it `box$1` -- the
    // names the corpus's `casts/nativeCCEMessage` cases pin, never the JVM's `...Kt$box$1` or
    // the frontend's placeholder.
    expect_native_box(
        "class MyObject\n\
         fun box(): String {\n\
         \x20   class MyLocalObject\n\
         \x20   var said = \"\"\n\
         \x20   try { MyLocalObject() as MyObject } catch (e: ClassCastException) { said += \"${e.message}|\" }\n\
         \x20   try { MyObject() as MyLocalObject } catch (e: ClassCastException) { said += \"${e.message}|\" }\n\
         \x20   try { object {} as MyObject } catch (e: ClassCastException) { said += \"${e.message}|\" }\n\
         \x20   val expected = \"class box\\$MyLocalObject cannot be cast to class MyObject|\" +\n\
         \x20       \"class MyObject cannot be cast to class box\\$MyLocalObject|\" +\n\
         \x20       \"class box\\$1 cannot be cast to class MyObject|\"\n\
         \x20   return if (said == expected) \"OK\" else \"fail: $said\"\n\
         }\n",
        "LocalCastMessage",
        "OK",
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

#[test]
fn reading_a_lateinit_property_before_it_is_set_throws() {
    // The guard is at the READ, which is where kotlinc puts it: the field being null is the only
    // evidence there is, and it is why `lateinit` is confined to types that have a null.
    expect_native_box(
        "class Holder {\n\
         \x20   lateinit var text: String\n\
         \x20   fun read(): String = text\n\
         }\n\
         fun box(): String {\n\
         \x20   val holder = Holder()\n\
         \x20   val said = try {\n\
         \x20       holder.read()\n\
         \x20       \"fail: read succeeded\"\n\
         \x20   } catch (e: UninitializedPropertyAccessException) {\n\
         \x20       e.message\n\
         \x20   }\n\
         \x20   if (said != \"lateinit property text has not been initialized\") return \"fail: $said\"\n\
         \x20   holder.text = \"set\"\n\
         \x20   return if (holder.text == \"set\") \"OK\" else \"fail after set\"\n\
         }\n",
        "LateinitGuard",
        "OK",
    );
}

#[test]
fn valueof_of_an_unknown_constant_throws() {
    expect_native_box(
        "enum class Color { RED, GREEN }\n\
         fun box(): String {\n\
         \x20   if (Color.valueOf(\"RED\") != Color.RED) return \"fail known\"\n\
         \x20   return try {\n\
         \x20       Color.valueOf(\"BLUE\")\n\
         \x20       \"fail: no throw\"\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       \"OK\"\n\
         \x20   }\n\
         }\n",
        "ValueOfUnknown",
        "OK",
    );
}

#[test]
fn a_throw_inside_an_invoked_lambda_reaches_the_try_around_the_call() {
    // A DISPATCHED call is as able to throw as a direct one and easier to forget: it is the one
    // call this backend emits that does not go through the ordinary call helper. `expectFail { … }`
    // is the corpus's own shape and the exception walked straight out of the `try` until the
    // indirect call checked too.
    expect_native_box(
        "fun expectFail(f: () -> Unit): String {\n\
         \x20   try {\n\
         \x20       f()\n\
         \x20   } catch (e: ArithmeticException) {\n\
         \x20       return \"caught\"\n\
         \x20   }\n\
         \x20   return \"fail: no throw\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val zero = 0\n\
         \x20   val one = expectFail { 1 / zero }\n\
         \x20   val two = expectFail { 2 * (1 / zero) }\n\
         \x20   return if (one == \"caught\" && two == \"caught\") \"OK\" else \"fail: $one/$two\"\n\
         }\n",
        "ThrowThroughLambda",
        "OK",
    );
}

#[test]
fn a_lateinit_property_that_overrides_one_is_guarded_too() {
    // The read goes through a vtable slot because the property overrides an interface's, so it
    // reaches the synthesized getter rather than the field directly. The guard has to be on what
    // that getter reads, not on the shape of the read.
    expect_native_box(
        "interface Named { val label: String }\n\
         class Tagged : Named {\n\
         \x20   override lateinit var label: String\n\
         \x20   fun read(): String = label\n\
         }\n\
         fun box(): String {\n\
         \x20   val tagged = Tagged()\n\
         \x20   val said = try { tagged.read(); \"fail: read succeeded\" }\n\
         \x20              catch (e: UninitializedPropertyAccessException) { \"caught\" }\n\
         \x20   if (said != \"caught\") return \"fail: $said\"\n\
         \x20   tagged.label = \"set\"\n\
         \x20   return if (tagged.read() == \"set\") \"OK\" else \"fail after set\"\n\
         }\n",
        "LateinitOverride",
        "OK",
    );
}

#[test]
fn a_cast_between_two_primitives_can_only_be_an_erased_object_cast() {
    // Kotlin has no cast between two primitive types -- `val x: Int = 1; x as Byte` does not
    // compile -- so when one reaches this backend the source was a type PARAMETER that the call
    // substituted. The question it is really asking is the one the value's own box answers, and
    // unboxing and converting instead would answer `1` where Kotlin raises ClassCastException.
    expect_native_box(
        "fun <T> check(param: T, f: (T) -> Unit): String {\n\
         \x20   try { f(param) } catch (e: ClassCastException) { return \"threw\" }\n\
         \x20   return \"quiet\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = check(1, { it as Byte })\n\
         \x20   val b = check(1, { it as Int })\n\
         \x20   return if (a == \"threw\" && b == \"quiet\") \"OK\" else \"fail: $a/$b\"\n\
         }\n",
        "ErasedPrimitiveCast",
        "OK",
    );
}

#[test]
fn assert_fails_with_answers_the_exception_the_block_threw() {
    // The reified `T` never reaches the backend as a type argument: kotlinc resolves it into the
    // call's RETURN type, so the class to test against is read from there.
    expect_native_box(
        "import kotlin.test.assertFailsWith\n\
         fun box(): String {\n\
         \x20   val e = assertFailsWith<IllegalStateException> { error(\"boom\") }\n\
         \x20   return if (e.message == \"boom\") \"OK\" else \"fail: ${e.message}\"\n\
         }\n",
        "AssertFailsWith",
        "OK",
    );
}

#[test]
fn assert_fails_with_a_supertype_takes_it() {
    // The test is `is_instance`, the same one a `catch` clause makes, so a supertype matches.
    // kotlinc 2.4.10 confirms: `assertFailsWith<RuntimeException>` takes an IllegalStateException.
    expect_native_box(
        "import kotlin.test.assertFailsWith\n\
         fun box(): String {\n\
         \x20   val e = assertFailsWith<RuntimeException> { error(\"sup\") }\n\
         \x20   return if (e.message == \"sup\") \"OK\" else \"fail: ${e.message}\"\n\
         }\n",
        "AssertFailsWithSupertype",
        "OK",
    );
}

#[test]
fn a_block_that_completes_fails_the_assertion() {
    expect_native_box(
        "import kotlin.test.assertFailsWith\n\
         fun box(): String {\n\
         \x20   return try {\n\
         \x20       assertFailsWith<IllegalStateException> { }\n\
         \x20       \"fail: no throw\"\n\
         \x20   } catch (e: AssertionError) {\n\
         \x20       val want = \"Expected an exception of class kotlin.IllegalStateException\" +\n\
         \x20                  \" to be thrown, but was completed successfully.\"\n\
         \x20       if (e.message == want) \"OK\" else \"fail: ${e.message}\"\n\
         \x20   }\n\
         }\n",
        "AssertFailsWithNoThrow",
        "OK",
    );
}

#[test]
fn a_block_that_throws_the_wrong_type_fails_the_assertion_rather_than_propagating() {
    // The plausible reading is that the unexpected exception travels on. kotlin-test catches
    // `Throwable` and fails the assertion with what it caught, so the original is REPLACED — and
    // kotlinc 2.4.10 is what settled it, not the reading.
    expect_native_box(
        "import kotlin.test.assertFailsWith\n\
         fun box(): String {\n\
         \x20   return try {\n\
         \x20       assertFailsWith<IllegalStateException> { throw NumberFormatException(\"nfe\") }\n\
         \x20       \"fail: no throw\"\n\
         \x20   } catch (e: NumberFormatException) {\n\
         \x20       \"fail: the wrong exception propagated\"\n\
         \x20   } catch (e: AssertionError) {\n\
         \x20       val want = \"Expected an exception of class kotlin.IllegalStateException\" +\n\
         \x20                  \" to be thrown, but was kotlin.NumberFormatException: nfe\"\n\
         \x20       if (e.message == want) \"OK\" else \"fail: ${e.message}\"\n\
         \x20   }\n\
         }\n",
        "AssertFailsWithWrongType",
        "OK",
    );
}

#[test]
fn a_supplied_message_is_a_prefix() {
    // `message` is declared BEFORE the block and defaulted, so the block is the LAST argument and
    // never the first — a call that supplies a message passes two.
    expect_native_box(
        "import kotlin.test.assertFailsWith\n\
         fun box(): String {\n\
         \x20   return try {\n\
         \x20       assertFailsWith<IllegalStateException>(\"mine\") { }\n\
         \x20       \"fail: no throw\"\n\
         \x20   } catch (e: AssertionError) {\n\
         \x20       val want = \"mine. Expected an exception of class kotlin.IllegalStateException\" +\n\
         \x20                  \" to be thrown, but was completed successfully.\"\n\
         \x20       if (e.message == want) \"OK\" else \"fail: ${e.message}\"\n\
         \x20   }\n\
         }\n",
        "AssertFailsWithMessage",
        "OK",
    );
}

#[test]
fn a_null_cast_to_an_erased_type_parameter_names_it_by_its_owner() {
    // The descriptor is erased, the name is not. Kotlin/Native 2.4.20 raises this exception with no
    // message; krusty renders the target as the JVM backend does, except that a target without file
    // facades keeps a top-level function in its package: `T of p.generic` where the JVM says
    // `T of p.ErasedNullCastKt.generic`.
    expect_native_box(
        "package p\n\
         fun <T : Any> generic(a: Any?): T = a as T\n\
         fun box(): String {\n\
         \x20   val cast = try { generic<String>(null); \"no throw\" }\n\
         \x20              catch (e: NullPointerException) { e.message ?: \"none\" }\n\
         \x20   if (cast != \"null cannot be cast to non-null type T of p.generic\") return cast\n\
         \x20   return if (generic<String>(\"k\") == \"k\") \"OK\" else \"fail: non-null\"\n\
         }\n",
        "ErasedNullCast",
        "OK",
    );
}

// ---- `finally` ---------------------------------------------------------------------------------
//
// `finally` runs on EVERY way out of a `try`: normal completion, each handler, an exception nobody
// caught, and a `return`, `break` or `continue` written inside the body. Every expectation below
// was taken from kotlinc 2.4.10 first — several are not what reading the construct suggests.

#[test]
fn a_finally_runs_after_the_body_completes() {
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): String { try { log.append(\"t\"); return \"N\" } finally { log.append(\"f\") } }\n\
         fun box(): String {\n\
         \x20   val answer = f()\n\
         \x20   return if (answer == \"N\" && log.toString() == \"tf\") \"OK\" else \"fail: $answer/$log\"\n\
         }\n",
        "FinallyNormal",
        "OK",
    );
}

#[test]
fn a_finally_runs_after_the_handler_that_caught() {
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): String {\n\
         \x20   try { log.append(\"t\"); throw IllegalStateException(\"x\") }\n\
         \x20   catch (e: IllegalStateException) { log.append(\"c\"); return \"C\" }\n\
         \x20   finally { log.append(\"f\") }\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = f()\n\
         \x20   return if (answer == \"C\" && log.toString() == \"tcf\") \"OK\" else \"fail: $answer/$log\"\n\
         }\n",
        "FinallyAfterCatch",
        "OK",
    );
}

#[test]
fn a_finally_runs_while_an_exception_is_travelling_and_then_it_carries_on() {
    // The slot has to be CLEARED around the block: the finally's own calls each check it, so
    // leaving it set would make the first of them turn straight round and the block would not run.
    // kotlinc confirms a finally may call whatever it likes here.
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): String { try { throw IllegalStateException(\"p\") } finally { log.append(\"f\") } }\n\
         fun box(): String {\n\
         \x20   val said = try { f() } catch (e: IllegalStateException) { e.message }\n\
         \x20   return if (said == \"p\" && log.toString() == \"f\") \"OK\" else \"fail: $said/$log\"\n\
         }\n",
        "FinallyThenPropagate",
        "OK",
    );
}

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

#[test]
fn a_break_out_of_a_try_runs_its_finally() {
    // The finally belongs to a `try` INSIDE the loop, so the breaking turn runs it like the
    // others: kotlinc logs `0ff` for three turns where the second breaks.
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): String {\n\
         \x20   for (i in 0..2) {\n\
         \x20       try { if (i == 1) break; log.append(\"$i\") } finally { log.append(\"f\") }\n\
         \x20   }\n\
         \x20   return \"L\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = f()\n\
         \x20   return if (answer == \"L\" && log.toString() == \"0ff\") \"OK\" else \"fail: $answer/$log\"\n\
         }\n",
        "FinallyOnBreak",
        "OK",
    );
}

#[test]
fn a_continue_out_of_a_try_runs_its_finally() {
    expect_native_box(
        "val log = StringBuilder()\n\
         fun box(): String {\n\
         \x20   for (i in 0..2) {\n\
         \x20       try { if (i == 1) continue; log.append(\"$i\") } finally { log.append(\"f\") }\n\
         \x20   }\n\
         \x20   // `0f` for the first turn, `f` alone for the one that continues, `2f` for the last.\n\
         \x20   return if (log.toString() == \"0ff2f\") \"OK\" else \"fail: $log\"\n\
         }\n",
        "FinallyOnContinue",
        "OK",
    );
}

#[test]
fn nested_finallys_run_innermost_first() {
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): String {\n\
         \x20   try { try { return \"r\" } finally { log.append(\"inner\") } } finally { log.append(\"outer\") }\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = f()\n\
         \x20   return if (answer == \"r\" && log.toString() == \"innerouter\") \"OK\" else \"fail: $answer/$log\"\n\
         }\n",
        "NestedFinallys",
        "OK",
    );
}

#[test]
fn a_try_finally_with_no_catch_still_propagates() {
    expect_native_box(
        "val log = StringBuilder()\n\
         fun f(): Int { try { return 1 } finally { log.append(\"a\") } }\n\
         fun box(): String {\n\
         \x20   val n = f()\n\
         \x20   val caught = try {\n\
         \x20       try { throw IllegalStateException(\"z\") } finally { log.append(\"b\") }\n\
         \x20   } catch (e: IllegalStateException) { e.message }\n\
         \x20   return if (n == 1 && caught == \"z\" && log.toString() == \"ab\") \"OK\" else \"fail: $n/$caught/$log\"\n\
         }\n",
        "TryFinallyNoCatch",
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
