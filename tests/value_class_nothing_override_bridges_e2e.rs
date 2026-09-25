//! A `Nothing` override of a value-class member is reached through kotlinc's mangled bridges.
//!
//! The supertype's accessor or function returns a value class, so its JVM name carries the
//! value-class hash (`getP-<hash>`, `f-<hash>`) whether the value class is spelled boxed (`X?`) or
//! as its carrier (`X`). An override returning `Nothing`/`Nothing?` keeps its plain name and a
//! `Void` result, so the class declares one bridge per member under the supertype's mangled name.
//! Each bridge calls the override, casts its `Void` to the value class and, where the supertype
//! spells the carrier, unboxes it. The bridges follow the class's members in declaration order, a
//! property's beside a function's.

use super::common;

const SOURCE: &str = r#"@JvmInline value class Inlined(val value: Int)
interface A {
    val property: Inlined?
    val property2: Inlined
    fun foo(): Inlined?
    fun foo2(): Inlined
}
class B : A {
    override val property: Nothing? = null
    override val property2: Nothing
        get() = throw Throwable("OK")
    override fun foo(): Nothing? = null
    override fun foo2(): Nothing = throw Throwable("OK")
}
fun box(): String {
    val a: A = B()
    if (a.property != null) return "property"
    if (a.foo() != null) return "foo"
    try {
        a.property2
        return "property2"
    } catch (e: Throwable) {}
    try {
        a.foo2()
        return "foo2"
    } catch (e: Throwable) {}
    return "OK"
}
"#;

/// Both compilers' methods of `class`, disassembled verbosely with constant-pool indices
/// normalized away (two pools interned in different orders describe the same members).
fn disassembled(class: &str) -> (String, String) {
    let dir = common::scratch_dir().expect("a scratch directory");
    let (kotlinc, krusty) = (dir.join("kotlinc"), dir.join("krusty"));
    std::fs::create_dir_all(&kotlinc).expect("kotlinc output directory");
    std::fs::create_dir_all(&krusty).expect("krusty output directory");
    let source = dir.join("NothingOverrideBridges.kt");
    std::fs::write(&source, SOURCE).expect("source file");
    let (status, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        kotlinc.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(status, 0, "kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(
        SOURCE,
        "NothingOverrideBridges",
        &[common::stdlib_jar()],
    )
    .expect("krusty compiles the source");
    for (name, bytes) in classes {
        std::fs::write(krusty.join(format!("{name}.class")), bytes).expect("krusty class file");
    }
    let render = |root: &std::path::Path| {
        let text = common::javap(&["-p", "-v", "-cp", &root.to_string_lossy(), class])
            .expect("javap runs");
        let (_, members) = text.split_once("\n{").expect("javap prints the members");
        let (members, _) = members.split_once("\n}").expect("javap closes the members");
        members
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(|token| if token.starts_with('#') { "#" } else { token })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rendered = (render(&kotlinc), render(&krusty));
    let _ = std::fs::remove_dir_all(&dir);
    rendered
}

/// The overriding class declares kotlinc's members in kotlinc's order, bridges included, each
/// matching in full: flags, code, debug tables and annotations.
#[test]
fn a_nothing_override_declares_kotlincs_mangled_bridges() {
    let (reference, actual) = disassembled("B");
    assert_eq!(actual, reference);
}

/// The interface's accessor of a property typed by the value class returns the carrier and signs
/// nothing beyond its descriptor.
#[test]
fn an_abstract_value_class_accessor_signs_its_carrier() {
    let (reference, actual) = disassembled("A");
    assert_eq!(actual, reference);
}

#[test]
fn nothing_overrides_of_value_class_members_run() {
    let output = common::compile_and_run_box(
        SOURCE,
        "NothingOverrideBridgesBox",
        &[common::stdlib_jar()],
        None,
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
