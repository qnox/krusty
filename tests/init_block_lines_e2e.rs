//! `init { … }` blocks keep kotlinc's line and local-variable tables: the block opens on its `init`
//! line and closes on its `}`, each with an entry of its own (a `nop` where no instruction of the
//! block sits on that line), and the constructor's table keeps the lines and locals of every block.

use super::common;

#[test]
fn init_blocks_keep_their_opening_and_closing_lines_like_kotlinc() {
    let src = r#"
class Marker
fun measure(value: Marker): Int = 1

class Primary(p: Int) {
    var b = 0
    init {
        val t = measure(Marker())
        b = t
    }
    init { b = 2 }
    init {
    }
    val c: Int
    init {
        c = 1
        if (p > 0) {
            b = 3
        }
    }
}

class OnlySecondary {
    var d = 0
    init {
        d = measure(Marker())
    }
    val e: Int =
        measure(Marker())

    constructor(value: Marker) {
        d = measure(value)
    }
}
"#;
    common::assert_classes_identical_to_kotlinc(
        "InitBlockLines",
        src,
        &["Primary", "OnlySecondary", "InitBlockLinesKt"],
    );
}
