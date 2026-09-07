//! kotlinc's last specificity tie-break: when two candidates are equally specific for a call, a
//! non-parameterized callable wins over a parameterized one (Kotlin spec 11.7). JUnit's
//! `assertDoesNotThrow(Executable)` beside `<T> assertDoesNotThrow(ThrowingSupplier<T>)` is the
//! everyday case: a `{ … }` lambda converts to either SAM, and kotlinc selects the non-generic
//! one instead of reporting an ambiguity. Measured against kotlinc 2.4.10.

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

const ASSERTIONS_JAVA: &str = "public class Assertions {\n\
    public interface Executable { void execute() throws Throwable; }\n\
    public interface ThrowingSupplier<T> { T get() throws Throwable; }\n\
    public static String assertDoesNotThrow(Executable executable) {\n\
        try { executable.execute(); } catch (Throwable t) { return \"THREW\"; }\n\
        return \"plain\";\n\
    }\n\
    public static <T> T assertDoesNotThrow(ThrowingSupplier<T> supplier) {\n\
        try { return supplier.get(); } catch (Throwable t) { return null; }\n\
    }\n\
}\n";

#[test]
fn a_lambda_selects_the_non_generic_sam_overload() {
    let use_src = "import Assertions.assertDoesNotThrow\n\
object V { fun validate(x: Int) { require(x > 0) } }\n\
fun box(): String {\n\
    val imported = assertDoesNotThrow {\n\
        V.validate(1)\n\
    }\n\
    val qualified = Assertions.assertDoesNotThrow { V.validate(2) }\n\
    return if (imported == \"plain\" && qualified == \"plain\") \"OK\" else \"FAIL: $imported $qualified\"\n\
}\n";
    assert_eq!(
        java_box_with_stdlib(
            "non-generic-sam-overload",
            &[("Assertions.java", ASSERTIONS_JAVA)],
            use_src
        ),
        "OK"
    );
}
