//! A deeply right-nested expression (`0+(0+(0+...(1)...))`) must survive the checker's and
//! lowerer's recursive descent on a bounded stack. The recursion guard admits 500 levels, so the
//! per-level frame size decides how much stack that costs: with the fat unoptimized `expr_inner`
//! frames (the whole match's locals in one frame, ~125-145 KB each) 450 levels needed ~60 MB and
//! overflowed anything but the conformance pool's 64 MB workers. With the match arms extracted
//! into per-variant helpers, a level costs ~10 KB and 450 levels fit comfortably in the 16 MB
//! thread this test pins — a regression here aborts on stack overflow rather than failing softly.
use super::common;

/// Depth stays under the 500-level recursion guard (past it the file types as `Error` and skips).
const DEPTH: usize = 450;

#[test]
fn deep_expression_nesting_survives_bounded_stack() {
    let mut expr = String::from("1");
    for _ in 0..DEPTH {
        expr = format!("0+({expr})");
    }
    let src = format!(
        "fun box(): String {{\n  val x = {expr}\n  return if (x == 1) \"OK\" else \"FAIL\"\n}}\n"
    );
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || common::compile_and_run_with_stdlib(&src, "Main"))
        .unwrap()
        .join()
        .expect("deep-nesting compile thread must not overflow its 16 MB stack");
    assert_eq!(out.expect("deep-nested expression compiles"), "OK");
}
