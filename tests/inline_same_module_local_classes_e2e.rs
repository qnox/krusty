//! A same-module inline call whose callee's body creates an object of an anonymous class is
//! inlined from the callee's compiled body, as kotlinc compiles it: the caller reuses the
//! callee's object when nothing specializes it, and regenerates it per call site when a reified
//! type argument does. A private inline function's object is outside the public ABI, and its copy
//! takes the call site's scope.

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
