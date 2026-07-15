//! `if (this is B)` flow-narrows the implicit receiver to the subtype `B` inside the guarded branch,
//! so a bare member of `B` resolves through `this`. The lowerer inserts a `checkcast` on the loaded
//! `this` before the field read / getter call.

use crate::common;

#[test]
fn this_smartcast_implicit_receiver() {
    match common::run_box_corpus_case("smartCasts/implicitReceiver.kt") {
        Some(s) => assert_eq!(s, "OK"),
        None => panic!("unexpectedly skipped"),
    }
}

#[test]
fn this_smartcast_member_property() {
    let src = r#"
open class Shape {
    class Circle : Shape() {
        val r = 3
    }

    fun describe(): Int {
        if (this is Circle) return r
        return -1
    }
}

fun box(): String {
    val c: Shape = Shape.Circle()
    return if (c.describe() == 3) "OK" else "FAIL: ${c.describe()}"
}
"#;
    common::expect_box_ok_with_stdlib(src, "ThisSmartcastMember");
}
