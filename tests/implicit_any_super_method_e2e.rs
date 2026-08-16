//! `super.equals(…)` / `super.hashCode()` / `super.toString()` resolve through the implicit `Any`
//! supertype even when a class declares only interfaces, or nothing at all.
//!
//! The super-call path looked at the declared SUPERCLASS and then at superinterface defaults. A class
//! whose only declared supertype is an interface has neither, so the call died as
//! "krusty: unresolved super method 'equals'" — although every class extends `Any`/`java.lang.Object`,
//! which is what kotlinc calls (`invokespecial java/lang/Object.equals`).
//!
//! Found by diffing krusty-lsp against the JetBrains Kotlin language server on intellij-community's
//! `fleet.fastutil` `IntOpenHashSet`, which is declared `: MutableIntSet` and calls `super.equals`;
//! the reference server reports nothing there.

use std::fs;

use super::common;

const EMISSION_SOURCE: &str = "interface Marker\n\
class Probe : Marker {\n\
\x20   fun same(other: Any?): Boolean = super.equals(other)\n\
\x20   fun identityHash(): Int = super.hashCode()\n\
\x20   fun identityText(): String = super.toString()\n\
}\n";

fn object_super_targets(disassembly: &str) -> Vec<&str> {
    disassembly
        .lines()
        .filter(|line| line.contains("invokespecial"))
        .filter_map(|line| line.split("// Method ").nth(1))
        .filter(|target| {
            [
                "java/lang/Object.equals:",
                "java/lang/Object.hashCode:",
                "java/lang/Object.toString:",
            ]
            .iter()
            .any(|method| target.starts_with(method))
        })
        .map(str::trim)
        .collect()
}

#[test]
fn super_equals_and_hash_code_resolve_with_only_an_interface_supertype() {
    let source = "interface Marker\n\
class Plain(val tag: String) : Marker {\n\
\x20   override fun equals(other: Any?): Boolean {\n\
\x20       if (super.equals(other)) return true\n\
\x20       return other is Plain && other.tag == tag\n\
\x20   }\n\
\x20   override fun hashCode(): Int = super.hashCode() + tag.length\n\
}\n\
fun box(): String {\n\
\x20   val a = Plain(\"x\")\n\
\x20   if (!a.equals(a)) return \"fail: identity\"\n\
\x20   if (!a.equals(Plain(\"x\"))) return \"fail: equal tags\"\n\
\x20   if (a.equals(Plain(\"y\"))) return \"fail: different tags\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(source, "IASM1");
}

#[test]
fn super_to_string_resolves_with_no_declared_supertype() {
    let source = "class Bare {\n\
\x20   override fun toString(): String = super.toString()\n\
}\n\
fun box(): String {\n\
\x20   val text = Bare().toString()\n\
\x20   return if (text.startsWith(\"Bare@\")) \"OK\" else \"fail:\" + text\n\
}\n";
    common::expect_box_ok_with_stdlib(source, "IASM2");
}

#[test]
fn implicit_any_super_dispatch_matches_kotlinc_physical_targets() {
    let classes = common::expect_classes_with_stdlib(EMISSION_SOURCE, "IASMEmit");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Probe")
        .expect("krusty emitted Probe");
    let dir = common::scratch_dir().expect("scratch dir");
    let krusty_class = dir.join("Probe.class");
    fs::write(&krusty_class, bytes).expect("write krusty class");
    let krusty = common::javap(&["-c", "-p", &krusty_class.to_string_lossy()])
        .expect("pooled javap available");

    let reference = dir.join("kotlinc");
    fs::create_dir_all(&reference).expect("create kotlinc output");
    let source = dir.join("IASMEmit.kt");
    fs::write(&source, EMISSION_SOURCE).expect("write kotlinc source");
    let args = vec![
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("kotlinc available");
    assert_eq!(code, 0, "kotlinc rejected the reference source: {stderr}");
    let reference_class = reference.join("Probe.class");
    let kotlinc = common::javap(&["-c", "-p", &reference_class.to_string_lossy()])
        .expect("pooled javap available");

    let expected = vec![
        "java/lang/Object.equals:(Ljava/lang/Object;)Z",
        "java/lang/Object.hashCode:()I",
        "java/lang/Object.toString:()Ljava/lang/String;",
    ];
    assert_eq!(object_super_targets(&krusty), expected);
    assert_eq!(
        object_super_targets(&krusty),
        object_super_targets(&kotlinc)
    );
    fs::remove_dir_all(dir).expect("remove scratch dir");
}
