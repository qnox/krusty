//! A cast to the continuation-passing function does not erase a suspend callable.
//!
//! `Foo::bar as Function2<Int, Continuation<Int>, Any?>` still answers `is SuspendFunction1`
//! and `is KSuspendFunction1`. A suspend lambda cast the same way still answers both
//! `is SuspendFunction1` and `is Function2`.

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
