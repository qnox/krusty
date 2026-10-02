//! A class companion's `init` blocks run in the enclosing class `<clinit>`, after the companion
//! instance is stored and interleaved with that companion's property initializers. Private
//! companion properties are private static fields of the outer class, initialized in that same
//! order.

use super::common::{self, expect_box_same_as_kotlinc};

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

const PRIVATE_PROPERTY: &str = r#"
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
"#;

#[test]
fn private_companion_property_is_an_outer_static_initialized_after_the_instance() {
    expect_box_same_as_kotlinc(PRIVATE_PROPERTY, "CompanionInitPrivateProperty");
    assert_same_fields(PRIVATE_PROPERTY, "C");
    assert_same_fields(PRIVATE_PROPERTY, "C$Companion");
    assert_same_method(PRIVATE_PROPERTY, "C", "<clinit>");
    assert_same_method(PRIVATE_PROPERTY, "C$Companion", "C$Companion");
    assert_same_method(PRIVATE_PROPERTY, "C$Companion", "read");
}

const MIXED_PROPERTIES: &str = r#"
class C {
    companion object {
        val seen = mutableListOf<Int>()
        private val secret = 7
        init { seen.add(secret) }
        val after = secret + seen.size
        fun read() = secret
        fun snapshot() = seen.toList()
    }
}

fun box(): String {
    if (C.read() != 7) return "secret=${C.read()}"
    if (C.snapshot() != listOf(7)) return "seen=${C.snapshot()}"
    if (C.after != 8) return "after=${C.after}"
    return "OK"
}
"#;

#[test]
fn mixed_private_and_public_companion_properties_initialize_in_source_order() {
    expect_box_same_as_kotlinc(MIXED_PROPERTIES, "CompanionInitMixedProperties");
    assert_same_fields(MIXED_PROPERTIES, "C");
    assert_same_fields(MIXED_PROPERTIES, "C$Companion");
    assert_same_method(MIXED_PROPERTIES, "C", "<clinit>");
    assert_same_method(MIXED_PROPERTIES, "C$Companion", "C$Companion");
    assert_same_method(MIXED_PROPERTIES, "C$Companion", "read");
    assert_same_method(MIXED_PROPERTIES, "C$Companion", "getSeen");
    assert_same_method(MIXED_PROPERTIES, "C$Companion", "getAfter");
}

fn assert_same_method(src: &str, class: &str, method: &str) {
    let pair = common::ModuleClassPair::compile(&[("Companion.kt", src)], class);
    let (kotlinc, krusty) = pair.method_code(class, method);
    assert_eq!(
        krusty, kotlinc,
        "{class}.{method} instructions differ\n--- kotlinc ---\n{kotlinc}--- krusty ---\n{krusty}"
    );
}

fn assert_same_fields(src: &str, class: &str) {
    let pair = common::ModuleClassPair::compile(&[("Companion.kt", src)], class);
    let kotlinc = field_lines(&pair.kotlinc, class);
    let krusty = field_lines(&pair.krusty, class);
    assert_eq!(
        krusty, kotlinc,
        "{class} fields differ\n--- kotlinc ---\n{kotlinc}--- krusty ---\n{krusty}"
    );
}

fn field_lines(bytes: &[u8], class: &str) -> String {
    let work = common::scratch_dir().expect("cannot allocate field disassembly");
    let path = work.join(format!("{}.class", class.replace('/', "_")));
    std::fs::write(&path, bytes).expect("write class for field disassembly");
    let text = common::javap(&["-p", &path.to_string_lossy()]).expect("javap unavailable");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .map(str::trim)
        .filter(|line| {
            *line != "static {};"
                && line.contains("static")
                && line.ends_with(';')
                && !line.contains('(')
        })
        .map(|line| format!("{line}\n"))
        .collect()
}
