//! Two classpath-resolution gaps against a kotlinc-compiled dependency, exercised broadly:
//!   i1  a constructor whose parameter is a Kotlin COLLECTION (`Rule(val v: Set<String>)`): the JVM
//!       `<init>` descriptor erases the param to `Ljava/util/Set;` and drops the `<String>`, but the call
//!       passes `setOf("a")` typed `kotlin/collections/Set<String>` — ctor matching must bridge the
//!       kotlin↔jvm collection identity and erase the type argument. Covers `Set`/`List`/`Map`/mutable
//!       variants, a collection alongside a scalar, a data class, a nested generic, a custom element type,
//!       a named collection argument, an argument passed through a `val`, and — via the supertype closure
//!       — a `Collection`/`Iterable` parameter accepting a `List`/`Set` (subtype), plus overload
//!       preference (an exact `List` wins over a `Collection` overload).
//!   i2  NAMED arguments on a nested type imported UNQUALIFIED (`import lib.Op.Apply; Apply(a = 1)`), in
//!       omitted-default, all-provided, reordered, positional, and deeply-nested forms.
//! Both were `unresolved`/`named arguments … only top-level` before. The library is built by the real
//! kotlinc via the shared `common::run_box_against` harness (skips when the toolchain is absent).
mod common;

const LIB: &str = "package lib\n\
     class Rule(val v: Set<String>)\n\
     class Route(val hops: List<Int>)\n\
     class MutRule(val v: MutableSet<String>)\n\
     class MapRule(val m: Map<String, Int>)\n\
     class Pair2(val a: Set<String>, val b: List<Int>)\n\
     class Mixed(val a: Set<String>, val n: Int)\n\
     data class Dc(val v: List<Int>)\n\
     class Nested(val g: List<List<Int>>)\n\
     class Key(val n: String)\n\
     class Elem(val s: Set<Key>)\n\
     class Coll(val c: Collection<String>)\n\
     class Iter(val i: Iterable<Int>)\n\
     class Grid(val cells: Array<Set<String>>)\n\
     object Op { class Apply(val a: Int, val b: Int = 7, val c: String = \"z\") }\n\
     object Deep { object Mid { class N(val x: Int, val y: Int = 3) } }\n";

#[test]
fn classpath_collection_param_and_nested_named_ctor() {
    let main = "import lib.Rule\n\
        import lib.Route\n\
        import lib.MutRule\n\
        import lib.MapRule\n\
        import lib.Pair2\n\
        import lib.Mixed\n\
        import lib.Dc\n\
        import lib.Nested\n\
        import lib.Key\n\
        import lib.Elem\n\
        import lib.Coll\n\
        import lib.Iter\n\
        import lib.Op.Apply\n\
        import lib.Deep.Mid.N\n\
        fun box(): String {\n\
        \x20 if (Rule(setOf(\"a\")).v.size != 1) return \"fail i1-set\"\n\
        \x20 if (Route(listOf(1, 2, 3)).hops.size != 3) return \"fail i1-list\"\n\
        \x20 if (MutRule(mutableSetOf(\"a\", \"b\")).v.size != 2) return \"fail i1-mutset\"\n\
        \x20 if (MapRule(mapOf(\"a\" to 1)).m.size != 1) return \"fail i1-map\"\n\
        \x20 val p = Pair2(setOf(\"x\"), listOf(9))\n\
        \x20 if (p.a.size != 1 || p.b.size != 1) return \"fail i1-two\"\n\
        \x20 val m = Mixed(setOf(\"a\"), 7)\n\
        \x20 if (m.a.size != 1 || m.n != 7) return \"fail i1-mixed\"\n\
        \x20 if (Dc(listOf(1, 2, 3)).v.size != 3) return \"fail i1-data\"\n\
        \x20 if (Nested(listOf(listOf(1), listOf(2, 3))).g.size != 2) return \"fail i1-nestedgen\"\n\
        \x20 if (Elem(setOf(Key(\"a\"))).s.size != 1) return \"fail i1-customelem\"\n\
        \x20 if (Rule(v = setOf(\"a\", \"b\")).v.size != 2) return \"fail i1-namedcoll\"\n\
        \x20 val s: Set<String> = setOf(\"a\")\n\
        \x20 if (Rule(s).v.size != 1) return \"fail i1-viavar\"\n\
        \x20 if (Coll(listOf(\"a\", \"b\")).c.size != 2) return \"fail i1-coll-from-list\"\n\
        \x20 if (Coll(setOf(\"a\")).c.size != 1) return \"fail i1-coll-from-set\"\n\
        \x20 if (Iter(listOf(1, 2, 3)).i.count() != 3) return \"fail i1-iter\"\n\
        \x20 val ap = Apply(a = 1)\n\
        \x20 if (ap.a != 1 || ap.b != 7 || ap.c != \"z\") return \"fail i2-omit\"\n\
        \x20 val ap2 = Apply(a = 2, b = 3, c = \"q\")\n\
        \x20 if (ap2.a != 2 || ap2.b != 3 || ap2.c != \"q\") return \"fail i2-all\"\n\
        \x20 val ap3 = Apply(c = \"x\", a = 5)\n\
        \x20 if (ap3.a != 5 || ap3.b != 7 || ap3.c != \"x\") return \"fail i2-reorder\"\n\
        \x20 val ap4 = Apply(9)\n\
        \x20 if (ap4.a != 9 || ap4.b != 7) return \"fail i2-positional\"\n\
        \x20 val n = N(x = 1)\n\
        \x20 if (n.x != 1 || n.y != 3) return \"fail i2-deepnested\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("coll_nested", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }

    // A collection nested inside an ARRAY parameter (`Array<Set<String>>` → `[Ljava/util/Set;`) exercises
    // the recursive arm of the descriptor-form normalization: `arrayOf(setOf(...))` types as
    // `Array<kotlin/collections/Set<String>>` and must match the erased `Array<java/util/Set>` parameter.
    // Constructing a reference array end-to-end is an orthogonal, not-yet-lowered feature, so this asserts
    // at the RESOLUTION level (the constructor resolves — no diagnostic), not by running.
    let ctor_main = "import lib.Grid\nfun probe(a: Array<Set<String>>): Grid = Grid(a)\n";
    if let Some(msgs) = common::checker_diags_against("coll_arr", LIB, ctor_main) {
        assert!(
            msgs.is_empty(),
            "Array<Set<String>> ctor param should resolve, got: {msgs:?}"
        );
    }
}

/// Overloaded constructors distinguished by collection type: an EXACT `List` parameter is preferred over
/// a `Collection` parameter when the call passes a `List` (the descriptor-form exact pass wins over the
/// subtype pass); a `Set` argument — not a `List` — routes to the `Collection` overload.
#[test]
fn classpath_collection_ctor_overload_prefers_exact() {
    const LIB_OV: &str = "package lib\n\
         class Ov(val tag: String) {\n\
           constructor(l: List<Int>) : this(\"list${l.size}\")\n\
           constructor(c: Collection<String>) : this(\"coll${c.size}\")\n\
         }\n";
    let main = "import lib.Ov\n\
        fun box(): String {\n\
        \x20 if (Ov(listOf(1, 2)).tag != \"list2\") return \"fail exact-list: ${Ov(listOf(1, 2)).tag}\"\n\
        \x20 if (Ov(setOf(\"a\")).tag != \"coll1\") return \"fail subtype-set: ${Ov(setOf(\"a\")).tag}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("coll_ov", LIB_OV, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
