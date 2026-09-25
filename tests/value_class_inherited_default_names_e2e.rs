//! An interface default whose signature mentions a value class keeps its hashed JVM name when a
//! class or sub-interface inherits it.
//!
//! `fun rep(n: Count)` is `rep-<hash>(I)` on its interface. A sub-interface's `DefaultImpls`
//! forwarder and an implementing class's forwarder answer to that same name and call it under
//! it, as kotlinc's do; the plain `rep` names no method anywhere.

use super::common;

const SOURCE: &str = r#"@JvmInline
value class Count(val value: Int)

interface Parser {
    fun rep(n: Count): String = if (n.value == 1) "rep1" else "rep?"
}

interface ByteParser : Parser

class Concrete : ByteParser

fun box(): String = if (Concrete().rep(Count(1)) == "rep1") "OK" else "fail"
"#;

// Separate from the generic regression above: this pins the stdlib metadata boundary. `UInt` has
// a JVM-native carrier, but its checked declaration must still publish that it is a value class so
// an inherited member is named from semantic identity rather than a hardcoded stdlib list.
const UNSIGNED_SOURCE: &str = r#"interface UnsignedParser {
    fun rep(n: UInt): String = "rep" + n.toInt()
}

interface UnsignedChild : UnsignedParser

class UnsignedConcrete : UnsignedChild
"#;

/// The class disassembled verbosely without its constant pool, with pool indices masked.
fn disassembled(bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("a scratch directory");
    let path = work.join("Disassembled.class");
    std::fs::write(&path, bytes).expect("class file");
    let text = common::javap(&["-p", "-v", &path.to_string_lossy()]).expect("javap runs");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .skip_while(|line| !line.starts_with("public") && !line.starts_with("final"))
        .filter(|line| !line.trim_start().starts_with('#') && !line.contains("Constant pool:"))
        .map(|line| {
            line.split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything before the `@Metadata` annotation, which is the metadata writer's.
fn before_metadata(disassembly: &str) -> &str {
    disassembly
        .split_once("RuntimeVisibleAnnotations:")
        .map_or(disassembly, |(members, _)| members)
}

fn assert_matches_kotlinc(class: &str) {
    let pair = common::ModuleClassPair::compile(&[("Parser.kt", SOURCE)], class);
    assert_eq!(
        before_metadata(&disassembled(&pair.krusty)),
        before_metadata(&disassembled(&pair.kotlinc))
    );
}

#[test]
fn a_sub_interface_forwards_the_default_under_its_hashed_name() {
    assert_matches_kotlinc("ByteParser$DefaultImpls");
}

#[test]
fn an_implementing_class_forwards_the_default_under_its_hashed_name() {
    assert_matches_kotlinc("Concrete");
}

#[test]
fn a_call_reaches_the_inherited_default() {
    common::expect_box_ok_with_stdlib(SOURCE, "InheritedDefaultNames");
}

#[test]
fn stdlib_unsigned_metadata_drives_the_inherited_member_name() {
    let pair = common::ModuleClassPair::compile(
        &[("UnsignedParser.kt", UNSIGNED_SOURCE)],
        "UnsignedConcrete",
    );
    assert_eq!(
        before_metadata(&disassembled(&pair.krusty)),
        before_metadata(&disassembled(&pair.kotlinc))
    );
}
