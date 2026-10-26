//! A same-module call whose callee takes a `crossinline` lambda is inlined from the callee's
//! compiled body, as kotlinc compiles it. The literal's body is compiled for the object that keeps
//! it, outside its own class: a property it writes goes through the setter, a private one through a
//! synthetic accessor, and its captured outer instance is the copy's `this$0`. A local return from
//! the literal joins its locals' scope at the lambda method's end. A bound reference's receiver is
//! the regenerated object's `$receiver`, and a lambda class the callee's body declares around the
//! parameter is regenerated with it.

use super::common;

const DECLARATION: &str = r#"interface Consumer {
    fun consume(s: String)
}

inline fun cross(crossinline block: (String) -> Unit): Consumer = object : Consumer {
    override fun consume(s: String) {
        block(s)
    }
}

inline fun <T> readAction(crossinline action: () -> T): T = action()

inline fun produce(crossinline make: () -> String): String = object {
    fun get(): String = make()
}.get()
"#;

const CALLER: &str = r#"class Container {
    var shown = ""
    private var hidden = ""

    fun viaSetter(): Consumer = cross { shown = it }

    fun viaAccessor(): Consumer = cross { hidden = it }

    fun read() = shown + hidden
}

class Named {
    val value = produce(::name)

    fun name() = "named"
}

fun bar(): Int? = 42

class Foo {
    fun <K> infer(): K = "OK" as K
}

fun early(): String = readAction {
    bar() ?: return@readAction "Failed"
    val foo = Foo()
    foo.infer()
}

fun box(): String {
    val container = Container()
    container.viaSetter().consume("O")
    container.viaAccessor().consume("K")
    if (container.read() != "OK") return "container ${container.read()}"
    if (Named().value != "named") return "named ${Named().value}"
    return early()
}
"#;

#[test]
fn a_crossinline_literal_is_inlined_from_the_compiled_body_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declaration.kt", DECLARATION),
        ("Caller.kt", CALLER),
    ]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // The regenerated objects still name the caller's source file and keep the literal's field in
    // their metadata; the classes around them are kotlinc's.
    for name in ["CallerKt", "Container", "DeclarationKt", "Foo", "Named"] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_crossinline_literal_inlined_from_the_compiled_body_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "CallerKt",
    );
}

const WRAPPED_DECLARATION: &str = r#"interface Step {
    fun step()
}

class Steps {
    inline fun first(crossinline f: (Int) -> Any) = object : Step {
        override fun step() {
            f(1)
        }
    }.step()

    inline fun second(crossinline f: (Int) -> Any) = first { f(1) }
}
"#;

const WRAPPED_CALLER: &str = r#"fun box(): String {
    var seen = 0
    Steps().second { seen = it }
    return if (seen == 1) "OK" else "seen $seen"
}
"#;

#[test]
fn a_lambda_class_around_a_crossinline_parameter_is_regenerated_with_the_literal() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", WRAPPED_DECLARATION),
            ("Caller.kt", WRAPPED_CALLER),
        ],
        "CallerKt",
    );
}

const TOP_LEVEL_DECLARATION: &str = r#"interface Consumer {
    fun consume(s: String)
}

inline fun cross(crossinline block: (String) -> Unit): Consumer = object : Consumer {
    override fun consume(s: String) {
        block(s)
    }
}

var log = ""

inline fun note(text: String) {
    log = log + text
}
"#;

const TOP_LEVEL_CALLER: &str = r#"fun logged(): Consumer = cross { log = log + it }

fun box(): String {
    logged().consume("O")
    note("K")
    return log
}
"#;

#[test]
fn a_top_level_property_is_written_through_its_setter_from_code_other_classes_inline() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", TOP_LEVEL_DECLARATION),
            ("Caller.kt", TOP_LEVEL_CALLER),
        ],
        "CallerKt",
    );
}
