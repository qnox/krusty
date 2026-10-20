//! A same-module inline member is inlined from its compiled body, as kotlinc's
//! `IrSourceCompilerForInline` compiles the member into its own class and hands that `MethodNode`
//! to the inliner: the dispatch receiver is the body's slot 0, stored from the caller's receiver
//! operand like any other parameter.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A public member read from a top-level caller and from a sibling member of its own class.
const INSTANCE_MEMBER: &str = r#"class Counter(var count: Int) {
    inline fun <T> bump(x: T): T {
        count += 1
        return x
    }

    fun twice(): Int = bump(bump(1) + 1)
}

fun box(): String {
    val counter = Counter(0)
    val text = counter.bump("O") + "K"
    if (counter.twice() != 2) return "twice"
    return if (counter.count == 3) text else "count ${counter.count}"
}
"#;

#[test]
fn an_instance_member_is_inlined_from_its_compiled_body() {
    assert_like_kotlinc(&[("Member.kt", INSTANCE_MEMBER)]);
}

#[test]
fn an_instance_member_inlined_from_its_compiled_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(INSTANCE_MEMBER, "Member"),
        "OK"
    );
}

/// A companion member and an object member are instance methods of their singleton class; the
/// caller's receiver operand is the singleton's instance.
const SINGLETON_MEMBERS: &str = r#"class Host {
    companion object {
        inline fun <T> wrap(x: T): List<T> = listOf(x)
    }
}

object Registry {
    var hits = 0
    inline fun <T> record(x: T): T {
        hits += 1
        return x
    }
}

fun box(): String {
    val wrapped = Host.wrap("O")
    val recorded = Registry.record("K")
    if (Registry.hits != 1) return "hits ${Registry.hits}"
    return wrapped.single() + recorded
}
"#;

#[test]
fn singleton_members_are_inlined_from_their_compiled_bodies() {
    assert_like_kotlinc(&[("Singletons.kt", SINGLETON_MEMBERS)]);
}

#[test]
fn singleton_members_inlined_from_their_compiled_bodies_run() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SINGLETON_MEMBERS, "Singletons"),
        "OK"
    );
}

/// A nested class inlines its outer class's private inline member: kotlinc compiles that member's
/// body with the outer class's synthetic accessor for the private property it reads, since the
/// body runs in the nested class.
const PRIVATE_MEMBER: &str = r#"class Probe(private val value: Any?) {
    private inline fun <reified T> holds(): Boolean = value is T

    fun own(): Boolean = holds<String>()

    class Reader {
        fun read(p: Probe): Boolean = p.holds<Other>()
    }
}

class Other

fun box(): String {
    if (!Probe.Reader().read(Probe(Other()))) return "reader"
    if (Probe.Reader().read(Probe(null))) return "null"
    return if (Probe("x").own()) "OK" else "own"
}
"#;

#[test]
fn a_private_member_inlined_into_a_nested_class_reads_through_an_accessor() {
    assert_like_kotlinc(&[("Private.kt", PRIVATE_MEMBER)]);
}

/// A value class's inline member is its class's static `-impl`, whose first parameter is the
/// unboxed receiver.
const VALUE_CLASS_MEMBER: &str = r#"@JvmInline
value class Meters(val value: Int) {
    inline fun <T> scaled(by: Int, tag: T): Pair<Int, T> = Pair(value * by, tag)
}

fun box(): String {
    val (scaled, tag) = Meters(2).scaled(3, "OK")
    return if (scaled == 6) tag else "scaled $scaled"
}
"#;

#[test]
fn a_value_class_member_is_inlined_from_its_compiled_impl() {
    assert_like_kotlinc(&[("ValueMember.kt", VALUE_CLASS_MEMBER)]);
}

#[test]
fn a_value_class_member_inlined_from_its_compiled_impl_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(VALUE_CLASS_MEMBER, "ValueMember"),
        "OK"
    );
}

/// Member delegate operators whose `property` parameter the body never reads take `null`, so the
/// file publishes no `$$delegatedProperties`.
const DELEGATE_OPERATORS: &str = r#"class Cell<T>(var stored: T) {
    inline operator fun getValue(owner: Any?, property: Any): T = stored
    inline operator fun setValue(owner: Any?, property: Any, value: T) {
        stored = value
    }
}

fun box(): String {
    var text by Cell("Fail")
    text = "OK"
    return text
}
"#;

#[test]
fn member_delegate_operators_pass_null_for_an_unread_property() {
    assert_like_kotlinc(&[("Delegates.kt", DELEGATE_OPERATORS)]);
}

/// A member declared in another file is compiled on its own class, under that file's name, and
/// its body reads the caller's receiver from slot 0.
const OTHER_FILE_HOLDER: &str = r#"class Holder(val value: Any?) {
    inline fun <reified T> holds(): Boolean = value is T

    inline fun <T> tagged(tag: T): Pair<Any?, T> = Pair(value, tag)
}
"#;

const OTHER_FILE_CALLER: &str = r#"fun box(): String {
    if (!Holder("x").holds<String>()) return "holds"
    if (Holder(1).holds<String>()) return "rejects"
    return Holder(null).tagged("OK").second
}
"#;

#[test]
fn a_member_of_another_file_is_inlined_from_its_compiled_body() {
    assert_like_kotlinc(&[
        ("Holder.kt", OTHER_FILE_HOLDER),
        ("Caller.kt", OTHER_FILE_CALLER),
    ]);
}

/// A member of another file's value class is compiled as that class realizes it, a static `-impl`
/// over the carrier, and the call passes its receiver's carrier first.
const OTHER_FILE_VALUE_CLASS: &str = r#"@JvmInline
value class Foo(val x: Int) {
    inline fun inc(): Foo = Foo(x + 1)

    inline fun <T> paired(tag: T): Pair<Int, T> = Pair(x, tag)
}
"#;

const OTHER_FILE_VALUE_CLASS_CALLER: &str = r#"fun box(): String {
    val b = Foo(0).inc().inc()
    if (b.x != 2) return "inc ${b.x}"
    return Foo(1).paired("OK").second
}
"#;

#[test]
fn a_value_class_member_of_another_file_is_inlined_from_its_compiled_impl() {
    assert_like_kotlinc(&[
        ("Foo.kt", OTHER_FILE_VALUE_CLASS),
        ("Caller.kt", OTHER_FILE_VALUE_CLASS_CALLER),
    ]);
}

/// An inline function's own body reads a companion property through the companion's getter, and
/// a private one inlined into a nested class reaches the private companion through the outer
/// class's `access$getCompanion$p`.
const COMPANION_PROPERTY_READ: &str = r#"class Outer {
    private companion object {
        val result = "OK"
    }

    private inline fun bar() = result

    class Nested {
        fun foo(x: Outer) = x.bar()
    }

    fun own() = bar()

    fun test() = Nested().foo(this)
}

fun box(): String = if (Outer().own() == "OK") Outer().test() else "own"
"#;

#[test]
fn an_inline_member_reads_a_companion_property_through_its_getter() {
    assert_like_kotlinc(&[("Companion.kt", COMPANION_PROPERTY_READ)]);
}

#[test]
fn an_inline_member_reading_a_private_companion_runs_in_a_nested_class() {
    assert_eq!(
        common::expect_box_run_with_stdlib(COMPANION_PROPERTY_READ, "Companion"),
        "OK"
    );
}
