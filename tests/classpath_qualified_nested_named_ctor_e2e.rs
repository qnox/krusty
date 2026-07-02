//! A QUALIFIED nested-class constructor `Outer.Nested(...)` (a sealed subclass, or any nested class)
//! with NAMED arguments and/or an OMITTED default parameter — `Op.Ext(a = 1, b = "x")`, `Op.Ext(a = 1)`,
//! `Op.Ext(b = "q", a = 3)`, and the positional-omit `Op.Ext(4)`. Positional all-provided already worked;
//! the named forms were rejected ("named arguments … only top-level") — the receiver `Op` names a TYPE,
//! which the named-arg gate wrongly tried to type as a value — and the omitted-default positional form
//! was "unresolved" (no `<init>$default` synthetic on the nested path). The library is built by the real
//! kotlinc via the shared `common::run_box_against` harness.
mod common;

const LIB: &str = "package lib\n\
     sealed class Op {\n\
       class Ext(val a: Int, val b: String = \"z\") : Op()\n\
       data class Cfg(val x: Int, val y: Int = 9, val z: String = \"d\") : Op()\n\
     }\n";

#[test]
fn classpath_qualified_nested_ctor_named_and_defaults() {
    let main = "import lib.Op\n\
        fun box(): String {\n\
        \x20 val e = Op.Ext(a = 1, b = \"x\")\n\
        \x20 if (e.a != 1 || e.b != \"x\") return \"fail named-all: ${e.a},${e.b}\"\n\
        \x20 val e2 = Op.Ext(a = 2)\n\
        \x20 if (e2.a != 2 || e2.b != \"z\") return \"fail named-omit: ${e2.a},${e2.b}\"\n\
        \x20 val e3 = Op.Ext(b = \"q\", a = 3)\n\
        \x20 if (e3.a != 3 || e3.b != \"q\") return \"fail named-reorder: ${e3.a},${e3.b}\"\n\
        \x20 val e4 = Op.Ext(4)\n\
        \x20 if (e4.a != 4 || e4.b != \"z\") return \"fail positional-omit: ${e4.a},${e4.b}\"\n\
        \x20 val e5 = Op.Ext(5, \"p\")\n\
        \x20 if (e5.a != 5 || e5.b != \"p\") return \"fail positional-all\"\n\
        \x20 val c = Op.Cfg(x = 1, z = \"q\")\n\
        \x20 if (c.x != 1 || c.y != 9 || c.z != \"q\") return \"fail cfg-mid-omit: ${c.x},${c.y},${c.z}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("qual_nested", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
