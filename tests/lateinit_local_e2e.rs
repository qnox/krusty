//! `lateinit var` LOCALS: a mutable local with no initializer (slot defaults to null); a read while
//! still null throws `UninitializedPropertyAccessException`. Mirrors the member-field lateinit guard.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn lateinit_local_initialized_then_read() {
    const SRC: &str = "fun box(): String {\n\
    lateinit var s: String\n\
    s = \"OK\"\n\
    return s\n\
}\n";
    assert_eq!(run(SRC).expect("initialized lateinit read"), "OK");
}

#[test]
fn lateinit_local_uninitialized_read_throws() {
    // A read while still null throws UninitializedPropertyAccessException (a RuntimeException).
    const SRC: &str = "fun box(): String {\n\
    lateinit var s: String\n\
    try {\n\
        val r = s\n\
        return \"FAIL: no throw, got $r\"\n\
    } catch (e: RuntimeException) {\n\
        return \"OK\"\n\
    }\n\
}\n";
    assert_eq!(run(SRC).expect("uninitialized lateinit throws"), "OK");
}

#[test]
fn lateinit_local_assigned_in_inline_lambda() {
    // Assigned inside an inline `run { … }` before the read.
    const SRC: &str = "fun box(): String {\n\
    lateinit var ok: String\n\
    run { ok = \"OK\" }\n\
    return ok\n\
}\n";
    assert_eq!(run(SRC).expect("lateinit assigned in run{}"), "OK");
}
