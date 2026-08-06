//! A class may extend a plain NESTED class named by its qualified source form (`Outer.Bar`): the
//! nested class is hoisted to a top-level `Outer$Bar` and the subclass's `super(args)` targets it.
//! Covers both a class-nested and an object-nested base (a singleton's nested class is still a plain
//! nested class, not `inner`), and reading an inherited constructor-property field. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn extends_class_nested_base_reads_inherited_field() {
    const SRC: &str = "class Outer { open class Bar(val bar: String) }\n\
        class Baz: Outer.Bar(\"OK\")\n\
        fun box(): String = Baz().bar\n";
    assert_eq!(run(SRC).expect("class-nested base"), "OK");
}

#[test]
fn extends_object_nested_base_reads_inherited_field() {
    const SRC: &str = "object Foo { open class Bar(val bar: String) }\n\
        class Baz: Foo.Bar(\"OK\")\n\
        fun box(): String = Baz().bar\n";
    assert_eq!(run(SRC).expect("object-nested base"), "OK");
}

#[test]
fn extends_no_arg_nested_base() {
    const SRC: &str = "class Outer { open class Bar { fun tag() = \"OK\" } }\n\
        class Baz: Outer.Bar()\n\
        fun box(): String = Baz().tag()\n";
    assert_eq!(run(SRC).expect("no-arg nested base"), "OK");
}

#[test]
fn nested_class_implements_sibling_nested_interface() {
    // A nested class implements a SIBLING nested interface named by simple name (`Foo`, not
    // `Test.Foo`) — resolved through the enclosing scope. The interface is hoisted and emitted.
    const SRC: &str = "class Test {\n\
        \x20 interface Foo { fun r(): String }\n\
        \x20 class Impl: Foo { override fun r() = \"OK\" }\n\
        }\n\
        fun box(): String = Test.Impl().r()\n";
    assert_eq!(run(SRC).expect("sibling nested iface"), "OK");
}

#[test]
fn nested_class_extends_sibling_nested_class() {
    // A nested class extends a SIBLING nested (open) class by simple name.
    const SRC: &str = "class Test {\n\
        \x20 open class Base(val s: String)\n\
        \x20 class Sub: Base(\"OK\")\n\
        }\n\
        fun box(): String = Test.Sub().s\n";
    assert_eq!(run(SRC).expect("sibling nested base"), "OK");
}

#[test]
fn packaged_nested_class_implements_qualified_interface() {
    const SRC: &str = "package sample.scope\n\
        sealed interface Kind {\n\
        \x20 interface Branch : Kind\n\
        \x20 interface Leaf : Branch\n\
        }\n\
        sealed class Marker {\n\
        \x20 class Leaf : Kind.Leaf\n\
        }\n\
        fun box(): String {\n\
        \x20 val leaf = Marker.Leaf()\n\
        \x20 return if (leaf is Kind.Leaf && leaf is Kind.Branch && leaf is Kind) \"OK\" else \"FAIL\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "PackagedNestedInterface");
}

#[test]
fn implements_stdlib_nested_interface() {
    // A CLASSPATH Kotlin nested interface as supertype (`MutableMap.MutableEntry` →
    // `kotlin/collections/MutableMap$MutableEntry`): the dotted form must join with `$`, not `/`.
    const SRC: &str = "class Entry(override val key: String, override val value: Int) : MutableMap.MutableEntry<String, Int> {\n\
        \x20 override fun setValue(newValue: Int): Int = value\n\
        }\n\
        fun box(): String = Entry(\"OK\", 1).key\n";
    assert_eq!(run(SRC).expect("stdlib nested interface supertype"), "OK");
}

#[test]
fn overriding_nested_typed_supertype_property_emits_loadable_class() {
    // The metadata type of `CoroutineContext.Element.key` uses the source-facing dotted nested name
    // `kotlin/coroutines/CoroutineContext.Key`. Override comparison and bytecode emission both consume
    // the shared descriptor helper, so that helper must turn the class-tail dot into the JVM `$` form.
    // Before that boundary normalization, bridge derivation observed unequal descriptors and emitted a
    // dotted descriptor that the VM rejected while loading the otherwise ordinary implementation.
    const SRC: &str = "import kotlin.coroutines.CoroutineContext\n\
        class Elem(val name: String) : CoroutineContext.Element {\n\
        \x20 override val key: CoroutineContext.Key<*> get() = TODO()\n\
        }\n\
        fun box(): String = Elem(\"OK\").name\n";
    assert_eq!(run(SRC).expect("nested-typed property override"), "OK");
}

#[test]
fn coroutine_context_nested_supertypes_resolve_on_the_frontend() {
    // A synthetic compound header: `CoroutineContext.Element` + `CoroutineContext.Key` are
    // supertypes of a class AND its companion. The shape additionally extends an abstract library
    // base, which hits a SEPARATE IR-backend gate — pin here that the provider-neutral FRONT END
    // resolves both nested supertypes without the "supertype could not be resolved" diagnostic.
    const SRC: &str = "import kotlin.coroutines.AbstractCoroutineContextElement\n\
        import kotlin.coroutines.CoroutineContext\n\
        class Elem(val name: String) : AbstractCoroutineContextElement(Elem), CoroutineContext.Element {\n\
        \x20 companion object : CoroutineContext.Key<Elem>\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
