//! kotlinc's deprecation checks for declarations read from the classpath: a Kotlin dependency's
//! `@Deprecated` declarations, a Java declaration deprecated by the `Deprecated` attribute or
//! `@java.lang.Deprecated` (`Deprecated in Java.`), and a standard-library declaration whose level
//! `@DeprecatedSinceKotlin` derives from the API version.

use super::common;
use std::path::PathBuf;

const KOTLIN_DEPENDENCY: &str = r#"package lib

@Deprecated("Use g instead")
fun f(x: Int): Int = x

@Deprecated("err", level = DeprecationLevel.ERROR)
fun fe(): Int = 1

@Deprecated("hidden", level = DeprecationLevel.HIDDEN)
fun fh(): Int = 1
fun fh(x: Int): Int = x

@Deprecated("top prop")
var counter = 0

@Deprecated("class")
open class Old(val v: Int) {
    class Nested
    fun member() {}
}

@Deprecated("iface")
interface OldApi

@Deprecated("obj")
object Registry { fun lookup() = 1 }

class Holder {
    @Deprecated("member fun") fun m() {}
    @Deprecated("member val") val p: Int = 1
    var s: Int = 0
        @Deprecated("setter") set
    val g: Int
        @Deprecated("getter") get() = 2
    @Deprecated("property fallback")
    var accessorWins: Int = 0
        @Deprecated("getter wins", level = DeprecationLevel.ERROR) get
        @Deprecated("setter wins", level = DeprecationLevel.ERROR) set
    @Deprecated("hidden member", level = DeprecationLevel.HIDDEN) fun hm() {}
}

@java.lang.Deprecated
class ExplicitJavaDeprecated {
    @java.lang.Deprecated fun old() {}
}

class Ctor @Deprecated("primary") constructor(val a: Int)

@Deprecated("ext")
fun <T : Comparable<T>> List<T>.ext(vararg ys: String, z: Int = 0): T? = null

@Deprecated("err class", level = DeprecationLevel.ERROR)
class EC

open class MemberExtensionOwner {
    @Deprecated("member ext") fun String.oldExt(): Int = length
}
"#;

const JAVA_OLD: &str = r#"package j;
public class JOld {
    @Deprecated public static void old() {}
    @Deprecated public int field = 1;
    /** @deprecated use other */
    public void doc() {}
    @Deprecated public JOld() {}
    public JOld(int x) {}
    @Deprecated public static class Nested {}
    public void fine() {}
}
"#;

const JAVA_DEPRECATED_CLASS: &str = r#"package j;
@Deprecated
public class JDep { public static int get() { return 1; } }
"#;

fn kotlin_dependency() -> PathBuf {
    common::kotlinc_library(KOTLIN_DEPENDENCY).expect("reference kotlinc builds the dependency")
}

fn java_dependency() -> PathBuf {
    common::javac_compile(
        &[
            ("JOld.java".to_string(), JAVA_OLD.to_string()),
            ("JDep.java".to_string(), JAVA_DEPRECATED_CLASS.to_string()),
        ],
        &[],
    )
    .expect("javac builds the Java dependency")
    .0
}

