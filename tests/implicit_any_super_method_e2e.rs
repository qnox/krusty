//! `super.equals(…)` / `super.hashCode()` / `super.toString()` resolve through the implicit `Any`
//! supertype even when a class declares only interfaces, or nothing at all.
//!
//! The super-call path looked at the declared SUPERCLASS and then at superinterface defaults. A class
//! whose only declared supertype is an interface has neither, so the call died as
//! "krusty: unresolved super method 'equals'" — although every class extends `Any`/`java.lang.Object`,
//! which is what kotlinc calls (`invokespecial java/lang/Object.equals`).
//!
//! Found by diffing krusty-lsp against the JetBrains Kotlin language server on intellij-community's
//! `fleet.fastutil` `IntOpenHashSet`, which is declared `: MutableIntSet` and calls `super.equals`;
//! the reference server reports nothing there.

use super::common;

#[test]
fn super_equals_and_hash_code_resolve_with_only_an_interface_supertype() {
    let source = "interface Marker\n\
class Plain(val tag: String) : Marker {\n\
\x20   override fun equals(other: Any?): Boolean {\n\
\x20       if (super.equals(other)) return true\n\
\x20       return other is Plain && other.tag == tag\n\
\x20   }\n\
\x20   override fun hashCode(): Int = super.hashCode() + tag.length\n\
}\n\
fun box(): String {\n\
\x20   val a = Plain(\"x\")\n\
\x20   if (!a.equals(a)) return \"fail: identity\"\n\
\x20   if (!a.equals(Plain(\"x\"))) return \"fail: equal tags\"\n\
\x20   if (a.equals(Plain(\"y\"))) return \"fail: different tags\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(source, "IASM1");
}

#[test]
fn super_to_string_resolves_with_no_declared_supertype() {
    let source = "class Bare {\n\
\x20   override fun toString(): String = super.toString()\n\
}\n\
fun box(): String {\n\
\x20   val text = Bare().toString()\n\
\x20   return if (text.startsWith(\"Bare@\")) \"OK\" else \"fail:\" + text\n\
}\n";
    common::expect_box_ok_with_stdlib(source, "IASM2");
}
