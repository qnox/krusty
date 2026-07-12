//! A `finally` that CONTAINS a `try`/`catch`, with no `return` crossing it, now emits correctly — the
//! caught-exception slot the catch-all re-raises is tracked in the StackMapTable frames recorded while
//! the finally (its inner try/catch) is emitted. Before, it VerifyError'd ("Bad local variable type").
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn finally_containing_try_catch() {
    const SRC: &str = "var log = \"\"\n\
        fun foo() {\n\
        \x20 try { log += \"T\" } finally {\n\
        \x20   try { log += \"F\"; throw RuntimeException() } catch (e: Throwable) { log += \"C\" }\n\
        \x20 }\n\
        }\n\
        fun box(): String { foo(); return if (log == \"TFC\") \"OK\" else \"fail: $log\" }\n";
    assert_eq!(run(SRC).expect("finally containing try/catch"), "OK");
}
