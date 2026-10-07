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
    let methods = [
        "public static final java.lang.String path(",
        "public static final boolean pathEq(",
        "public static final int pathHc(",
        "public static final java.lang.String fileName(",
        "public static final java.lang.String parent(",
        "public static final java.lang.String sequence(",
        "public static final java.lang.String comparable(",
        "public static final java.lang.String runnable(",
        "public static final java.lang.String number(",
        "public static final java.lang.String builder(",
        "public static final boolean builderEq(",
        "public static final java.lang.String file(",
        "public static final java.lang.String thrown(",
        "public static final java.lang.String any(",
        "public static final java.lang.String annotation(",
        "public static final java.lang.String text(",
    ];
    let results = common::method_code_diffs_against_kotlinc(
        "ObjectMethodJdk",
        &[],
        JDK,
        "ObjectMethodJdkKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    for (method, result) in methods.into_iter().zip(results) {
        result.unwrap_or_else(|difference| panic!("{method}: {difference}"));
    }
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
    let actual_dir = scratch.join("krusty");
    std::fs::create_dir_all(&reference).expect("reference directory");
    std::fs::create_dir_all(&actual_dir).expect("krusty directory");
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
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "DeclaredKt")
        .expect("krusty emitted DeclaredKt");
    let krusty_class = actual_dir.join("DeclaredKt.class");
    std::fs::write(&krusty_class, bytes).expect("write krusty class");
    let methods = [
        "public static final java.lang.String iface(",
        "public static final boolean ifaceEq(",
        "public static final int ifaceHc(",
        "public static final java.lang.String ifaceName(",
        "public static final java.lang.String carrier(",
        "public static final boolean carrierEq(",
        "public static final int carrierHc(",
        "public static final java.lang.String carrierName(",
        "public static final java.lang.String plain(",
        "public static final java.lang.String plainName(",
        "public static final java.lang.String impl(",
    ];
    for method in methods {
        let expected = method_body(&reference, "DeclaredKt", method);
        let actual = method_body(&actual_dir, "DeclaredKt", method);
        assert_eq!(actual, expected, "{method}");
    }
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

/// Instruction text and the local-variable table, with line numbers omitted. Constant-pool indices
/// collapse so two equal sequences still compare when their pools differ.
fn method_body(dir: &std::path::Path, class: &str, method: &str) -> String {
    let text = common::javap(&["-p", "-c", "-l", "-cp", &dir.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("javap {class}"));
    let mut body = String::new();
    let mut inside = false;
    let mut skipping_lines = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(method) && trimmed.contains('(') {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        if trimmed == "Code:" {
            continue;
        }
        if trimmed == "LineNumberTable:" {
            skipping_lines = true;
            continue;
        }
        if trimmed == "LocalVariableTable:" {
            skipping_lines = false;
            body.push_str("LocalVariableTable\n");
            continue;
        }
        if skipping_lines {
            continue;
        }
        let mut parts = trimmed.splitn(2, "//");
        let code = parts.next().unwrap_or(trimmed).trim();
        let normalized = code
            .split_whitespace()
            .map(|token| if token.starts_with('#') { "#" } else { token })
            .collect::<Vec<_>>()
            .join(" ");
        body.push_str(normalized.trim_end_matches(','));
        if let Some(reference) = parts.next() {
            body.push_str(" // ");
            body.push_str(reference.trim());
        }
        body.push('\n');
    }
    assert!(!body.is_empty(), "javap has no method {method}");
    body
}
