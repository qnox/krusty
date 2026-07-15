//! A VALUE-result labeled return from an inline scope-function lambda — `x.let { if (c) return@let a;
//! b }`. The `return@let` yields a value (not `Unit`), so it binds a result slot; the inline-lambda
//! return mechanism assigns it and the wrapper loop yields it. Both the early-return path and the
//! fall-through path must produce the right value. Needs the JVM toolchain + kotlin-stdlib + kotlinc.
use super::common;

#[test]
fn inline_lambda_value_labeled_return_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    //   pick(5):  it>0 -> return@let "pos"          => "pos"
    //   pick(-1): fall through to "nonpos"          => "nonpos"
    //   grade(90): return@run early                 => "A"
    //   grade(50): fall through                      => "F"
    const MAIN: &str = "\
        fun pick(n: Int): String = n.let { if (it > 0) return@let \"pos\"; \"nonpos\" }\n\
        fun grade(n: Int): String = n.run { if (this >= 60) return@run \"A\"; \"F\" }\n\
        fun box(): String {\n\
            val ok = pick(5) == \"pos\" && pick(-1) == \"nonpos\" && grade(90) == \"A\" && grade(50) == \"F\"\n\
            return if (ok) \"OK\" else \"F ${pick(5)} ${pick(-1)} ${grade(90)} ${grade(50)}\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "value-result `return@let`/`return@run` on both the early and fall-through paths"
    );
}
