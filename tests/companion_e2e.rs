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
fn companion_property_custom_accessors_run() {
    // A `companion object` property with a custom accessor IS its accessor: kotlinc emits no static
    // field for it, just `getZERO()`/`getLEVEL()`/`setLEVEL(int)` on `C$Companion`, and `C.ZERO`
    // compiles to `getstatic C.Companion; invokevirtual`. A plain companion property (`backing`)
    // still hoists to a static field on the OUTER class, so the accessors read and write it there.
    // (The accessor bodies name `backing` QUALIFIED: reading a companion property unqualified from
    // inside the companion is a separate, still-unsupported shape, and this test is about accessors.)
    let src = "class C {\n\
    companion object {\n\
        var backing = 10\n\
        val ZERO: Int get() = 0\n\
        val DERIVED: Int get() = C.backing * 2\n\
        var LEVEL: Int\n\
            get() = C.backing\n\
            set(v) { C.backing = v + 1 }\n\
    }\n\
}\n\
fun box(): String {\n\
    if (C.ZERO != 0) return \"f1\"\n\
    if (C.DERIVED != 20) return \"f2\"\n\
    if (C.LEVEL != 10) return \"f3\"\n\
    C.LEVEL = 41\n\
    if (C.LEVEL != 42) return \"f4\"\n\
    if (C.DERIVED != 84) return \"f5\"\n\
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
