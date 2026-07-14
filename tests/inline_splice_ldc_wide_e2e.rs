//! Splicing a stdlib inline body (`require(cond) { lazyMessage }`) into a host whose class constant pool
//! is LARGE pushes a relocated `ldc` (1-byte pool index) past 255 — the emergent whole-file failure that
//! blocked mission-core's RbacService / MissionDriftService (the require/check inline splice compiled in
//! isolation but not in the full file). `relocate_insns` now widens such an `ldc` to `ldc_w` (0x13, the
//! 2-byte-index form, identical semantics) instead of bailing. This test forces a >255-entry pool with
//! ~300 distinct string constants in the same facade, then exercises an inlined `require { … }`, and RUNS
//! it — verifying the widened splice both verifies and behaves (the guard fires and the fallback path).
use super::common;

fn src() -> String {
    // ~300 distinct string literals in one facade → the constant pool exceeds 255 entries, so a `require`
    // body spliced into a same-facade function relocates its `ldc` to an index > 255.
    let mut s = String::new();
    s.push_str("fun pad(): Int {\n    val xs = listOf(\n");
    for i in 0..300 {
        s.push_str(&format!("        \"unique-constant-string-marker-{i}\",\n"));
    }
    s.push_str("    )\n    return xs.size\n}\n");
    // A function whose body inlines `require(cond) { lazyMessage }` — the lazyMessage lambda captures a
    // local (so it is a real closure), forcing the branchy-host inline-splice path that relocates `ldc`.
    s.push_str(
        "fun checked(n: Int): Int {\n\
        \x20   require(n >= 0) { \"n must be non-negative but was $n (pad=${pad()})\" }\n\
        \x20   return n * 2\n\
        }\n",
    );
    s.push_str(
        "fun box(): String {\n\
        \x20   val ok = checked(3) == 6\n\
        \x20   val caught = try { checked(-1); false } catch (e: IllegalArgumentException) { true }\n\
        \x20   return if (ok && caught && pad() == 300) \"OK\" else \"FAIL:$ok:$caught\"\n\
        }\n",
    );
    s
}

#[test]
fn require_inline_splice_with_large_pool_widens_ldc() {
    assert_eq!(
        common::compile_and_run_with_stdlib(&src(), "Main")
            .expect("require inline splice into a large-pool facade compiles, verifies, and runs"),
        "OK"
    );
}
