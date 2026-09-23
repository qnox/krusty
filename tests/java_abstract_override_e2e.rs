//! A Kotlin class implementing the abstract members of a javac-compiled class.
//!
//! kotlinc matches an override against a Java member with the Java platform type `String!` equal
//! to either of its bounds, and a Java class's own scope merges the members it inherits by erasure
//! (`AbstractCollection.contains(Object)` implements `List<E>.contains(E)` inside
//! `java.util.AbstractList`). krusty compared the substituted signatures exactly, so every concrete
//! subclass of an abstract Java class with a reference-typed parameter was rejected as "not
//! abstract and does not implement all abstract members" (Moshi's `JsonAdapter<T>` adapters).

use super::common;

const FROM: &str = "public abstract class J3 { public abstract String from(String r); }";

const ADAPTER: &str = "public abstract class Adapter<T> {\n\
    public abstract T fromJson(String s);\n\
    public abstract void toJson(StringBuilder w, T value);\n\
    public final String roundTrip(String s) {\n\
        StringBuilder b = new StringBuilder();\n\
        toJson(b, fromJson(s));\n\
        return b.toString();\n\
    }\n\
}";

#[test]
fn kotlin_class_implements_an_abstract_java_method_with_reference_types() {
    let main = r#"
class A : J3() { override fun from(r: String): String = "O" + r }
class Lenient : J3() { override fun from(r: String?): String? = r }

fun box(): String {
    val viaBase: J3 = A()
    return if (Lenient().from(null) == null) viaBase.from("K") else "FAIL"
}
"#;
    assert_eq!(
        common::java_interop_box("abstract-java-ref", &[("J3.java", FROM)], main),
        "OK"
    );
}

/// The Moshi `JsonAdapter<T>` shape: the abstract members mention the class type parameter, so the
/// overrides are reached through bridges from Java's erased `Object` signatures.
#[test]
fn kotlin_class_implements_a_generic_abstract_java_adapter() {
    let main = r#"
class Foo(val v: String)

class FooAdapter : Adapter<Foo>() {
    override fun fromJson(s: String): Foo? = if (s.isEmpty()) null else Foo(s)
    override fun toJson(w: StringBuilder, value: Foo?) {
        w.append(if (value == null) "null" else value.v)
    }
}

fun box(): String {
    val adapter = FooAdapter()
    return if (adapter.roundTrip("") == "null") adapter.roundTrip("OK") else "FAIL"
}
"#;
    assert_eq!(
        common::java_interop_box("abstract-java-adapter", &[("Adapter.java", ADAPTER)], main),
        "OK"
    );
}

/// kotlinc's class for the plain override is the reference: no bridge, a non-null parameter check.
#[test]
fn abstract_java_method_override_matches_kotlinc_bytes() {
    let (java, _) = common::javac_compile(&[("J3.java".to_string(), FROM.to_string())], &[])
        .expect("javac compiles the Java fixture");
    let main = "class A : J3() { override fun from(r: String): String = \"x\" + r }\n";
    let scratch = common::scratch_dir().expect("scratch directory");
    let reference = scratch.join("reference");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source = scratch.join("A.kt");
    std::fs::write(&source, main).expect("write the Kotlin fixture");
    let Some((status, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-cp".to_string(),
        java.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(status, 0, "kotlinc rejected the fixture: {stderr}");
    let expected = std::fs::read(reference.join("A.class")).expect("kotlinc emitted A");
    let classes = common::compile_in_process_metadata_cp_module_target(
        main,
        "A",
        &[java, common::stdlib_jar(), common::jdk_modules()],
        "main",
        None,
    )
    .expect("krusty compiles the override");
    let actual = classes
        .iter()
        .find(|(name, _)| name == "A")
        .map(|(_, bytes)| bytes)
        .expect("krusty emitted A");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        actual == &expected,
        "A.class differs from kotlinc ({} vs {} bytes)",
        actual.len(),
        expected.len()
    );
}

/// JDK skeleton collections implement most `MutableList`/`MutableCollection` members in Java with
/// `Object` parameters; those Java implementations discharge the Kotlin obligations.
#[test]
fn kotlin_classes_extend_abstract_jdk_collections() {
    let main = r#"
class Seven : java.util.AbstractList<Int>() {
    override val size: Int get() = 1
    override fun get(index: Int): Int = 7
}

class Words : java.util.AbstractCollection<String>() {
    override val size: Int get() = 1
    override fun iterator(): MutableIterator<String> = mutableListOf("O").iterator()
}

class Letters : java.util.AbstractSet<String>() {
    override val size: Int get() = 1
    override fun iterator(): MutableIterator<String> = mutableListOf("K").iterator()
}

fun box(): String {
    val seven = Seven()
    if (seven.size != 1 || seven[0] != 7 || !seven.contains(7) || seven.indexOf(7) != 0) {
        return "FAIL: list"
    }
    if (seven.contains(3) || !Words().contains("O")) return "FAIL: collection"
    return Words().first() + Letters().first()
}
"#;
    let jdk = common::jdk_modules();
    let classpath = common::classpath_jars_for(main);
    assert_eq!(
        common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path())).as_deref(),
        Some("OK")
    );
}

