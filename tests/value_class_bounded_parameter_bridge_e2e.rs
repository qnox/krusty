//! An override of a member whose parameter is a type parameter bounded by a value class.
//!
//! `fun foo(i: T?)` with `T : Inlined` erases to `foo-<hash>(LInlined;)V`: the parameter is the
//! value class, so the name is mangled, while its generic `Signature` keeps `(TT;)V`. An override
//! taking `Nothing?` is `foo(Ljava/lang/Void;)V`, so its class declares a bridge under the
//! supertype's mangled name that casts the argument and calls the override. A supertype argument
//! of `Nothing` makes the class header raw, so the class carries no `Signature` at all.

use super::common;

const SOURCE: &str = r#"@JvmInline value class Inlined(val value: Int)

interface A<T : Inlined> {
    fun foo(i: T?)
}

class B : A<Nothing> {
    override fun foo(i: Nothing?) {}
}

fun box(): String {
    val a: A<*> = B()
    a.foo(null)
    return "OK"
}
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

#[test]
fn the_override_declares_kotlincs_mangled_bridge() {
    let pair = common::ModuleClassPair::compile(&[("Bounded.kt", SOURCE)], "B");
    assert_eq!(
        before_metadata(&disassembled(&pair.krusty)),
        before_metadata(&disassembled(&pair.kotlinc))
    );
}

#[test]
fn the_interface_member_keeps_its_type_parameter_signature() {
    let pair = common::ModuleClassPair::compile(&[("Bounded.kt", SOURCE)], "A");
    assert_eq!(
        before_metadata(&disassembled(&pair.krusty)),
        before_metadata(&disassembled(&pair.kotlinc))
    );
}

#[test]
fn a_call_through_the_interface_reaches_the_override() {
    common::expect_box_ok_with_stdlib(SOURCE, "BoundedParameterBridge");
}
