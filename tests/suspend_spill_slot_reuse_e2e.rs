//! A spilled local must occupy ONE slot across its two declarations — the coroutine dispatch's
//! loop-top restore AND its real in-body declaration in a resume state (both carry the same
//! value-index). If the emitter allocates a fresh slot per declaration, the loop-top restore
//! populates a different slot than the frame at a `?: continue` target expects, leaving that slot
//! `top` on the fresh (first-entry) edge — a StackMapTable VerifyError. This is the shape of
//! `ResourceAggregationService.getAllResources`/`getResourceById`: an outer SUSPENDING loop with a
//! `?: continue`, a local declared AFTER the continue that is live across a later suspension, and a
//! nested structural loop. Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc.
use super::common;

#[test]
fn suspend_spill_slot_reuse_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(coro) = common::coroutines_jar() else {
        return;
    };
    // agg: for each (name, count) in cfg, skip when catalog has no tag (`?: continue`), else run a
    // nested loop that reads `tag` (declared at the continue) and `scaled` (declared AFTER it, live
    // across the nested suspensions). Both are spilled restore-only locals reached on the fresh edge.
    //   a: tag "xx"(2) items[10,20](2) scaled 2: Σ v+2+2+2 = 16+26 = 42
    //   b: tag "y"(1)  items[1,2,3](3)  scaled 3: Σ v+1+3+3 = 8+9+10 = 27
    //   z: catalog[\"z\"]==null → continue                       = 0
    //   total = 69
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun d(x: Int): Int = x\n\
        suspend fun agg(cfg: Map<String, Int>, catalog: Map<String, String>, data: Map<String, List<Int>>): Int {\n\
            var total = 0\n\
            for ((name, count) in cfg) {\n\
                val tag = catalog[name] ?: continue\n\
                val items = data[name] ?: emptyList()\n\
                val scaled = d(count)\n\
                for (v in items) {\n\
                    total += d(v) + tag.length + scaled + items.size\n\
                }\n\
            }\n\
            return total\n\
        }\n\
        fun box(): String = runBlocking {\n\
            val r = agg(\n\
                linkedMapOf(\"a\" to 2, \"b\" to 3, \"z\" to 1),\n\
                mapOf(\"a\" to \"xx\", \"b\" to \"y\"),\n\
                mapOf(\"a\" to listOf(10, 20), \"b\" to listOf(1, 2, 3)))\n\
            if (r == 69) \"OK\" else \"F r=$r\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "spilled restore-only local across a `?: continue` + nested loop must share one slot"
    );
}
