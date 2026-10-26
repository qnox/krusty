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

// kotlinc's `kt22304`: a callable reference and a lambda in an inline function's body are classes
// of that function's scope, which kotlinc makes public (`inInlineFunctionScope`), so a caller in
// another package can construct them. The caller's literal holds another inlined literal of its
// own, which is code of the literal's body, never a method of the caller.
const REFERENCE_LIBRARY: &str = r#"package library

fun test(): String = "O"

inline fun String.switchMapOnce(crossinline mapper: (String) -> String): String {
    val reference: () -> String = ::test
    return reference() + { mapper(this) }()
}
"#;

const REFERENCE_CALLER: &str = r#"package caller

import library.*

fun box(): String {
    val result = "A".switchMapOnce {
        "B".switchMapOnce { it + "K" }
    }
    return if (result == "OOBK") "OK" else result
}
"#;

#[test]
fn an_inline_functions_reference_class_is_public_to_a_caller_in_another_package() {
    let sources = [
        ("Library.kt", REFERENCE_LIBRARY),
        ("Caller.kt", REFERENCE_CALLER),
    ];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "caller.CallerKt"),
        "OK",
        "kotlinc reference for a reference class constructed from another package",
    );
    common::expect_box_ok_files_with_stdlib(&sources, "caller.CallerKt");
}

// An object's own outer instance and the one an inlined lambda captures are both `this$0`; kotlinc
// renames the object's to `this$0$inline_fun` (`extractParametersMappingAndPatchConstructor`).
// They can be different objects of the same type, or of different types.
const OWN_OUTER_INSTANCE: &str = r#"class A(val s: String) {
    inline fun wrap(crossinline f: () -> String): () -> String = object : () -> String {
        override fun invoke() = s + f()
    }

    fun go(): String = wrap { s + "K" }()
}

class B(val t: String) {
    fun go(a: A): String = a.wrap { t }()
}

fun box(): String {
    val same = A("O").go()
    if (same != "OOK") return "same type $same"
    val other = B("K").go(A("O"))
    if (other != "OK") return "other type $other"
    return "OK"
}
"#;

#[test]
fn an_objects_own_outer_instance_is_renamed_beside_a_lambdas_captured_one() {
    common::expect_box_same_as_kotlinc(OWN_OUTER_INSTANCE, "OwnOuterInstance");
}

// `filter`'s compiled body hands `predicate` to the suspend literal it passes to `source`, which
// this compiler materializes rather than inlining into the object it regenerates. Which lowering
// the call takes is decided from that body before anything is inlined
// (`inliner::keeps_lambda_as_value`), never by inlining it and retrying another route.
const KEPT_AS_VALUE: &str = r#"import kotlin.coroutines.*

interface Sink {
    suspend fun send(item: Int)
}

interface Source {
    suspend fun consume(sink: Sink)
}

inline fun source(crossinline action: suspend Sink.() -> Unit): Source = object : Source {
    override suspend fun consume(sink: Sink) {
        action(sink)
    }
}

suspend inline fun Source.consumeEach(crossinline action: suspend (Int) -> Unit) {
    consume(object : Sink {
        override suspend fun send(item: Int) = action(item)
    })
}

suspend inline fun Source.fold(initial: Int, crossinline operation: suspend (Int, Int) -> Int): Int {
    var acc = initial
    consumeEach {
        acc = operation(acc, it)
    }
    return acc
}

inline fun Source.filter(crossinline predicate: (Int) -> Boolean): Source = source {
    consumeEach {
        if (predicate(it)) send(it)
    }
}

fun range(start: Int, count: Int): Source = source {
    for (i in start until start + count) send(i)
}

fun box(): String {
    var result = 0
    suspend { result = range(1, 4).filter { it % 2 == 0 }.fold(0) { a, b -> a + b } }
        .startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
    return if (result == 6) "OK" else "result $result"
}
"#;

#[test]
fn a_body_keeping_a_crossinline_lambda_as_a_value_is_routed_before_inlining() {
    common::expect_box_same_as_kotlinc(KEPT_AS_VALUE, "KeptAsValue");
}
