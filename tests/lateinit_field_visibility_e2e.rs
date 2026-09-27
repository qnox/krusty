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
    protected var calculated: String = \"\"\n\
        get() = field\n\
        set(value) { field = value }\n\
    var plain: String = \"\"\n\
    fun fill() { guarded = \"g\"; hidden = \"h\"; family = \"f\"; shared = \"s\" }\n\
    fun hiddenValue(): String = hidden\n\
    inner class Probe : Holder() {\n\
        fun read(): String {\n\
            open = \"o\"; module = \"m\"; exposed = \"x\"; calculated = \"c\"; fill()\n\
            return open + module + family + guarded + hiddenValue() + exposed + shared + calculated\n\
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
    return if (read == \"omfghxscelk\") \"OK\" else \"fail: \" + read\n\
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
        // Both default and source-written accessors keep exact property/setter visibility.
        let protected_accessors = |bytes: &[u8]| {
            method_flags(bytes)
                .into_iter()
                .filter(|(name, _, _)| {
                    [
                        "getFamily",
                        "setFamily",
                        "getExposed",
                        "setExposed",
                        "getCalculated",
                        "setCalculated",
                    ]
                    .contains(&name.as_str())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            protected_accessors(emitted),
            protected_accessors(&reference),
            "{class}: accessor access flags"
        );
    }
}

/// A generated property-reference carrier is not a subclass, so it cannot directly invoke a
/// protected accessor declared in another package. The subclass that contains the reference owns
/// the static bridge; getter and setter remain independent for a public property with a protected
/// setter. The identifiers are deliberately repository-local rather than stdlib spellings so this
/// exercises generic visibility realization.
#[test]
fn cross_package_subclass_property_references_bridge_only_protected_accessors() {
    let declaration = r#"
package sample.origin

open class Vessel {
    protected var signal: String = "initial"
    var readable: String = "readable"
        protected set
}
"#;
    let subclass = r#"
package sample.consumer

class Receiver : sample.origin.Vessel() {
    fun protectedReference() = this::signal
    fun narrowedReference() = this::readable
}
"#;
    let main = r#"
fun box(): String {
    val receiver = sample.consumer.Receiver()
    val protected = receiver.protectedReference()
    protected.set("protected")
    val narrowed = receiver.narrowedReference()
    narrowed.set("setter")
    return if (protected.get() + ":" + narrowed.get() == "protected:setter") "OK" else "fail"
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Vessel.kt", declaration),
            ("Receiver.kt", subclass),
            ("Main.kt", main),
        ],
        "cross-package protected property references",
    );
}

/// A nested class and a callable-reference carrier are separate JVM classes, even though Kotlin
/// checks both accesses inside the subclass's lexical scope. Preserve the selected declaration and
/// concrete generic argument so the subclass can own one legal static bridge: its public ABI takes
/// `Long`, while the bridge alone boxes for the inherited method's erased `Object` parameter.
/// Repository-local names keep this regression independent of stdlib intrinsics.
#[test]
fn cross_package_nested_calls_keep_typed_protected_member_bridges() {
    let declaration = r#"
package sample.source

open class Reservoir<T> {
    protected fun sample(value: T): String = "sample"
    protected fun token(): String = "token"
}
"#;
    let subclass = r#"
package sample.use

class Consumer : sample.source.Reservoir<Long>() {
    inner class Nested {
        fun read(): String = sample(7L)
    }

    fun bound(): () -> String = this::token
}
"#;
    let main = r#"
fun box(): String {
    val consumer = sample.use.Consumer()
    val value = consumer.Nested().read() + ":" + consumer.bound().invoke()
    return if (value == "sample:token") "OK" else "fail: " + value
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Reservoir.kt", declaration),
            ("Consumer.kt", subclass),
            ("Main.kt", main),
        ],
        "cross-package typed protected member bridges",
    );
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
