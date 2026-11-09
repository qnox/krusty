//! A classpath classifier whose supertype the classpath lacks, compared with kotlinc: where
//! `MISSING_DEPENDENCY_SUPERCLASS` is reported, when a constructor call is only the eager warning,
//! and how `-XXLanguage:+AllowEagerSupertypeAccessibilityChecks` makes that warning an error.
//!
//! krusty builds the library chain itself: `a` alone, then `b` against `a`. Each consumer is then
//! compiled by both compilers against `b` only, so every supertype `b` takes from `a` is missing.
//! A compilation without a JDK is compared too: every JDK supertype is missing there.

use std::path::{Path, PathBuf};

use super::common;

const A: &str = "\
package a
open class A { fun fromA() = 1 }
interface I
";

const B: &str = "\
package b
open class B : a.A(), a.I { fun fromB() = 2 }
open class B2 : a.A() { val p = 3; companion object { fun s() = 4 } }
fun B2.ext() = 5
class Plain { fun m(b: B2) = 6 }
open class Gen<T>
open class B3 : a.A() {
    var v = 1
    operator fun plus(o: Int) = this
    operator fun get(i: Int) = i
    operator fun invoke() = 1
    operator fun component1() = 1
    operator fun iterator() = listOf(1).iterator()
    fun memberFun() = 1
}
fun mk(): B3 = B3()
";

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Compile `source` into `output` with krusty, against `classpath`; it must succeed.
fn krusty_library(work: &Path, name: &str, source: &str, classpath: Option<&Path>) -> PathBuf {
    let file = work.join(format!("{name}.kt"));
    std::fs::write(&file, source).expect("write a library source");
    let output = work.join(name);
    let mut command = std::process::Command::new(common::krusty_binary());
    command.arg(text(&file)).arg("-d").arg(text(&output));
    if let Some(classpath) = classpath {
        command.arg("-cp").arg(text(classpath));
    }
    let result = command.output().expect("run krusty");
    assert!(
        result.status.success(),
        "krusty builds library {name}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

/// Compile `source` with both compilers against the library `b` alone (and `a` too when
/// `complete`), under `arguments`; see [`assert_like_kotlinc_on`].
fn assert_like_kotlinc(source: &str, complete: bool, arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let a = krusty_library(&work, "a", A, None);
    let b = krusty_library(&work, "b", B, Some(&a));
    let classpath = if complete {
        std::env::join_paths([&b, &a]).expect("join the classpath")
    } else {
        b.clone().into_os_string()
    };
    assert_like_kotlinc_on(&work, source, &classpath, arguments);
}

/// Compile `source` with both compilers against `classpath` under `arguments`; both must exit
/// alike, report the same diagnostics at the same file, line and column in the same order, and
/// write the same output tree byte for byte.
fn assert_like_kotlinc_on(
    work: &Path,
    source: &str,
    classpath: &std::ffi::OsStr,
    arguments: &[&str],
) {
    let consumer = work.join("Use.kt");
    std::fs::write(&consumer, source).expect("write the consumer");
    let command_line = |output: &str| {
        let mut command_line = vec![
            text(&consumer),
            "-cp".into(),
            classpath.to_string_lossy().into_owned(),
            "-d".into(),
            text(&work.join(output)),
        ];
        command_line.extend(arguments.iter().map(|argument| argument.to_string()));
        command_line
    };
    let (kotlinc_code, kotlinc_stderr) = common::kotlinc_compile(&command_line("kotlinc"))
        .expect("the reference kotlinc is available");
    let krusty = std::process::Command::new(common::krusty_binary())
        .args(command_line("krusty"))
        .output()
        .expect("run krusty");
    let krusty_stderr = String::from_utf8_lossy(&krusty.stderr);
    let reported = |stderr: &str| {
        (
            common::compiler_errors(stderr),
            common::compiler_warnings(stderr),
        )
    };
    assert_eq!(
        (krusty.status.code(), reported(&krusty_stderr)),
        (Some(kotlinc_code), reported(&kotlinc_stderr)),
        "{arguments:?}\nkrusty: {krusty_stderr}\nkotlinc: {kotlinc_stderr}"
    );
    assert_eq!(
        common::output_tree(&work.join("krusty")),
        common::output_tree(&work.join("kotlinc")),
        "{arguments:?}"
    );
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn a_declaration_inheriting_a_missing_supertype_is_reported() {
    assert_like_kotlinc(
        "\
package s
import b.*
@Suppress(\"x\") open class C1 : B2()
/** doc */ abstract class C2<T> : B2() where T : Any
object O : B2()
class Outer { inner class In : B2(); companion object : B2() }
class D(val x: Int) : B()
class O2 { @Suppress(\"x\") companion object : B2() }
class O3 { companion object Named : B2() }
typealias TA = B2
class E : TA()
fun <T : B2> bound(t: T) = t
fun <T> where(t: T) where T : B3 = t
class K<@Suppress(\"x\") T : B3>
fun local() { class L : B2() }
val anonymous = object : B2() {}
",
        false,
        &[],
    );
}

#[test]
fn an_access_through_a_classifier_missing_a_supertype_is_reported() {
    assert_like_kotlinc(
        "\
package v
import b.*
fun call(x: B) = x.fromB()
fun read(x: B2) = x.p
fun extension(x: B2) = x.ext()
fun unrelated(p: Plain, x: B2) = p.m(x)
fun companion() = B2.s()
fun reference() = B2::p
class G : Gen<B2>()
interface J : Comparable<B2>
fun unresolved(x: B2) = x.nope()
fun operators(x: B3?) {
    x!!.v = 2
    x?.v
    x + 1
    x[0]
    x()
    val (c) = x
    for (i in x) {}
    val r = x::memberFun
    mk()
}
",
        false,
        &[],
    );
}

#[test]
fn a_constructor_call_alone_is_the_eager_warning() {
    let source = "package w\nfun make() = b.B2()\nfun lam() = { x: b.B2 -> x }\n";
    assert_like_kotlinc(source, false, &[]);
    assert_like_kotlinc(
        source,
        false,
        &["-XXLanguage:+AllowEagerSupertypeAccessibilityChecks"],
    );
}

#[test]
fn a_complete_classpath_reports_nothing() {
    assert_like_kotlinc(
        "\
package c
import b.*
class C : B()
fun use(x: B) = x.fromB() + x.fromA()
fun make() = B2().ext()
fun ops(x: B3) = x[0] + x() + x.memberFun()
",
        true,
        &[],
    );
}

#[test]
fn no_jdk_reports_missing_platform_supertypes() {
    assert_like_kotlinc(
        "package n\nfun length(value: String) = value.length\n",
        true,
        &["-no-jdk"],
    );
}

/// Without a JDK every JDK supertype is missing, including the `java.io.Serializable` kotlinc gives
/// arrays and the mapped builtins whatever the classpath holds. Each operator convention on a
/// primitive is a member access of that primitive.
#[test]
fn without_a_jdk_its_supertypes_are_missing() {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let stdlib = common::stdlib_jar();
    assert_like_kotlinc_on(
        &work,
        "\
package n
fun read(s: String, a: Array<Int>, p: Pair<Int, Int>, r: Result<Int>) = s.length + a.size + p.first
fun success(r: Result<Int>) = r.isSuccess
fun template(i: Int, s: String) = \"v=$i $s\" + s
fun equality(s: String, i: Int) = s == \"x\" && i == 3
fun members(i: Int, e: Enum<*>, n: Number, t: Throwable) = i.toLong() + e.name.length + n.toInt() + t.hashCode()
class C : Throwable()
fun extension(s: String) = s.uppercase()
fun index(s: String) = s[0]
fun compare(i: Int, j: Int, a: Long, b: Long) = i < j || a >= b
fun unary(i: Int, b: Boolean) = if (!!b) -i else +i
fun increment(i: Int): Int {
    var j = i
    j++
    --j
    j += 1
    return ++j
}
fun ranges(n: Int, x: Int, l: List<Int>) {
    for (i in 0..n) {}
    for (i in 0 until n) {}
    val r = 0..<n
    val c = 'a'..'z'
    val u = 1 until 3
    if (x in 1..3 || x !in l) {}
}
fun collections(l: List<String>, m: Map<String, Int>) = l.size + m.entries.first().value
fun def(s: String, flag: Boolean = false) = 0
fun named(r: IntRange) {
    val a = def(\"x\", flag = true) < 1
    val b = 1 + def(s = \"x\")
    val c = (def(s = \"x\")) + 1
    val d = def(s = \"x\")..3
    val e = def(s = \"x\") !in r
    val f = def(s = \"x\") until 3
    for (i in def(s = \"x\")..3) {}
}
fun nested() = 1 + run { var k = 0; k += 1; k } + (fun(): Int { val z = 2; return z })()
",
        stdlib.as_os_str(),
        &["-no-jdk", "-no-stdlib", "-no-reflect"],
    );
}

const SAM_A: &str = "\
package a
interface A { fun fromA() = 1 }
interface B
";

const SAM_B: &str = "\
package b
fun interface Sam : a.A { fun run() }
fun interface Both : a.A, a.B { fun run() }
fun take(s: Sam) = 1
";

const SAM_USE: &str = "\
fun explicit() = b.Sam { }
fun typed(): b.Sam = { }
fun arg() = b.take { }
fun both() = b.Both { }
fun ok() = Local { }
fun interface Local { fun run() }
";

/// 2.5.0-Beta2 warns when a SAM constructor or conversion targets an interface whose supertype
/// is missing, and `ForbidSamConstructorCallsWithMissingDependencySupertype` (on at language 2.6,
/// under `-progressive`, or by name) makes that an error. 2.4.21 has no such check.
#[test]
fn a_sam_constructor_missing_a_dependency_supertype_follows_beta2() {
    let beta2 = "2.5.0-Beta2";
    let stdlib = common::kotlinc_lib_for_release(beta2)
        .expect("kotlinc 2.5.0-Beta2")
        .join("kotlin-stdlib.jar");
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let a = release_library(beta2, &work, "sam-a", SAM_A, None);
    let b = release_library(beta2, &work, "sam-b", SAM_B, Some(&a));
    for arguments in [
        &["-language-version", "2.5"][..],
        &[
            "-language-version",
            "2.5",
            "-XXLanguage:+ForbidSamConstructorCallsWithMissingDependencySupertype",
        ][..],
        &["-language-version", "2.6"][..],
    ] {
        assert_like_release(&work, beta2, &b, Some(&stdlib), arguments);
    }
    // `-progressive` turns this modeled feature on, and it also turns on features no phase
    // implements. krusty refuses that command line, so the row checks the refusal. Language 2.6
    // above is where the feature's own state is the error kotlinc reports.
    assert_progressive_refuses(&work, beta2, &b, &stdlib);
    let _ = std::fs::remove_dir_all(&work);

    // 2.4.21 reports nothing for the same program.
    let release = "2.4.21";
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let a = release_library(release, &work, "sam-a", SAM_A, None);
    let b = release_library(release, &work, "sam-b", SAM_B, Some(&a));
    assert_like_release(&work, release, &b, None, &[]);
    let _ = std::fs::remove_dir_all(&work);
}

fn release_library(
    release: &str,
    work: &Path,
    name: &str,
    source: &str,
    classpath: Option<&Path>,
) -> PathBuf {
    let file = work.join(format!("{name}.kt"));
    std::fs::write(&file, source).expect("write a library source");
    let output = work.join(name);
    let mut args = vec![text(&file), "-d".into(), text(&output), "-nowarn".into()];
    if let Some(classpath) = classpath {
        args.push("-cp".into());
        args.push(text(classpath));
    }
    let (code, stderr) = compile_release(release, args);
    assert_eq!(code, 0, "kotlinc {release} builds library {name}: {stderr}");
    output
}

/// `-progressive` on this release selects a feature krusty does not model, so the command is
/// refused before the program's diagnostics. The refused name is the first such feature in the
/// release table, not a recorded exception.
fn assert_progressive_refuses(work: &Path, release: &str, library: &Path, stdlib: &Path) {
    use krusty::features::LangFeatures;
    use krusty::kotlin_version::KotlinVersion;
    use krusty::language_version::LanguageVersion;

    let version = KotlinVersion::parse(release).expect("a supported release");
    let language = LanguageVersion::default_for(version);
    let before = LangFeatures::for_release(version, language, language);
    let feature = before
        .table()
        .progressive()
        .find(|feature| !before.has(&feature.name) && !LangFeatures::models(&feature.name))
        .expect("progressive selects a feature krusty does not model");
    let expected = format!(
        "krusty does not implement the language feature '{}' selected by '-progressive'",
        feature.name
    );
    let consumer = work.join("SamUse.kt");
    std::fs::write(&consumer, SAM_USE).expect("write the consumer");
    let classpath = std::env::join_paths([stdlib, library]).expect("join the classpath");
    let args = [
        format!("-Xkotlin-reference-version={release}"),
        text(&consumer),
        "-no-stdlib".into(),
        "-cp".into(),
        classpath.to_string_lossy().into_owned(),
        "-d".into(),
        text(&work.join("krusty-progressive")),
        "-language-version".into(),
        language.to_string(),
        "-progressive".into(),
    ];
    let result = std::process::Command::new(common::krusty_binary())
        .args(args)
        .output()
        .expect("run krusty");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_ne!(result.status.code(), Some(0), "{stderr}");
    assert_eq!(stderr.trim(), format!("krusty: error: {expected}"));
}

fn compile_release(release: &str, args: Vec<String>) -> (i32, String) {
    common::kotlinc_compile_for_release(release, &args)
        .unwrap_or_else(|| panic!("kotlinc {release} is not available"))
}

/// Compile the consumer with `release`'s kotlinc and with krusty pinned to that release.
/// `stdlib` puts that distribution's standard library on both classpaths (`-no-stdlib`); without
/// it each compiler keeps its own.
fn assert_like_release(
    work: &Path,
    release: &str,
    library: &Path,
    stdlib: Option<&Path>,
    arguments: &[&str],
) {
    let consumer = work.join("SamUse.kt");
    std::fs::write(&consumer, SAM_USE).expect("write the consumer");
    let classpath = match stdlib {
        Some(stdlib) => std::env::join_paths([stdlib, library]).expect("join the classpath"),
        None => library.as_os_str().to_owned(),
    };
    let compile = |output: &Path, reference: bool| -> (i32, String) {
        let mut args = vec![
            text(&consumer),
            "-cp".into(),
            classpath.to_string_lossy().into_owned(),
        ];
        if stdlib.is_some() {
            args.insert(1, "-no-stdlib".into());
        }
        args.push("-d".into());
        args.push(text(output));
        args.extend(arguments.iter().map(|argument| argument.to_string()));
        if reference {
            args.insert(0, format!("-Xkotlin-reference-version={release}"));
            let result = std::process::Command::new(common::krusty_binary())
                .args(&args)
                .output()
                .expect("run krusty");
            (
                result.status.code().unwrap_or(1),
                String::from_utf8_lossy(&result.stderr).into_owned(),
            )
        } else {
            compile_release(release, args)
        }
    };
    let label = if arguments.is_empty() {
        "default".to_string()
    } else {
        arguments.join("-")
    };
    let (kotlinc_code, kotlinc_stderr) = compile(&work.join(format!("kotlinc-{label}")), false);
    let (krusty_code, krusty_stderr) = compile(&work.join(format!("krusty-{label}")), true);
    let located = |stderr: &str| {
        (
            common::compiler_errors(stderr),
            common::compiler_warnings(stderr),
        )
    };
    assert_eq!(
        (krusty_code, located(&krusty_stderr)),
        (kotlinc_code, located(&kotlinc_stderr)),
        "{release} {arguments:?}\nkrusty: {krusty_stderr}\nkotlinc: {kotlinc_stderr}"
    );
}
