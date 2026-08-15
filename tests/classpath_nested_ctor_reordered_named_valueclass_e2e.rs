//! A classpath nested-class constructor (`Outer.Nested(...)`, a sealed subclass) with VALUE-CLASS
//! parameters, called with NAMED arguments written OUT OF declaration order. The value-class params
//! force Kotlin to emit a private real constructor plus a synthetic public
//! `<init>(<erased…>, DefaultConstructorMarker)`; the resolver reorders the named args to parameter
//! order, then must match that synthetic ctor. Before the fix the reordered-named path only consulted
//! the plain constructor (never the value-class synthetic) and fell back to positional erased-type
//! matching — which silently worked only when the erased parameter types were permutation-invariant
//! (all the same), so an asymmetric mix (`Reg, AId, Int`) written out of order failed to resolve.
//! The fixture uses only synthetic type and parameter names. Needs the JVM toolchain + kotlin-stdlib
//! + real kotlinc.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class AId(val v: String)\n\
    @JvmInline value class BId(val v: String)\n\
    @JvmInline value class Count(val n: Int)\n\
    class Reg(val v: String)\n\
    class Overloaded {\n\
        val tag: String\n\
        constructor(id: AId) { tag = \"A\" + id.v }\n\
        constructor(count: Count) { tag = \"C\" + count.n }\n\
    }\n\
    class Outer {\n\
        inner class Item(val id: AId)\n\
        inner class Plain(val first: String, val second: Int)\n\
    }\n\
    sealed interface Node {\n\
        data class Managed(val id: Reg, val a: AId, val b: BId, val n: Int) : Node\n\
        // A value class over a PRIMITIVE underlying type, mixed with a plain-object param — the\n\
        // synthetic marker ctor erases `Count` to `int`, so arg order + unboxing must stay correct.\n\
        data class Counted(val id: Reg, val c: Count, val a: AId) : Node\n\
        companion object { fun helper(): Int = 1 }\n\
    }\n";

#[test]
fn classpath_nested_ctor_reordered_named_valueclass_runs() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(lo) = common::compile_lib("nested_vc_ctor", LIB) else {
        return;
    };
    // Named args deliberately out of declaration order (id, a, b, n); value-class-typed arguments.
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val m = Node.Managed(a = AId(\"A\"), n = 7, id = Reg(\"R\"), b = BId(\"B\"))\n\
            val c = Node.Counted(c = Count(9), a = AId(\"X\"), id = Reg(\"Q\"))\n\
            val a = Overloaded(AId(\"Z\"))\n\
            val n = Overloaded(Count(4))\n\
            val outer = Outer()\n\
            val i = outer.Item(AId(\"I\"))\n\
            val p = outer.Plain(\"P\", 2)\n\
            val q = outer.Plain(second = 3, first = \"Q\")\n\
            return if (m.id.v == \"R\" && m.a.v == \"A\" && m.b.v == \"B\" && m.n == 7\n\
                && c.id.v == \"Q\" && c.c.n == 9 && c.a.v == \"X\"\n\
                && a.tag == \"AZ\" && n.tag == \"C4\" && i.id.v == \"I\"\n\
                && p.first == \"P\" && p.second == 2\n\
                && q.first == \"Q\" && q.second == 3) \"OK\"\n\
            else \"F id=${m.id.v} a=${m.a.v} b=${m.b.v} n=${m.n} c=${c.c.n}\"\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[lo, sl, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "reordered named args on a classpath value-class-param nested ctor"
    );
}
