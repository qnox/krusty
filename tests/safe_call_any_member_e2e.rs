//! `kotlin/Any`'s zero-argument universal members (`toString()`, `hashCode()`) reached through a SAFE
//! call. They are declared on `Any`, so they have no class-side method-table entry and resolve to no
//! library member; `?.` reaches them because its non-null arm re-enters the qualified access, which
//! owns that rule. These cases pin the parts of that behaviour nothing else covers: member-over-
//! extension precedence on the narrowed receiver, a same-named overload of another arity, dispatch to
//! an override, and single evaluation of the receiver.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn safe_to_string_dispatches_to_an_override() {
    const SRC: &str = "class Tagged(val tag: String) {\n\
    \x20 override fun toString(): String = tag\n\
    }\n\
    fun box(): String {\n\
    \x20 val t: Tagged? = Tagged(\"OK\")\n\
    \x20 return t?.toString() ?: \"F\"\n\
    }\n";
    assert_eq!(run(SRC).expect("?.toString() reaches the override"), "OK");
}

#[test]
fn same_named_overload_of_another_arity_does_not_veto_the_call() {
    // The method-table probe matches by NAME only. A `toString(radix: Int)` declared on the receiver
    // must not pre-empt the zero-argument `Any.toString()` and reject the file on an arity mismatch.
    const SRC: &str = "class R {\n\
    \x20 fun toString(radix: Int): String = \"r\" + radix\n\
    \x20 override fun toString(): String = \"OK\"\n\
    }\n\
    fun box(): String {\n\
    \x20 val r: R? = R()\n\
    \x20 return r?.toString() ?: \"F\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("arity-mismatched overload doesn't veto"),
        "OK"
    );
}

#[test]
fn a_member_wins_over_a_same_named_extension_on_a_supertype() {
    // Kotlin resolves a member before an extension, so `Any.toString()` shadows `fun Base.toString()`
    // even when the receiver is declared as the supertype the extension is on — and the safe call must
    // agree with the qualified one, which dispatches to the override.
    const SRC: &str = "open class Base\n\
    class Foo : Base() {\n\
    \x20 override fun toString(): String = \"OK\"\n\
    }\n\
    fun Base.toString(): String = \"EXT\"\n\
    fun box(): String {\n\
    \x20 val f: Base? = Foo()\n\
    \x20 val safe = f?.toString() ?: \"F\"\n\
    \x20 return if (safe == \"OK\") safe else \"F:$safe\"\n\
    }\n";
    assert_eq!(run(SRC).expect("member beats supertype extension"), "OK");
}

#[test]
fn a_member_wins_over_a_same_named_extension_on_an_interface() {
    const SRC: &str = "interface Named\n\
    class Foo : Named {\n\
    \x20 override fun toString(): String = \"OK\"\n\
    }\n\
    fun Named.toString(): String = \"EXT\"\n\
    fun box(): String {\n\
    \x20 val f: Named? = Foo()\n\
    \x20 val safe = f?.toString() ?: \"F\"\n\
    \x20 return if (safe == \"OK\") safe else \"F:$safe\"\n\
    }\n";
    assert_eq!(run(SRC).expect("member beats interface extension"), "OK");
}

#[test]
fn safe_to_string_evaluates_its_receiver_once() {
    const SRC: &str = "var calls = 0\n\
        fun receiver(): String? { calls++; return \"x\" }\n\
        fun box(): String {\n\
        \x20 val r = receiver()?.toString()\n\
        \x20 return if (r == \"x\" && calls == 1) \"OK\" else \"F:$r/$calls\"\n\
        }\n";
    assert_eq!(run(SRC).expect("safe-call receiver runs once"), "OK");
}

#[test]
fn safe_to_string_on_a_collection_receiver() {
    // The shape from the template renderer that first hit this: a nullable value of an interface
    // type, stringified through `?.toString()`.
    const SRC: &str = "fun box(): String {\n\
        val l: List<String>? = listOf(\"O\", \"K\")\n\
        val s = l?.toString() ?: \"F\"\n\
        return if (s == \"[O, K]\") \"OK\" else \"F:$s\"\n\
    }\n";
    assert_eq!(run(SRC).expect("l?.toString() on a List"), "OK");
}
