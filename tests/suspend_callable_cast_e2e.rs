//! A cast to the continuation-passing function keeps both the carrier and the suspend value.
//!
//! `Foo::bar as Function2<Int, Continuation<Int>, Any?>` still answers `is SuspendFunction1`
//! and `is KSuspendFunction1`. A suspend lambda cast the same way still answers both
//! `is SuspendFunction1` and `is Function2`. After `value as Function2<...>`, `value(1, c)`
//! uses that carrier, and a non-null cast of a nullable suspend value still has the non-null
//! suspend type.

use super::common;

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn a_suspend_callable_keeps_its_type_after_a_continuation_cast() {
    let source = r#"
        import kotlin.coroutines.*
        import kotlin.reflect.KFunction2
        import kotlin.reflect.KSuspendFunction1

        open class EmptyContinuation(
            override val context: CoroutineContext = EmptyCoroutineContext,
        ) : Continuation<Any?> {
            companion object : EmptyContinuation()
            override fun resumeWith(result: Result<Any?>) { result.getOrThrow() }
        }

        class Foo(val x: Int) {
            suspend fun bar(y: Int) = y + x
        }

        fun builder(c: suspend () -> Int): Int {
            var res = 0
            c.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })
            return res
        }

        fun box(): String {
            val ref = Foo(42)::bar
            if ((ref as KFunction2<Int, Continuation<Int>, Any?>)(117, EmptyContinuation) != 159) return "a"
            if ((ref as Function2<Int, Continuation<Int>, Any?>)(117, EmptyContinuation) != 159) return "b"
            if (ref !is KSuspendFunction1<Int, Any?>) return "c"
            if (ref !is SuspendFunction1<Int, Any?>) return "d"
            if (builder { (ref as SuspendFunction1<Int, Int>)(117) } != 159) return "e"
            if (builder { (ref as KSuspendFunction1<Int, Int>)(117) } != 159) return "f"
            if (builder { (ref as suspend (Int) -> Int)(117) } != 159) return "g"

            val ref1 = suspend { x: Int -> x }
            if (ref1 is KSuspendFunction1<Int, Any?>) return "h"
            if (ref1 is KFunction2<*, *, *>) return "i"
            if ((ref1 as Function2<Int, Continuation<Int>, Any?>)(117, EmptyContinuation) != 117) return "j"
            if (ref1 !is SuspendFunction1<Int, Any?>) return "k"
            if (ref1 !is Function2<Int, Continuation<Int>, Any?>) return "l"
            if (builder { (ref1 as SuspendFunction1<Int, Int>)(117) } != 117) return "m"
            if (builder { (ref1 as suspend (Int) -> Int)(117) } != 117) return "n"

            fun cps(value: suspend (Int) -> Int, c: Continuation<Int>): Any? {
                value as Function2<Int, Continuation<Int>, Any?>
                if (value !is SuspendFunction1<Int, Any?>) return "o"
                if (value !is Function2<Int, Continuation<Int>, Any?>) return "p"
                return value(1, c)
            }
            if (cps({ y -> y + 41 }, EmptyContinuation) != 42) return "q"

            fun nonNullSuspend(value: (suspend (Int) -> Int)?): Int {
                value as Function2<Int, Continuation<Int>, Any?>
                val typed: suspend (Int) -> Int = value
                return builder { typed(7) }
            }
            val nullable: (suspend (Int) -> Int)? = { y -> y + 1 }
            if (nonNullSuspend(nullable) != 8) return "r"
            return "OK"
        }
    "#;
    let reflect = reflect_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&reflect));
    assert_eq!(
        common::Fixture::new().with_reflect().run_box(source),
        reference
    );
    assert_eq!(reference, "OK");
}

/// Discovering a function value's callable shape is not itself a non-null proof. Only the checked
/// cast flow above may contribute the non-null constituent.
#[test]
fn a_nullable_function_value_is_not_promoted_by_a_function_expectation() {
    let source = "fun reject(value: (() -> Unit)?) {\n\
        val nonNull: () -> Unit = value\n\
    }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &[]);
    common::expect_identical_rejection(&result, "nullable function expectation");
}
