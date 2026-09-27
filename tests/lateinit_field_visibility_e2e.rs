//! kotlinc publishes a `lateinit var`'s backing field at its setter's visibility rather than
//! keeping it private: the field is the property's storage, reachable from wherever the property
//! can be assigned. `internal` has no JVM spelling and is public.

use super::common;

/// Every visibility a `lateinit var` and its setter can take, in a class and an object, with a
/// subclass and an inner class that assign and read them.
const LATEINIT: &str = "open class Holder {\n\
    lateinit var open: String\n\
    internal lateinit var module: String\n\
    protected lateinit var family: String\n\
    private lateinit var hidden: String\n\
    lateinit var guarded: String\n\
        private set\n\
    var plain: String = \"\"\n\
    fun fill() { guarded = \"g\"; hidden = \"h\"; family = \"f\" }\n\
    fun hiddenValue(): String = hidden\n\
    inner class Probe : Holder() {\n\
        fun read(): String { open = \"o\"; module = \"m\"; fill(); return open + module + family + guarded + hiddenValue() }\n\
    }\n\
}\n\
object Registry { lateinit var entry: String }\n\
fun box(): String {\n\
    Registry.entry = \"e\"\n\
    val read = Holder().Probe().read() + Registry.entry\n\
    return if (read == \"omfghe\") \"OK\" else \"fail: \" + read\n\
}\n";

#[test]
fn lateinit_fields_run() {
    common::expect_box_ok_with_stdlib(LATEINIT, "Lateinit");
}

#[test]
fn lateinit_fields_take_their_setters_visibility_like_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(LATEINIT, "Lateinit");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("Lateinit.kt");
    std::fs::write(&path, LATEINIT).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in ["Holder", "Holder$Probe", "Registry"] {
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

/// Each field's name, descriptor and access flags, in classfile order.
fn field_flags(bytes: &[u8]) -> Vec<(String, String, u16)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.descriptor.clone(), field.access))
        .collect()
}