/// The Java-scope erasure rule applies only where one Java classifier inherits both members.
/// `JBase.add(Object)` erases like `Sink<String>.add(String)`, but nothing in Java merges the two,
/// so the Kotlin class still owes `add(String)` (kotlinc: "does not implement abstract member").
#[test]
fn an_unrelated_java_method_does_not_implement_a_kotlin_interface_member() {
    let base = "public abstract class JBase { public void add(Object o) {} }";
    let main = "interface Sink<E> { fun add(e: E) }\nclass K : JBase(), Sink<String>\n";
    assert_eq!(
        java_diagnostics(&[("JBase.java", base)], main),
        ["class 'K' is not abstract and does not implement all abstract members"]
    );
}

#[test]
fn an_override_with_another_parameter_type_does_not_implement_the_java_method() {
    let main = "class Wrong : J3() { override fun from(r: Int): String = \"x\" }\n";
    assert_eq!(
        java_diagnostics(&[("J3.java", FROM)], main),
        ["class 'Wrong' is not abstract and does not implement all abstract members"]
    );
}

/// An override of `from(String!)` declared as `from(String)` is the one member of the class, so a
/// call through the subclass sees only its non-null parameter, exactly as for a Kotlin base.
#[test]
fn an_override_of_a_java_method_replaces_the_platform_signature_at_call_sites() {
    let open = "public class J4 { public String from(String r) { return r; } }";
    let main = "class B : J4() { override fun from(r: String): String = \"x\" + r }\n\
        fun f(b: B) = b.from(null)\n";
    let kotlin_base = "open class J4 { open fun from(r: String): String = r }\n\
        class B : J4() { override fun from(r: String): String = \"x\" + r }\n\
        fun f(b: B) = b.from(null)\n";
    let expected = [
        "none of the following candidates is applicable:\n\nfun from(r: String): String",
        "null cannot be a value of a non-null type 'String'.",
    ];
    assert_eq!(java_diagnostics(&[("J4.java", open)], main), expected);
    assert_eq!(
        common::front_end_diagnostics(kotlin_base, &[], None),
        expected
    );
}

fn java_diagnostics(java: &[(&str, &str)], main: &str) -> Vec<String> {
    let sources = java
        .iter()
        .map(|(name, source)| (name.to_string(), source.to_string()))
        .collect::<Vec<_>>();
    let (java, _) = common::javac_compile(&sources, &[]).expect("javac compiles the Java fixture");
    let jdk = common::jdk_modules();
    let mut classpath = common::classpath_jars_for(main);
    classpath.push(java.clone());
    let diagnostics = common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()));
    if let Some(root) = java.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    diagnostics
}
