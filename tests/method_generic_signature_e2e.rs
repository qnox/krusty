//! The generic `Signature` attribute kotlinc writes on a method. It writes none on a synthetic
//! method (a value class's private constructor), none on a lambda body, and none that only spells
//! the erased descriptor back.

use super::common;

const SIGNATURES: &str = "class Tag<T>(val label: T)\n\
@JvmInline value class Slot(val tag: Tag<String>)\n\
class Holder(val tags: Array<Tag<String>>, val plain: Array<String>)\n\
fun <T> apply(tag: Tag<T>, step: (Tag<T>) -> Tag<T>): Tag<T> = step(tag)\n\
fun box(): String {\n\
    val tag = apply(Tag(\"OK\")) { seen: Tag<String> -> seen }\n\
    val holder = Holder(arrayOf(tag), arrayOf(\"x\"))\n\
    return Slot(holder.tags[0]).tag.label\n\
}\n";

#[test]
fn signature_shapes_run() {
    common::expect_box_ok_with_stdlib(SIGNATURES, "Signatures");
}

#[test]
fn generic_signatures_match_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(SIGNATURES, "Signatures");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("Signatures.kt");
    std::fs::write(&path, SIGNATURES).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    // Only the methods this test is about: the value class's other members follow the value-class
    // lowering, which is covered elsewhere.
    for (class, methods) in [
        ("Slot", &["<init>"][..]),
        ("Holder", &["<init>", "getTags", "getPlain"][..]),
        ("SignaturesKt", &["apply", "box", "box$lambda$0"][..]),
    ] {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            method_signatures(emitted, methods),
            method_signatures(&reference, methods),
            "{class}: generic method signatures"
        );
    }
}

/// The named methods' descriptor and generic `Signature`, in classfile order.
fn method_signatures(bytes: &[u8], methods: &[&str]) -> Vec<(String, String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .filter(|method| methods.contains(&method.name.as_str()))
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
            )
        })
        .collect()
}
