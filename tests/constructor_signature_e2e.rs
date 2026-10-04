//! The generic `Signature` kotlinc writes on a constructor. The outer instance of an inner class and
//! the name and ordinal of an enum constructor are not written into it, but they decide whether it is
//! written: an enum constructor always signs its source parameters, even none (`()V`), and so does
//! the constructor of an enum entry's subclass; an inner class of a generic class signs its own
//! parameters even when none of them is generic. An enum's source parameters keep their generic
//! types.

use super::common;

const CONSTRUCTORS: &str = "enum class Tone(val label: String) {\n\
    SOFT(\"O\") { override fun mark(): String = label },\n\
    LOUD(\"K\");\n\
    open fun mark(): String = label\n\
}\n\
enum class Rule(val pick: () -> String) { FIRST({ \"O\" }), SECOND({ \"K\" }) }\n\
class Holder<T>(val item: T) {\n\
    inner class Reader { fun read(): T = item }\n\
    inner class Joined<U>(val other: U) { fun first(): T = item }\n\
    inner class Named(val text: String) { fun first(): T = item }\n\
}\n\
class Plain(val text: String) { inner class Part(val more: String) { fun all(): String = text + more } }\n\
fun box(): String {\n\
    val tones = Tone.SOFT.mark() + Tone.LOUD.mark()\n\
    val rules = Rule.FIRST.pick() + Rule.SECOND.pick()\n\
    val holder = Holder(\"O\")\n\
    val held = holder.Reader().read() + holder.Joined(\"K\").other + holder.Named(\"\").first()\n\
    val plain = Plain(\"O\").Part(\"K\").all()\n\
    return if (tones == \"OK\" && rules == \"OK\" && held == \"OKO\") plain else \"fail\"\n\
}\n";

const CLASSES: [&str; 8] = [
    "Tone",
    "Tone$SOFT",
    "Rule",
    "Holder",
    "Holder$Reader",
    "Holder$Joined",
    "Holder$Named",
    "Plain$Part",
];

#[test]
fn constructor_signature_shapes_run() {
    common::expect_box_ok_with_stdlib(CONSTRUCTORS, "ConstructorSignatures");
}

#[test]
fn constructors_sign_as_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(CONSTRUCTORS, "ConstructorSignatures");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("ConstructorSignatures.kt");
    std::fs::write(&path, CONSTRUCTORS).expect("write source");
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
            constructor_signatures(emitted),
            constructor_signatures(&reference),
            "{class}: constructor signatures"
        );
    }
}

/// Every constructor's descriptor and generic `Signature`, in classfile order.
fn constructor_signatures(bytes: &[u8]) -> Vec<(String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .filter(|method| method.name == "<init>")
        .map(|method| (method.descriptor.clone(), method.signature.clone()))
        .collect()
}
