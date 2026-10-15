//! A Kotlin class implementing the abstract members of a javac-compiled class.
//!
//! kotlinc matches an override against a Java member with the Java platform type `String!` equal
//! to either of its bounds, and a Java class's own scope merges the members it inherits by erasure
//! (`AbstractCollection.contains(Object)` implements `List<E>.contains(E)` inside
//! `java.util.AbstractList`). krusty compared the substituted signatures exactly, so every concrete
//! subclass of an abstract Java class with a reference-typed parameter was rejected as "not
//! abstract and does not implement abstract base class member" (Moshi's `JsonAdapter<T>` adapters).

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

/// Exercise the Java-scope erasure rule with repository-owned names. The JDK collection regression
/// above proves the motivating mapped-builtin case, but this one ensures the behavior is provided
/// by the normalized Java hierarchy rather than a special case for a library classifier or member.
#[test]
fn a_java_class_merges_inherited_members_by_erasure_without_stdlib_names() {
    let contract = "public interface Receiver<E> { boolean accepts(E value); }";
    let implementation =
        "public abstract class ReceiverBase { public boolean accepts(Object value) { return true; } }";
    let merged =
        "public abstract class MergedReceiver<E> extends ReceiverBase implements Receiver<E> {}";
    let main = r#"
class TextReceiver : MergedReceiver<String>()

fun box(): String = if (TextReceiver().accepts("OK")) "OK" else "FAIL"
"#;
    assert_eq!(
        common::java_interop_box(
            "abstract-java-erasure",
            &[
                ("Receiver.java", contract),
                ("ReceiverBase.java", implementation),
                ("MergedReceiver.java", merged),
            ],
            main,
        ),
        "OK"
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
        ["class 'K' is not abstract and does not implement abstract member:\nfun add(e: E): Unit"]
    );
}

/// A Java classifier may inherit two Kotlin declarations without changing their Kotlin override
/// identity. Their JVM parameter carriers are both `int`, but the concrete signed default must not
/// discharge the separately mangled unsigned abstract declaration.
#[test]
fn a_java_join_does_not_merge_ordinary_kotlin_members_by_erasure() {
    let kotlin = common::kotlinc_lib_out(&[(
        "Contracts.kt",
        "package fixture\ninterface Signed { fun f(x: Int) {} }\ninterface Unsigned { fun f(x: UInt) }\n",
    )])
    .expect("reference kotlinc is provisioned");
    let java_source = "package fixture; public abstract class Join implements Signed, Unsigned {}";
    let (java, _) = common::javac_compile(
        &[("fixture/Join.java".to_string(), java_source.to_string())],
        &[kotlin.clone(), common::stdlib_jar()],
    )
    .expect("javac compiles the Java join");
    let source = "class Derived : fixture.Join()\n";
    let result = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[kotlin, java, common::stdlib_jar()],
    );
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| fun f(x: UInt): Unit",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// A Java subclass can deliberately re-abstract a concrete superclass method. The reverse owner
/// relation must not let that older concrete declaration discharge the nearer abstract slot.
#[test]
fn a_java_subclass_can_reabstract_a_concrete_superclass_method() {
    let base = "public class ConcreteBase { public int f(int value) { return value; } }";
    let reabstract = "public abstract class Reabstract extends ConcreteBase {\n\
        @Override public abstract int f(int value);\n\
    }";
    let main = "class Derived : Reabstract()\n";
    assert_eq!(
        java_diagnostics(
            &[("ConcreteBase.java", base), ("Reabstract.java", reabstract),],
            main,
        ),
        [
            "class 'Derived' is not abstract and does not implement abstract base class member:\n\
          fun f(p0: Int): Int"
        ]
    );
}

/// kotlinc 2.4.20 reports both the unimplemented `from(String!)` and the `from(Int)` override that
/// matches nothing, offering `from(p0: String!)` as the potential signature.
#[test]
fn an_override_with_another_parameter_type_does_not_implement_the_java_method() {
    let main = "class Wrong : J3() { override fun from(r: Int): String = \"x\" }\n";
    assert_eq!(
        java_diagnostics(&[("J3.java", FROM)], main),
        [
            "class 'Wrong' is not abstract and does not implement abstract base class member:\n\
             fun from(p0: String!): String!",
            "'from' overrides nothing. Potential signatures for overriding:\n\
             fun from(p0: String!): String!",
        ]
    );
}

