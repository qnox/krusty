//! `companion object` methods — compiled (like kotlinc) to a `C$Companion` class holding the methods,
//! a `public static final Companion` field on the outer class built in its `<clinit>`, and
//! `C.foo()` → `getstatic C.Companion; invokevirtual`. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn companion_methods_run() {
    let src = "class C {\n\
    companion object {\n\
        fun answer(): Int = 42\n\
        fun greet(s: String): String = \"hi \" + s\n\
    }\n\
}\n\
fun box(): String {\n\
if (C.answer() != 42) return \"f1\"\n\
if (C.greet(\"x\") != \"hi x\") return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn property_inferred_from_generic_companion_method() {
    // A property initialized by a same-file class's generic companion method (`val c =
    // C.create<String>()`) infers its type from the companion method's (inferred) return type.
    let src = "class C() {\n\
    companion object {\n\
        private fun <T> create() = C()\n\
    }\n\
    class ZZZ { val c = C.create<String>() }\n\
}\n\
fun box(): String { C.ZZZ().c; return \"OK\" }\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn companion_reaches_the_outer_class_private_var() {
    // A companion is a SEPARATE class file, so it can neither call a private property's accessor
    // (kotlinc synthesizes none) nor `putfield` the private backing field. kotlinc routes the write
    // through a synthetic `access$setX$p(Outer, T)` bridge; emitting a plain `setX` call is a
    // `NoSuchMethodError` at run time. Mirrors box `classes/kt504.kt`, which the default gate
    // does not run.
    let src = "class Identifier() {\n\
    private var myNullable: Boolean = true\n\
    fun read(): Boolean = myNullable\n\
    companion object {\n\
        fun init(isNullable: Boolean): Identifier {\n\
            val id = Identifier()\n\
            id.myNullable = isNullable\n\
            return id\n\
        }\n\
    }\n\
}\n\
fun box(): String = if (!Identifier.init(false).read()) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Identifier");
}

#[test]
fn a_private_property_keeps_its_source_written_setter() {
    // The accessor a private property does NOT get is the SYNTHESIZED one. A source-written
    // accessor is user code: dropping it turns `set(l) { /* ignore */ }` into a plain field store,
    // so the write below would take effect. Mirrors box `properties/kt3551.kt`.
    let src = "class Identifier() {\n\
    private var myNullable: Boolean = false\n\
        set(l: Boolean) {\n\
        }\n\
    fun read(): Boolean = myNullable\n\
    companion object {\n\
        fun init(isNullable: Boolean): Identifier {\n\
            val id = Identifier()\n\
            id.myNullable = isNullable\n\
            return id\n\
        }\n\
    }\n\
}\n\
fun box(): String = if (!Identifier.init(true).read()) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Identifier2");
}

#[test]
fn a_nested_class_reaches_the_outer_class_private_member() {
    // Kotlin's private visibility is LEXICAL: a nested (non-`inner`) class has no outer receiver at
    // all, yet it sits inside the outer class's body and reaches its privates — including the
    // companion's. Walking the receiver chain alone reported this inaccessible.
    let src = "class Outer {\n\
    private val secret: Int = 7\n\
    private fun twice(): Int = secret * 2\n\
    class Nested {\n\
        fun read(): Int = Outer().twice()\n\
    }\n\
}\n\
fun box(): String = if (Outer.Nested().read() == 14) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Outer");
}

#[test]
fn a_private_member_of_an_unrelated_class_stays_inaccessible() {
    // The lexical rule must not become "everything in the file is accessible".
    let src = "class A { private fun secret(): Int = 1 }\n\
class B { fun read(): Int = A().secret() }\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(src) else {
        return;
    };
    assert!(
        diagnostics.iter().any(|d| d.contains("secret")),
        "expected a private-access diagnostic, got: {diagnostics:?}"
    );
}
