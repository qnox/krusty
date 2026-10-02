//! A class companion's `init` blocks run in the enclosing class `<clinit>`, after the companion
//! instance is stored and interleaved with that companion's property initializers.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn companion_and_instance_init_inline_break_continue() {
    expect_box_same_as_kotlinc(
        r#"
class C {
    companion object {
        val visited = mutableListOf<Int>()

        init {
            for (i in 1..5) {
                run {
                    if (i == 2) continue
                    if (i == 4) break
                }
                C.visited.add(i)
            }
        }
    }

    val visited = mutableListOf<Int>()

    init {
        for (i in 1..5) {
            run {
                if (i == 1) continue
                if (i == 4) break
            }
            visited.add(i)
        }
    }
}

fun box(): String {
    val c = C()
    if (C.visited != listOf(1, 3)) return "companion=${C.visited}"
    if (c.visited != listOf(2, 3)) return "instance=${c.visited}"
    return "OK"
}
"#,
        "CompanionInitInlineBreak",
    );
}

#[test]
fn companion_init_blocks_interleave_with_property_initializers() {
    expect_box_same_as_kotlinc(
        r#"
class C {
    companion object {
        val a = mutableListOf(1)
        init { a.add(2) }
        val b = mutableListOf(a.size)
        init { a.add(3); b.add(4) }
    }
}

fun box(): String {
    if (C.a != listOf(1, 2, 3)) return "a=${C.a}"
    if (C.b != listOf(2, 4)) return "b=${C.b}"
    return "OK"
}
"#,
        "CompanionInitInterleaved",
    );
}

#[test]
fn companion_init_calls_a_companion_member() {
    expect_box_same_as_kotlinc(
        r#"
class C {
    companion object {
        fun value(): Int = 1
        val seen = mutableListOf<Int>()
        init { seen.add(value()) }
    }
}

fun box(): String = if (C.seen == listOf(1)) "OK" else "seen=${C.seen}"
"#,
        "CompanionInitMemberCall",
    );
}

#[test]
fn private_companion_property_init_stays_on_the_companion() {
    expect_box_same_as_kotlinc(
        r#"
class C {
    companion object {
        private val secret = 7
        init {
            if (secret != 7) throw IllegalStateException("secret=$secret")
        }
        fun read() = secret
    }
}

fun box(): String = if (C.read() == 7) "OK" else "secret=${C.read()}"
"#,
        "CompanionInitPrivateProperty",
    );
}