/// An override of `from(String!)` declared as `from(String)` is the one member of the class, so a
/// call through the subclass sees only its non-null parameter, exactly as for a Kotlin base. kotlinc
/// 2.4.20 resolves `b.from(null)` to it and reports only the null argument.
#[test]
fn an_override_of_a_java_method_replaces_the_platform_signature_at_call_sites() {
    let open = "public class J4 { public String from(String r) { return r; } }";
    let main = "class B : J4() { override fun from(r: String): String = \"x\" + r }\n\
        fun f(b: B) = b.from(null)\n";
    let kotlin_base = "open class J4 { open fun from(r: String): String = r }\n\
        class B : J4() { override fun from(r: String): String = \"x\" + r }\n\
        fun f(b: B) = b.from(null)\n";
    let expected = ["null cannot be a value of a non-null type 'String'."];
    assert_eq!(java_diagnostics(&[("J4.java", open)], main), expected);
    assert_eq!(
        common::front_end_diagnostics(kotlin_base, &[], None),
        expected
    );
}

const WILDCARD_BOX: &str = "public interface Box<T> {}\n";

const WILDCARD_CONTRACT: &str = "public interface WildcardContract<T> {\n\
    \u{20} void upper(Box<? extends T> p0);\n\
    \u{20} void lower(Box<? super T> p0);\n\
    \u{20} void star(Box<?> p0);\n\
    }\n";

/// Java-source headers preserve each use-site projection for the Kotlin override graph. A concrete
/// subclass in another file also proves that the accepted overrides discharge the inherited
/// obligations rather than only suppressing `NOTHING_TO_OVERRIDE` on their declarations.
#[test]
fn kotlin_overrides_keep_java_source_wildcard_variance() {
    let base = "abstract class KotlinContract<T : Any> : WildcardContract<T> {\n\
        \u{20} override fun upper(value: Box<out T>) {}\n\
        \u{20} override fun lower(value: Box<in T>) {}\n\
        \u{20} override fun star(value: Box<*>) {}\n\
        }\n";
    common::assert_sources_accepted_like_kotlinc(
        &[
            ("Box.java", WILDCARD_BOX),
            ("WildcardContract.java", WILDCARD_CONTRACT),
            ("Base.kt", base),
            ("Concrete.kt", "class Concrete : KotlinContract<String>()\n"),
        ],
        &[],
    );
}

/// Reversing `extends` and `super` remains a mismatch. Pin both complete diagnostic ledgers so
/// preserving a wildcard cannot silently become an over-permissive or invariant override rule.
/// The sole difference is the known nested-flexibility rendering gap: kotlinc marks the Java type
/// variable as `T!`, while krusty currently renders that nested argument as `T`.
#[test]
fn opposite_java_source_wildcard_variance_reports_exact_overrides() {
    let wrong = "abstract class Wrong<T : Any> : WildcardContract<T> {\n\
        \u{20} override fun upper(value: Box<in T>) {}\n\
        \u{20} override fun lower(value: Box<out T>) {}\n\
        \u{20} override fun star(value: Box<*>) {}\n\
        }\n";
    let sources = [
        ("Box.java", WILDCARD_BOX),
        ("WildcardContract.java", WILDCARD_CONTRACT),
        ("Wrong.kt", wrong),
    ];
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "Wrong.kt:2:3: 'upper' overrides nothing. Potential signatures for overriding:",
            "| fun upper(p0: Box<out T!>!): Unit",
            "Wrong.kt:3:3: 'lower' overrides nothing. Potential signatures for overriding:",
            "| fun lower(p0: Box<in T!>!): Unit",
        ]
    );
    assert_eq!(
        common::krusty_error_blocks_with_args(&sources, &[]),
        [
            "Wrong.kt:2:3: 'upper' overrides nothing. Potential signatures for overriding:",
            "| fun upper(p0: Box<out T>!): Unit",
            "Wrong.kt:3:3: 'lower' overrides nothing. Potential signatures for overriding:",
            "| fun lower(p0: Box<in T>!): Unit",
        ]
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
