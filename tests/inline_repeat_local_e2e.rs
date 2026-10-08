//! A local declared in an inlined `repeat` lambda shares the frame's closing `nop`.
//!
//! kotlinc lists the `$i$a$` marker, then that local, then the lambda parameter. All three ranges
//! end after the brace `nop`.

use super::common;

fn expect_class_matches(name: &str, src: &str, class: &str) {
    match common::class_bytes_diff_against_kotlinc(name, &[], src, class, "class") {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const REPEAT_LOCAL: &str = "fun write(n: Int, writeNextByte: (Int) -> Unit) {\n\
    repeat(n) {\n\
        val byte = it\n\
        writeNextByte(byte)\n\
    }\n\
}\n";

#[test]
fn an_inlined_repeat_local_matches_kotlinc() {
    expect_class_matches("InlineRepeatLocal", REPEAT_LOCAL, "InlineRepeatLocalKt");
}
