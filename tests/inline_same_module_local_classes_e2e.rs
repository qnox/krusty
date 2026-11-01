//! A same-module inline call whose callee's body creates an object of an anonymous class is
//! inlined from the callee's compiled body, as kotlinc compiles it: the caller reuses the
//! callee's object when nothing specializes it, and regenerates it per call site when a reified
//! type argument does. A private inline function's object is outside the public ABI, and its copy
//! takes the call site's scope. An object a regenerated copy's constructor creates is regenerated
//! with it, as the copy's first object.

use super::common;

const DECLARATION: &str = r#"interface Named {
    fun name(): String
}

inline fun named(): Named = object : Named {
    override fun name() = "O"
}

inline fun counted(times: Int): Int {
    val counter = object {
        var count = 0
    }
    repeat(times) { counter.count++ }
    return counter.count
}

class Box<T>

private inline fun <reified T> Box<T>.tagged(): Named = object : Named {
    override fun name() = T::class.simpleName!!
}

fun tags(): String = Box<String>().tagged().name() + Box<Int>().tagged().name()
"#;

const CALLER: &str = r#"fun box(): String {
    val first = named()
    val second = named()
    if (counted(3) != 3) return "counted"
    if (tags() != "StringInt") return "tags ${tags()}"
    return if (second.name() == "O") first.name() + "K" else "second"
}
"#;

#[test]
fn an_inline_bodys_objects_are_inlined_from_the_compiled_body_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declaration.kt", DECLARATION),
        ("Caller.kt", CALLER),
    ]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn an_inline_bodys_objects_inlined_from_the_compiled_body_run() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "CallerKt",
    );
}

const NESTED: &str = r#"interface Named {
    fun name(): String
}

inline fun <reified T> make(): Named = object : Named {
    val type = T::class
    val nested: Named = object : Named {
        override fun name() = "nested"
    }

    override fun name() = nested.name()
}

fun box(): String = make<String>().name().let { if (it == "nested") "OK" else it }
"#;

#[test]
fn an_object_a_regenerated_constructor_creates_is_regenerated_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Nested.kt", NESTED)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn an_object_a_regenerated_constructor_creates_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Nested.kt", NESTED)], "NestedKt");
}

/// kotlinc declares a copy's fields before it regenerates the constructor's objects, so a field of
/// an anonymous type keeps the original's type while the constructor stores the copy: the program
/// fails as kotlinc's does.
const UNTYPED_NESTED: &str = r#"inline fun <reified T> untyped(): Any = object {
    val type = T::class
    val nested = object {}
}

fun box(): String = if (untyped<Int>() is Any) "OK" else "untyped"
"#;

#[test]
fn an_object_of_anonymous_type_a_regenerated_constructor_creates_fails_as_kotlinc_does() {
    let reference = common::kotlinc_box_result(UNTYPED_NESTED);
    assert_eq!(
        reference,
        "ERROR:NoSuchFieldError:Class MainKt$box$$inlined$untyped$1 does not have member field \
         'MainKt$box$$inlined$untyped$1$1 nested'"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(UNTYPED_NESTED, "Main"),
        reference
    );
}

/// A regenerated object's suspend method constructs its continuation, which is regenerated with
/// the object; the copy's method names the copied continuation from its first instruction.
const SUSPEND_METHOD: &str = r#"import kotlin.coroutines.*

interface Named {
    suspend fun name(): String
}

var pending: Any? = null

inline fun <reified T> make(): Named = object : Named {
    override suspend fun name(): String {
        suspendCoroutine<Unit> { pending = it }
        return T::class.simpleName!!
    }
}

fun box(): String {
    var result = ""
    suspend { make<String>().name() }.startCoroutine(Continuation(EmptyCoroutineContext) {
        result = it.getOrThrow()
    })
    @Suppress("UNCHECKED_CAST")
    (pending as Continuation<Unit>).resume(Unit)
    return if (result == "String") "OK" else result
}
"#;

#[test]
fn a_regenerated_objects_suspend_method_takes_a_regenerated_continuation_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Suspending.kt", SUSPEND_METHOD)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // A continuation declared in an inline function's scope still carries krusty's protected
    // synthetic-class visibility in its metadata, where kotlinc records a local, public-ABI one.
    for name in [
        "Named",
        "SuspendingKt",
        "SuspendingKt$box$1",
        "SuspendingKt$box$1$invokeSuspend$$inlined$make$1",
        "SuspendingKt$make$1",
    ] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_regenerated_objects_suspend_method_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Suspending.kt", SUSPEND_METHOD)], "SuspendingKt");
}
