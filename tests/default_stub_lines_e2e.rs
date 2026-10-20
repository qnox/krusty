//! A `$default` stub's `LineNumberTable` follows kotlinc's: the stub stands on the signature line,
//! each default expression runs on its own lines, the call returns to the signature, and a
//! `$DefaultImpls` forward of the stub returns on the declaration's end line.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const SOURCE: &str = r#"
interface Port {
    fun create(
        name: String,
        domains: List<String> = emptyList(),
    ): String
    suspend fun createLater(
        name: String,
        domains: List<String> = emptyList(),
    ): String
}
open class Member {
    fun pick(
        a: Int = 1,
        b: String = "x",
        c: Int,
    ): String = "$a$b$c"
    fun mixed(a: Int = 1,
        b: Int = 2): Int = a + b
}
fun top(
    x: Int = 5,
): Int = x
"#;

/// A member stub marks each default on its own line and returns to the signature for the call.
#[test]
fn a_member_stub_marks_its_defaults_lines() {
    byte_identical("default_stub_lines_member", SOURCE, "Member");
}

/// An interface's stubs, abstract and suspend included, mark their defaults' lines.
#[test]
fn an_interface_stub_marks_its_defaults_lines() {
    byte_identical("default_stub_lines_interface", SOURCE, "Port");
}

/// The `$DefaultImpls` forward of an abstract member's stub returns on the declaration's end line.
#[test]
fn a_default_impls_forward_returns_on_the_declaration_end_line() {
    byte_identical("default_stub_lines_holder", SOURCE, "Port$DefaultImpls");
}

/// A top-level stub marks its default's line and returns to the signature for the call.
#[test]
fn a_top_level_stub_marks_its_default_line() {
    byte_identical(
        "default_stub_lines_top_level",
        SOURCE,
        "Default_stub_lines_top_levelKt",
    );
}
