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