/// Compile `source` against `classpath` with both compilers and require the same complete errors
/// and warnings, which must also be `errors` and `warnings` (`line:column: message`, in emission
/// order).
fn assert_matches_kotlinc(
    source: &str,
    classpath: &[PathBuf],
    args: &[&str],
    errors: &[&str],
    warnings: &[&str],
) {
    let mut entries = vec![common::stdlib_jar()];
    entries.extend_from_slice(classpath);
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(&[("Use.kt", source)], &entries, &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    let krusty_errors = render(&common::compiler_errors(&krusty_output));
    let kotlinc_errors = render(&common::compiler_errors(&result.reference_stderr));
    let krusty_warnings = render(&common::compiler_warnings(&krusty_output));
    let kotlinc_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(kotlinc_errors, errors, "kotlinc errors");
    assert_eq!(kotlinc_warnings, warnings, "kotlinc warnings");
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

/// A Kotlin dependency's `WARNING` declarations report at the same positions as source ones.
#[test]
fn kotlin_dependency_uses_warn_at_the_used_name() {
    assert_matches_kotlinc(
        r#"import lib.*

class Sub : Old(1), OldApi

fun use(h: Holder, o: Old, any: Any): List<Old> {
    f(1)
    counter = 1
    counter
    h.m()
    h.p
    h.s = 2
    h.s
    h.g
    val ref = ::f
    Old(1)
    Old.Nested()
    o.member()
    Registry.lookup()
    Ctor(1)
    listOf(1).ext()
    any is OldApi
    return listOf<Old>()
}
"#,
        &[kotlin_dependency()],
        &[],
        &[],
        &[
            "3:13: 'class Old : Any' is deprecated. class.",
            "3:21: 'interface OldApi : Any' is deprecated. iface.",
            "5:23: 'class Old : Any' is deprecated. class.",
            "5:44: 'class Old : Any' is deprecated. class.",
            "6:5: 'fun f(x: Int): Int' is deprecated. Use g instead.",
            "7:5: 'var counter: Int' is deprecated. top prop.",
            "8:5: 'var counter: Int' is deprecated. top prop.",
            "9:7: 'fun m(): Unit' is deprecated. member fun.",
            "10:7: 'val p: Int' is deprecated. member val.",
            "11:7: 'var s: Int' is deprecated. setter.",
            "13:7: 'val g: Int' is deprecated. getter.",
            "14:17: 'fun f(x: Int): Int' is deprecated. Use g instead.",
            "15:5: 'constructor(v: Int): Old' is deprecated. class.",
            "16:5: 'class Old : Any' is deprecated. class.",
            "18:5: 'object Registry : Any' is deprecated. obj.",
            "19:5: 'constructor(a: Int): Ctor' is deprecated. primary.",
            "20:15: 'fun <T : Comparable<T>> List<T>.ext(vararg ys: String, z: Int = ...): T?' is deprecated. ext.",
            "21:12: 'interface OldApi : Any' is deprecated. iface.",
            "22:19: 'class Old : Any' is deprecated. class.",
        ],
    );
}

/// `ERROR` reports an error; a `HIDDEN` dependency callable is no candidate.
#[test]
fn kotlin_dependency_error_and_hidden_levels() {
    assert_matches_kotlinc(
        r#"import lib.*

fun use(h: Holder) {
    fe()
    fh()
    EC()
    val e: EC? = null
    h.hm()
}
"#,
        &[kotlin_dependency()],
        &[],
        &[
            "4:5: 'fun fe(): Int' is deprecated. err.",
            "5:5: no value passed for parameter 'x'.",
            "6:5: 'constructor(): EC' is deprecated. err class.",
            "7:12: 'class EC : Any' is deprecated. err class.",
            "8:7: unresolved reference 'hm'.",
        ],
        &[],
    );
}

/// A selected accessor's own deprecation takes precedence over the property declaration's fact.
#[test]
fn kotlin_dependency_accessor_deprecation_takes_precedence() {
    assert_matches_kotlinc(
        r#"import lib.Holder

fun use(h: Holder) {
    h.accessorWins
    h.accessorWins = 1
}
"#,
        &[kotlin_dependency()],
        &[],
        &[
            "4:7: 'var accessorWins: Int' is deprecated. getter wins.",
            "5:7: 'var accessorWins: Int' is deprecated. setter wins.",
        ],
        &[],
    );
}

/// An explicit Java deprecation annotation remains semantic on a Kotlin-authored binary, but the
/// declaration's Kotlin origin suppresses Java's `Deprecated in Java` sentence. Only an actual Java
/// declaration receives that message; the synthetic classfile attribute beside a Kotlin
/// deprecation is also ignored.
#[test]
fn explicit_java_deprecation_on_kotlin_binary_warns() {
    assert_matches_kotlinc(
        r#"import lib.ExplicitJavaDeprecated

fun use(value: ExplicitJavaDeprecated) {
    ExplicitJavaDeprecated()
    value.old()
}
"#,
        &[kotlin_dependency()],
        &[],
        &[],
        &[
            "3:16: 'class ExplicitJavaDeprecated : Any' is deprecated.",
            "4:5: 'constructor(): ExplicitJavaDeprecated' is deprecated.",
            "5:11: 'fun old(): Unit' is deprecated.",
        ],
    );
}

/// A dependency member extension keeps its provider-published deprecation on the committed call;
/// the checker must not reopen the classpath from its owner and physical method name.
#[test]
fn kotlin_dependency_member_extension_warns() {
    assert_matches_kotlinc(
        r#"import lib.MemberExtensionOwner

class User : MemberExtensionOwner() {
    fun use(): Int = "x".oldExt()
}
"#,
        &[kotlin_dependency()],
        &[],
        &[],
        &["4:26: 'fun String.oldExt(): Int' is deprecated. member ext."],
    );
}

/// A Java declaration deprecated by its `Deprecated` attribute (`@Deprecated` or the Javadoc
/// `@deprecated` tag) warns with the message `Deprecated in Java`.
#[test]
fn java_deprecation_is_deprecated_in_java() {
    assert_matches_kotlinc(
        r#"import j.JOld
import j.JDep

fun use(jo: JOld) {
    JOld.old()
    jo.field
    jo.doc()
    JOld()
    JOld(1)
    JOld.Nested()
    JDep.get()
    val x: JDep? = null
    jo.fine()
}
"#,
        &[java_dependency()],
        &[],
        &[],
        &[
            "5:10: 'static fun old(): Unit' is deprecated. Deprecated in Java.",
            "6:8: 'field field: Int' is deprecated. Deprecated in Java.",
            "7:8: 'fun doc(): Unit' is deprecated. Deprecated in Java.",
            "8:5: 'constructor(): JOld' is deprecated. Deprecated in Java.",
            "10:10: 'constructor(): JOld.Nested' is deprecated. Deprecated in Java.",
            "11:5: 'class JDep : Any' is deprecated. Deprecated in Java.",
            "12:12: 'class JDep : Any' is deprecated. Deprecated in Java.",
        ],
    );
}

/// A standard-library declaration whose `@DeprecatedSinceKotlin` warning version the API version
/// reached warns.
#[test]
fn deprecated_since_kotlin_warnings() {
    assert_matches_kotlinc(
        r#"fun use() {
    "a".capitalize()
    listOf(1).sumBy { it }
}
"#,
        &[],
        &[],
        &[],
        &[
            "2:9: 'fun String.capitalize(): String' is deprecated. Use replaceFirstChar instead.",
            "3:15: 'fun <T> Iterable<T>.sumBy(selector: (T) -> Int): Int' is deprecated. Use sumOf instead.",
        ],
    );
}

/// One whose error version the API version reached is an error.
#[test]
fn deprecated_since_kotlin_errors() {
    assert_matches_kotlinc(
        r#"fun use() {
    "a".toUpperCase()
    "a".toLowerCase()
}
"#,
        &[],
        &[],
        &[
            "2:9: 'fun String.toUpperCase(): String' is deprecated. Use uppercase() instead.",
            "3:9: 'fun String.toLowerCase(): String' is deprecated. Use lowercase() instead.",
        ],
        &[],
    );
}

/// The level follows `-api-version`: below the error version, the same uses only warn.
#[test]
fn deprecated_since_kotlin_follows_the_api_version() {
    assert_matches_kotlinc(
        r#"fun use() {
    "a".toUpperCase()
    "a".toLowerCase()
}
"#,
        &[],
        &["-api-version", "2.0", "-language-version", "2.0"],
        &[],
        &[
            "2:9: 'fun String.toUpperCase(): String' is deprecated. Use uppercase() instead.",
            "3:9: 'fun String.toLowerCase(): String' is deprecated. Use lowercase() instead.",
        ],
    );
}
