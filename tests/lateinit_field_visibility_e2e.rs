//! kotlinc publishes a `lateinit var`'s backing field at its setter's visibility rather than
//! keeping it private: the field is the property's storage, reachable from wherever the property
//! can be assigned. `internal` has no JVM spelling and is public.

use super::common;

/// Every visibility a `lateinit var` and its setter can take, in a class and an object, with a
/// subclass and an inner class that assign and read them. (`internal … protected set` is not
/// Kotlin, since a setter may not be more visible than its property, and an object takes no
/// `protected` member.)
const LATEINIT: &str = "open class Holder {\n\
    lateinit var open: String\n\
    internal lateinit var module: String\n\
    protected lateinit var family: String\n\
    private lateinit var hidden: String\n\
    lateinit var guarded: String\n\
        private set\n\
    lateinit var exposed: String\n\
        protected set\n\
    internal lateinit var shared: String\n\
        private set\n\
    var plain: String = \"\"\n\
    fun fill() { guarded = \"g\"; hidden = \"h\"; family = \"f\"; shared = \"s\" }\n\
    fun hiddenValue(): String = hidden\n\
    inner class Probe : Holder() {\n\
        fun read(): String {\n\
            open = \"o\"; module = \"m\"; exposed = \"x\"; fill()\n\
            return open + module + family + guarded + hiddenValue() + exposed + shared\n\
        }\n\
    }\n\
}\n\
object Registry {\n\
    lateinit var entry: String\n\
    lateinit var locked: String\n\
        private set\n\
    internal lateinit var kept: String\n\
        private set\n\
    fun lock() { locked = \"l\"; kept = \"k\" }\n\
}\n\
fun box(): String {\n\
    Registry.entry = \"e\"\n\
    Registry.lock()\n\
    val read = Holder().Probe().read() + Registry.entry + Registry.locked + Registry.kept\n\
    return if (read == \"omfghxselk\") \"OK\" else \"fail: \" + read\n\
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
        // A narrowed setter keeps its own visibility. (kotlinc omits a private default setter,
        // protects both accessors of a protected property and mangles an internal accessor's
        // name; those method-shape gaps are not this fixture's subject.)
        let setters = |bytes: &[u8]| {
            method_flags(bytes)
                .into_iter()
                .filter(|(name, _, _)| ["setExposed"].contains(&name.as_str()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            setters(emitted),
            setters(&reference),
            "{class}: narrowed setter access flags"
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

/// Each method's name, descriptor and access flags, in classfile order.
fn method_flags(bytes: &[u8]) -> Vec<(String, String, u16)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect()
}

/// A `protected set` narrows the setter the frontend checks as well: a write from outside the
/// class hierarchy is rejected where kotlinc rejects it, while the read stays public.
#[test]
fn a_write_through_a_protected_setter_outside_its_hierarchy_is_rejected() {
    use super::diagnostics_parity_support::{errors, ObservedError};
    let declaration = "open class Guarded {\n    var count: Int = 1\n        protected set\n}\n";
    let use_site = "fun read(guarded: Guarded): Int = guarded.count\n\
                    fun write(guarded: Guarded) { guarded.count = 2 }\n";
    let result = common::compiler_diagnostics(
        &[("Decl.kt", declaration), ("Use.kt", use_site)],
        &[common::stdlib_jar()],
    );
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let expected = |message: &str| {
        vec![ObservedError {
            file: "Use.kt".to_string(),
            line: 2,
            column: 39,
            message: message.to_string(),
        }]
    };
    assert_eq!(
        krusty,
        expected("cannot access 'count': it is protected in 'Guarded'")
    );
    assert_eq!(
        errors(&result.reference_stderr),
        expected("cannot access 'count': it is protected in 'Guarded'.")
    );
}
