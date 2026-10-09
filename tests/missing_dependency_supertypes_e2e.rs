//! A classpath classifier whose supertype the classpath lacks, compared with kotlinc: where
//! `MISSING_DEPENDENCY_SUPERCLASS` is reported, when a constructor call is only the eager warning,
//! and how `-XXLanguage:+AllowEagerSupertypeAccessibilityChecks` makes that warning an error.
//!
//! krusty builds the library chain itself: `a` alone, then `b` against `a`. Each consumer is then
//! compiled by both compilers against `b` only, so every supertype `b` takes from `a` is missing.

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
/// `complete`), under `arguments`; both must exit alike, report the same diagnostics at the same
/// file, line and column in the same order, and write the same output tree byte for byte.
fn assert_like_kotlinc(source: &str, complete: bool, arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let a = krusty_library(&work, "a", A, None);
    let b = krusty_library(&work, "b", B, Some(&a));
    let classpath = if complete {
        std::env::join_paths([&b, &a]).expect("join the classpath")
    } else {
        b.clone().into_os_string()
    };
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
