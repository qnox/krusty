//! A FULLY-QUALIFIED CONSTRUCTOR call via a package path — `a.b.Ctx(x = 1, y = 2)` where `a.b` is a
//! package and `Ctx` is a top-level class of it (`a/b/Ctx`). Before, the leftmost segment was typed as a
//! value → "unresolved reference 'a'" (the two-segment `Outer.Nested` ctor path only matched a bare-`Name`
//! receiver). The constructor analog of the fully-qualified top-level CALL (`a.b.helper()`): the checker
//! and lowerer's `qualified_nested_ctor_internal` / `nested_ctor_internal` now resolve a dotted package
//! path `a.b.Ctx` to `a/b/Ctx` (verified on the classpath) and construct it — named, positional, reordered
//! and omitted-default forms. Built by the real kotlinc via the shared `common::run_box_against` harness.
use super::common;

const LIB: &str = "package a.b\n\
     class Ctx(val x: Int, val y: Int = 9, val z: String = \"d\")\n";

#[test]
fn fully_qualified_constructor_call() {
    let main = "fun box(): String {\n\
        \x20 val c = a.b.Ctx(x = 1, y = 2)\n\
        \x20 if (c.x != 1 || c.y != 2 || c.z != \"d\") return \"fail named: ${c.x},${c.y},${c.z}\"\n\
        \x20 val p = a.b.Ctx(3, 4, \"q\")\n\
        \x20 if (p.x != 3 || p.y != 4 || p.z != \"q\") return \"fail positional\"\n\
        \x20 val o = a.b.Ctx(x = 5)\n\
        \x20 if (o.x != 5 || o.y != 9 || o.z != \"d\") return \"fail omit-default: ${o.x},${o.y},${o.z}\"\n\
        \x20 val r = a.b.Ctx(z = \"w\", x = 6)\n\
        \x20 if (r.x != 6 || r.y != 9 || r.z != \"w\") return \"fail reorder: ${r.x},${r.y},${r.z}\"\n\
        \x20 val q = a.b.Ctx(7)\n\
        \x20 if (q.x != 7 || q.y != 9) return \"fail positional-omit\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("fq_ctor", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
