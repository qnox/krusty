//! The generic `Signature` kotlinc writes where a value class stands in a method position. It
//! expands the value class to the type the method carries: its underlying type with the class's type
//! arguments substituted, or the upper bound for an underlying type parameter (or array of one). A
//! value class without type arguments is spelled with every declaration-site wildcard, even as a
//! return. The generated `constructor-impl` declares the class's type parameters and returns the
//! class, and the `-impl` statics take the class as their first parameter.

use super::common;

const SIGNATURES: &str = "interface Sink<in T> { fun put(value: T): String }\n\
interface Source<out T> { fun take(): T }\n\
class Label(val text: String)\n\
class Reader : Sink<Label> { override fun put(value: Label): String = value.text }\n\
class Fixed(val label: Label) : Source<Label> { override fun take(): Label = label }\n\
@JvmInline value class Plain(val sink: Sink<Label>)\n\
@JvmInline value class Produced(val source: Source<Label>)\n\
@JvmInline value class Wrapped<T>(val sink: Sink<T>)\n\
@JvmInline value class Bounded<T : Label>(val value: T)\n\
@JvmInline value class Cells<T : Label>(val cells: Array<T>)\n\
fun plain(value: Plain): Plain = value\n\
fun produced(value: Produced): Produced = value\n\
fun wrapped(value: Wrapped<Label>): Wrapped<Label> = value\n\
fun <T> generic(value: Wrapped<T>): Wrapped<T> = value\n\
fun bounded(value: Bounded<Label>): Bounded<Label> = value\n\
fun cells(value: Cells<Label>): Cells<Label> = value\n\
fun box(): String {\n\
    val first = plain(Plain(Reader())).sink.put(Label(\"O\"))\n\
    val second = generic(wrapped(Wrapped(Reader()))).sink.put(produced(Produced(Fixed(Label(\"K\")))).source.take())\n\
    val third = bounded(Bounded(Label(\"\"))).value.text\n\
    return first + second + third + cells(Cells(arrayOf(Label(\"\")))).cells[0].text\n\
}\n";

const CLASSES: [&str; 6] = [
    "Plain",
    "Produced",
    "Wrapped",
    "Bounded",
    "Cells",
    "ValueClassSignaturesKt",
];

#[test]
fn value_class_signature_shapes_run() {
    common::expect_box_ok_with_stdlib(SIGNATURES, "ValueClassSignatures");
}

#[test]
fn value_class_positions_sign_as_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(SIGNATURES, "ValueClassSignatures");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("ValueClassSignatures.kt");
    std::fs::write(&path, SIGNATURES).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in CLASSES {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            method_signatures(emitted),
            method_signatures(&reference),
            "{class}: generic method signatures"
        );
    }
}

/// Every method's name, descriptor and generic `Signature`, in classfile order.
fn method_signatures(bytes: &[u8]) -> Vec<(String, String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
            )
        })
        .collect()
}
