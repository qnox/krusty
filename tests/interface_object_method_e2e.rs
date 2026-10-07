//! A Java interface method that only redeclares `Object.toString`, `hashCode`, or `equals` is not
//! a Kotlin member. The call resolves to `kotlin.Any` and compiles as `invokevirtual
//! java/lang/Object.*` with no expression null check. A class that declares the method keeps it:
//! `StringBuilder.toString()` is enhanced and checked, and an inherited call names the receiver
//! class (`Number.toString`).

use super::common;

const JDK: &str = r#"
fun path(p: java.nio.file.Path): String = p.toString()
fun pathEq(p: java.nio.file.Path, o: Any?): Boolean = p.equals(o)
fun pathHc(p: java.nio.file.Path): Int = p.hashCode()
fun fileName(p: java.nio.file.Path): String = p.fileName.toString()
fun parent(p: java.nio.file.Path): String = p.parent.toString()
fun sequence(c: CharSequence): String = c.toString()
fun comparable(c: Comparable<String>): String = c.toString()
fun runnable(r: Runnable): String = r.toString()
fun number(n: Number): String = n.toString()
fun builder(s: StringBuilder): String = s.toString()
fun builderEq(s: StringBuilder, o: Any?): Boolean = s.equals(o)
fun file(f: java.io.File): String = f.toString()
fun thrown(t: Throwable): String = t.toString()
fun any(a: Any): String = a.toString()
fun annotation(a: Annotation): String = a.toString()
fun text(s: String): String = s.toString()
"#;

const INTERFACE_JAVA: &str = "\
public interface ObjectMethods {\n\
    String toString();\n\
    boolean equals(Object other);\n\
    int hashCode();\n\
    String name();\n\
}\n";

const CLASS_JAVA: &str = "\
public class ObjectCarrier {\n\
    public String toString() { return \"c\"; }\n\
    public boolean equals(Object other) { return false; }\n\
    public int hashCode() { return 1; }\n\
    public String name() { return \"n\"; }\n\
}\n";

const ABSTRACT_JAVA: &str = "\
public abstract class PlainCarrier {\n\
    public abstract String name();\n\
}\n";

const DECLARED: &str = r#"
fun iface(x: ObjectMethods): String = x.toString()
fun ifaceEq(x: ObjectMethods, o: Any?): Boolean = x.equals(o)
fun ifaceHc(x: ObjectMethods): Int = x.hashCode()
fun ifaceName(x: ObjectMethods): String = x.name()
fun carrier(x: ObjectCarrier): String = x.toString()
fun carrierEq(x: ObjectCarrier, o: Any?): Boolean = x.equals(o)
fun carrierHc(x: ObjectCarrier): Int = x.hashCode()
fun carrierName(x: ObjectCarrier): String = x.name()
fun plain(x: PlainCarrier): String = x.toString()
fun plainName(x: PlainCarrier): String = x.name()

class Impl : ObjectMethods {
    override fun name(): String = "n"
    override fun toString(): String = "impl"
}

fun impl(x: Impl): String = x.toString()
"#;

#[test]
fn jdk_object_methods_match_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk("ObjectMethodJdk", JDK, &["ObjectMethodJdkKt"]);
}

#[test]
fn a_declared_interface_object_method_is_any() {
    let java = common::javac_compile(
        &[
            ("ObjectMethods.java".to_string(), INTERFACE_JAVA.to_string()),
            ("ObjectCarrier.java".to_string(), CLASS_JAVA.to_string()),
            ("PlainCarrier.java".to_string(), ABSTRACT_JAVA.to_string()),
        ],
        &[],
    )
    .expect("javac compiles the fixtures")
    .0;
    let scratch = common::scratch_dir().expect("scratch directory");
    let reference = scratch.join("reference");
    std::fs::create_dir_all(&reference).expect("reference directory");
    let source = scratch.join("Declared.kt");
    std::fs::write(&source, DECLARED).expect("write the Kotlin fixture");
    let Some((status, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-cp".to_string(),
        java.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]) else {
        panic!("reference kotlinc unavailable");
    };
    assert_eq!(status, 0, "kotlinc rejected the fixture: {stderr}");
    let classpath = [java.clone(), common::stdlib_jar()];
    let classes = common::compile_in_process(
        DECLARED,
        "Declared",
        &classpath,
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| {
        panic!(
            "{:?}",
            common::compile_in_process_diagnostics(
                DECLARED,
                "Declared",
                &classpath,
                Some(common::jdk_modules().as_path()),
            )
        )
    });
    let reference_classes = ["DeclaredKt", "Impl"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                std::fs::read(reference.join(format!("{name}.class")))
                    .unwrap_or_else(|error| panic!("read kotlinc {name}: {error}")),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let krusty_classes = classes
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        krusty_classes, reference_classes,
        "every emitted class must be byte-identical to kotlinc"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn path_and_builder_to_string_run() {
    common::expect_box_same_as_kotlinc(
        r#"
fun box(): String {
    val path = java.nio.file.Path.of("dir", "file")
    if (!path.toString().endsWith("file")) return "path " + path.toString()
    if (path.fileName.toString() != "file") return "fileName"
    val built = StringBuilder("OK").toString()
    return if (built == "OK") "OK" else built
}
"#,
        "ObjectMethodBox",
    );
}
