//! Storage of a class's constructor prefix: a value a local class or anonymous object captures, and
//! the enclosing instance it keeps. kotlinc makes each such field package-visible
//! `final synthetic`, so a class nested in the capturing class reads it directly. A private field
//! fails to link there with `IllegalAccessError`.

use super::common;

/// A local class captures `seed` and the enclosing `Sower`, and its inner class reads both through
/// the enclosing instance of the local class.
const LOCAL_CLASS: &str = "class Sower {\n\
    fun plant(seed: String): String {\n\
        class Row {\n\
            inner class Furrow {\n\
                fun both() = seed + seed + tag()\n\
            }\n\
        }\n\
        return Row().Furrow().both()\n\
    }\n\
    fun tag() = \"!\"\n\
}\n\
fun box(): String {\n\
    val grown = Sower().plant(\"O\")\n\
    return if (grown == \"OO!\") \"OK\" else \"fail: \" + grown\n\
}\n";

/// An anonymous object keeps its enclosing instance, and a class nested in it reads it.
const ANONYMOUS_OBJECT: &str = "class Lantern(val glow: String) {\n\
    fun light(): String {\n\
        val wick = object {\n\
            inner class Flame { fun shine() = glow }\n\
            fun burn() = Flame().shine()\n\
        }\n\
        return wick.burn()\n\
    }\n\
}\n\
fun box(): String = Lantern(\"OK\").light()\n";

#[test]
fn inner_class_of_local_class_reads_its_captures() {
    common::expect_box_ok_with_stdlib(LOCAL_CLASS, "Sower");
}

#[test]
fn class_nested_in_anonymous_object_reads_its_enclosing_instance() {
    common::expect_box_ok_with_stdlib(ANONYMOUS_OBJECT, "Lantern");
}

#[test]
fn captured_field_flags_match_kotlinc() {
    for (name, src, classes) in [
        (
            "Sower",
            LOCAL_CLASS,
            &["Sower$plant$Row", "Sower$plant$Row$Furrow"][..],
        ),
        (
            "Lantern",
            ANONYMOUS_OBJECT,
            &["Lantern$light$wick$1", "Lantern$light$wick$1$Flame"][..],
        ),
    ] {
        let krusty = common::expect_classes_with_stdlib(src, name);
        let dir = common::scratch_dir().expect("scratch directory");
        let path = dir.join(format!("{name}.kt"));
        std::fs::write(&path, src).expect("write source");
        let out = dir.join("ref");
        let (code, stderr) = common::kotlinc_compile(&[
            "-d".to_string(),
            out.to_string_lossy().into_owned(),
            path.to_string_lossy().into_owned(),
        ])
        .expect("reference kotlinc");
        assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
        for class in classes {
            let reference = std::fs::read(out.join(format!("{class}.class")))
                .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
            let (_, emitted) = krusty
                .iter()
                .find(|(emitted, _)| emitted == class)
                .unwrap_or_else(|| panic!("krusty did not emit {class}"));
            assert_eq!(
                field_flags(emitted),
                field_flags(&reference),
                "{class}: field access flags"
            );
        }
    }
}

/// The access flags of every field, in a stable order. Only the flags are compared: krusty still
/// spells a local class's captured enclosing instance `$this$0` where kotlinc writes `this$0`.
fn field_flags(bytes: &[u8]) -> Vec<u16> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    let mut flags: Vec<u16> = class.fields.iter().map(|field| field.access).collect();
    flags.sort();
    flags
}
