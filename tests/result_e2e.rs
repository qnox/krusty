//! kotlin.Result: construct via the inline `Result.success` and read via the inline extension
//! `getOrThrow()`. Both are `inline` (private in bytecode), so kotlinc inlines them; krusty must
//! resolve them via @Metadata (which marks them public inline) and splice their classpath bodies.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

// Target e2e for full kotlin.Result support. Ignored until the three required layers land: (1)
// metadata-primary companion/extension resolution [reader done — metadata_reader_e2e], (2) the
// inline-class unboxed ABI so `Result` flows as `Ljava/lang/Object;`, (3) splicing the inline
// `success`/`getOrThrow` bodies at the call site.
#[ignore = "needs inline-class unboxed ABI + inline-fn splicing of Result.success/getOrThrow"]
#[test]
fn result_success_get_or_throw() {
    const SRC: &str = "fun box(): String {\n\
    val r = Result.success(42)\n\
    return if (r.getOrThrow() == 42) \"OK\" else \"fail: \" + r.getOrThrow()\n\
}\n";
    let out = run(SRC).expect("Result.success + getOrThrow should compile + run");
    assert_eq!(out, "OK");
}
