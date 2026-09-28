//! `init { … }` blocks keep kotlinc's line and local-variable tables: the block opens on its `init`
//! line and closes on its `}`, each with an entry of its own (a `nop` where no instruction of the
//! block sits on that line), and the constructor's table keeps the lines and locals of every block.

use super::common;

#[test]
fn init_blocks_keep_their_opening_and_closing_lines_like_kotlinc() {
    let src = r#"
fun measure(s: String): Int = 1

class Primary(p: Int) {
    var b = 0
    init {
        val t = measure("a")
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
        d = measure("d")
    }
    val e: Int =
        measure("e")

    constructor(s: String) {
        d = measure(s)
    }
}
"#;
    common::assert_classes_identical_to_kotlinc(
        "InitBlockLines",
        src,
        &["Primary", "OnlySecondary", "InitBlockLinesKt"],
    );
}
