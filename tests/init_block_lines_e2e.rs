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

/// An inlined call inside an `init` block's condition writes the caller's line again for the jump
/// that follows, exactly as in a method body. The constructor's table keeps that entry beside the
/// equal one the statement began with; krusty collapsed the two.
#[test]
fn an_init_condition_restates_its_line_after_an_inlined_call_like_kotlinc() {
    let src = r#"
class Checked(name: String) {
    init {
        require(name.isNotBlank()) { "blank" }
    }
}

class Stored(val name: String) {
    init {
        val n = name
        require(n.isNotBlank()) { "blank" }
    }
}
"#;
    common::assert_classes_identical_to_kotlinc(
        "InitInlineRestatedLine",
        src,
        &["Checked", "Stored"],
    );
}
