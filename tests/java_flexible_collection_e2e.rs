//! A Java collection parameter or result reaches Kotlin as the flexible `(Mutable)X<..>!`. krusty
//! carries its mutable face under platform nullability; wherever that face is an EXPECTATION for
//! a generic call's result (a `T` bound to `MutableMap<String!, Any!>!` handed to `emptyMap()`),
//! the read-only face must bind the result variables too, as kotlinc binds them. Measured against
//! kotlinc 2.4.10: every shape here compiles and runs.

use super::common;

/// `java_interop_box` with the stdlib beside the Java fixture: compile the fixture, compile the
/// Kotlin `Use.kt` against fixture + stdlib + JDK, run a Java driver printing `UseKt.box()`.
fn java_box_with_stdlib(tag: &str, java_sources: &[(&str, &str)], use_src: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let sources: Vec<(String, String)> = java_sources
        .iter()
        .map(|(name, source)| ((*name).to_string(), (*source).to_string()))
        .collect();
    let (cp, _) = common::javac_compile(&sources, &[])
        .unwrap_or_else(|| panic!("{tag}: pooled javac failed on the Java fixture"));
    let out = common::scratch_dir().unwrap_or_else(|| panic!("{tag}: scratch unavailable"));
    let classpath = [cp.clone(), stdlib.clone()];
    if common::compile_to_dir(use_src, "Use", &classpath, Some(jdk.as_path()), &out).is_none() {
        let diagnostics = common::front_end_diagnostics(use_src, &classpath, Some(jdk.as_path()));
        panic!("{tag}: krusty failed against the Java fixture; diagnostics: {diagnostics:?}");
    }
    let main =
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let driver = out.join("M.java");
    std::fs::write(&driver, main).unwrap_or_else(|e| panic!("{tag}: write driver: {e}"));
    let run_cp = format!("{}:{}:{}", out.display(), cp.display(), stdlib.display());
    common::javac_run(
        &driver.to_string_lossy(),
        &run_cp,
        &out.to_string_lossy(),
        "M",
    )
    .unwrap_or_else(|| panic!("{tag}: pooled JavaRunner unavailable"))
    .trim()
    .to_string()
}

const ATTRS_JAVA: &str = "import java.util.Map;\n\
public interface JAttrs {\n\
    Map<String, Object> getAttributes();\n\
    void setAttributes(Map<String, Object> attributes);\n\
}\n";

#[test]
fn a_java_map_expectation_binds_a_read_only_generic_result() {
    // `Stub<T>.returns(value: T)` with `T := MutableMap<String!, Any!>!` from the Java getter.
    let use_src = "class Stub<T> { infix fun returns(value: T): Stub<T> = this }\n\
fun <T> every(block: () -> T): Stub<T> = Stub()\n\
object Holder : JAttrs {\n\
    private var map: MutableMap<String, Any> = mutableMapOf(\"k\" to 1)\n\
    override fun getAttributes(): MutableMap<String, Any> = map\n\
    override fun setAttributes(attributes: MutableMap<String, Any>) { map = attributes }\n\
}\n\
fun box(): String {\n\
    val j: JAttrs = Holder\n\
    val stub = every { j.attributes } returns emptyMap()\n\
    val direct = every { j.attributes } returns mapOf(\"a\" to 2)\n\
    j.setAttributes(emptyMap())\n\
    return if (stub === stub && direct === direct && j.attributes.isEmpty()) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        java_box_with_stdlib(
            "java-map-expectation",
            &[("JAttrs.java", ATTRS_JAVA)],
            use_src
        ),
        "OK"
    );
}

#[test]
fn a_kotlin_mutable_map_expectation_still_rejects_a_read_only_result() {
    // No flexibility without a Java origin: kotlinc rejects `Map<K, V>` for a Kotlin
    // `MutableMap<String, Any>` parameter.
    let src = "class Stub<T> { infix fun returns(value: T): Stub<T> = this }\n\
fun <T> every(block: () -> T): Stub<T> = Stub()\n\
interface KAttrs { val attributes: MutableMap<String, Any> }\n\
fun f(k: KAttrs) { every { k.attributes } returns emptyMap() }\n";
    let jdk = common::jdk_modules();
    let diagnostics =
        common::front_end_diagnostics(src, &[common::stdlib_jar()], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("type mismatch")
                || diagnostic.contains("none of the following candidates")),
        "{diagnostics:?}"
    );
}
