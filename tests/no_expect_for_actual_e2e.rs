//! An `actual` declaration with no `expect` to actualize.
//!
//! `actual` promises that some `expect` header exists to be filled; with nothing to fill it, the
//! modifier is meaningless and the reference compiler rejects the declaration. krusty accepted it
//! and emitted.
//!
//! The message names the declaration the way the reference compiler's own declaration renderer
//! does, which is the whole substance of the feature — so these tests are DIFFERENTIAL. They put
//! the same source through kotlinc and through krusty and compare the two reports, rather than
//! against a transcription of one: a transcription is exactly what a rendering this detailed gets
//! wrong (measuring one shape by hand already produced `vararged(vararg xs: Int): Int` for a
//! function returning `Unit`).

use super::common;

/// The measured shape of one report line, so the comparison is about the RENDERING and not about
/// which absolute path each compiler happened to print.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Reported {
    line: u32,
    column: u32,
    rendered: String,
}

const SENTENCE: &str = "has no corresponding expected declaration";

fn reported(report: &str, stem: &str) -> Vec<Reported> {
    let mut found = report
        .lines()
        .filter(|line| line.contains(SENTENCE))
        .filter_map(|line| {
            let (position, message) = line.split_once(": error: ")?;
            let mut position = position.rsplit(':');
            let column = position.next()?.parse().ok()?;
            let line = position.next()?.parse().ok()?;
            // Both compilers print a path; only the file it ends in has to agree.
            let path = position.next()?;
            assert!(
                path.ends_with(&format!("{stem}.kt")),
                "an unexpected file reported: {path}"
            );
            Some(Reported {
                line,
                column,
                rendered: message.trim_end().to_string(),
            })
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// Compile `source` with the reference compiler and with krusty, and return both reports.
fn both(source: &str, stem: &str) -> (Vec<Reported>, Vec<Reported>) {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join(format!("{stem}.kt"));
    std::fs::write(&file, source).expect("write the fixture");

    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (_, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        file.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");

    let out = std::process::Command::new(common::krusty_binary())
        .args([
            "-XXLanguage:+MultiPlatformProjects",
            "-no-stdlib",
            "-no-jdk",
            "-cp",
        ])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(dir.join("krusty"))
        .arg(&file)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));

    (reported(&reference, stem), reported(&krusty, stem))
}

/// Assert that krusty's report is exactly the reference compiler's.
fn assert_identical(source: &str, stem: &str) {
    let (reference, krusty) = both(source, stem);
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
    assert_eq!(
        krusty, reference,
        "krusty's report must be the reference compiler's, line, column and rendering"
    );
}

/// Callables: the visibility and modality slots, `suspend`/`inline`, type parameters with and
/// without a declared bound, an extension receiver, `vararg`, a default (rendered as the literal
/// `...`), a nullable type, a function type, and an INFERRED return — the one a rendering from
/// syntax alone cannot produce.
#[test]
fn a_callable_is_rendered_as_the_reference_compiler_renders_it() {
    assert_identical(
        "package plib\n\
         \n\
         actual fun simple(): Int = 1\n\
         actual fun inferred() = 1\n\
         actual fun unitRet() { }\n\
         actual fun nullableRet(): String? = null\n\
         actual fun vararged(vararg xs: Int) { }\n\
         actual fun defaulted(a: Int = 1) { }\n\
         actual fun <T> generic(t: T): T = t\n\
         actual fun <T : Comparable<T>> bounded(t: T): T = t\n\
         actual fun Int.receiver(): Int = this\n\
         actual fun functionParam(f: (Int) -> String): Int = 1\n\
         actual suspend fun susp(): Int = 1\n\
         actual inline fun inl(): Int = 1\n\
         internal actual fun internalFun(): Int = 1\n\
         private actual fun privateFun(): Int = 1\n\
         actual fun manyParams(a: Int, b: String?, c: List<Int>, vararg rest: Long): Int = 1\n",
        "Callables",
    );
}

/// The modifier words between `actual` and `fun`, and their measured ORDER — `external` before
/// `override`, `inline` before `operator`, `infix` and `suspend`. Each was rendered wrong first:
/// the modifiers were simply missing, and a source with `actual operator fun` read as though it
/// had not been written.
#[test]
fn a_callable_renders_every_modifier_it_wrote_in_order() {
    assert_identical(
        "package plib\n\
         \n\
         actual external fun ext(): Int\n\
         actual operator fun Int.unaryMinus(): Int = 1\n\
         actual infix fun Int.to2(other: Int): Int = other\n\
         actual tailrec fun loop(n: Int): Int = if (n == 0) 0 else loop(n - 1)\n\
         actual inline infix fun Int.both(other: Int): Int = other\n\
         actual inline suspend fun sus(f: () -> Int): Int = f()\n\
         actual inline operator fun Int.times2(other: Int): Int = other\n",
        "Modifiers",
    );
}

/// Properties: `val` and `var`, a function type, and an extension property whose type parameters
/// precede its receiver.
#[test]
fn a_property_is_rendered_as_the_reference_compiler_renders_it() {
    assert_identical(
        "package plib\n\
         \n\
         actual val prop: Int = 2\n\
         actual var mutable: String = \"\"\n\
         actual val lambdaProp: (Int) -> Unit = {}\n\
         actual val <T> List<T>.ext: Int get() = 1\n\
         internal actual val internalProp: Int = 3\n\
         actual const val constant: Int = 4\n\
         actual lateinit var late: String\n",
        "Properties",
    );
}

/// Classifiers: every kind and modality, the modifier words between `actual` and the kind keyword,
/// and the supertype the reference compiler always prints — including the one Kotlin supplies
/// (`Any`, `Enum<…>`, `Annotation`) and a supertype reached through a typealias.
#[test]
fn a_classifier_is_rendered_as_the_reference_compiler_renders_it() {
    assert_identical(
        "package plib\n\
         \n\
         interface I\n\
         interface J\n\
         interface Consumer<T>\n\
         open class Base\n\
         open class GBase<T>\n\
         typealias AliasBase = Base\n\
         \n\
         actual class Cls\n\
         actual class Generic<T>\n\
         actual class BoundedParams<T : Comparable<T>, U>\n\
         actual object Obj\n\
         actual interface Iface\n\
         actual sealed interface SealedIface\n\
         actual fun interface FunIface { fun call(): Int }\n\
         actual open class Op\n\
         actual abstract class Ab\n\
         actual sealed class Se\n\
         actual data class Data(val x: Int)\n\
         actual value class Wrapped(val x: Int)\n\
         actual enum class Colors { A }\n\
         actual annotation class Anno\n\
         actual class OnlyIface : I\n\
         actual class TwoIfaces : I, J\n\
         actual class BaseAndIface : Base(), I\n\
         actual class GenericBase : GBase<String>()\n\
         actual class ViaAlias : AliasBase()\n\
         actual object ObjWithSuper : Base(), I\n",
        "Classifiers",
    );
}

/// A `typealias` is how an `expect class` is actualized, so it carries `actual` and renders its
/// own way — with the alias's own type parameters and its resolved target, including a function
/// type.
#[test]
fn a_type_alias_is_rendered_as_the_reference_compiler_renders_it() {
    assert_identical(
        "package plib\n\
         \n\
         actual typealias Alias = String\n\
         actual typealias GenericAlias<T> = List<T>\n\
         actual typealias FunAlias = (Int) -> String\n",
        "Aliases",
    );
}

/// A MATCHED `actual` is silent, and so is every ordinary declaration beside it — the check must
/// not cost a correct multiplatform module a diagnostic.
///
/// This is the one case that cannot be differential, and the reason is a deliberate model
/// difference rather than a defect: the reference compiler rejects an `expect` and its `actual` in
/// the SAME module (`expect and corresponding actual are declared in the same module`), so it
/// reports all four of these as unmatched. krusty compiles a platform module and its `dependsOn`
/// chain as ONE source set — see the multiplatform entry in `docs/SPEC.md` — where a pair in one
/// file is exactly how a matched pair looks. Only krusty's side is asserted here.
#[test]
fn a_matched_actual_is_silent() {
    let (_, krusty) = both(
        "package plib\n\
         \n\
         expect fun helper(): Int\n\
         actual fun helper(): Int = 1\n\
         \n\
         expect class Holder\n\
         actual class Holder\n\
         \n\
         expect val prop: Int\n\
         actual val prop: Int = 2\n\
         \n\
         expect class Aliased\n\
         actual typealias Aliased = String\n\
         \n\
         fun ordinary(): Int = helper()\n",
        "Matched",
    );
    assert!(
        krusty.is_empty(),
        "a fun, a class, a val and a typealias all actualize something: {krusty:?}"
    );
}

/// An `actual` whose types reach its `expect` only THROUGH an `actual typealias` is matched.
///
/// This is why the name/arity key cannot be the authority: `expect val S.tag: S` and
/// `actual val String.tag: String` key differently on their receiver, and only actualization's own
/// shape matcher — which follows the `actual typealias S = String` — pairs them. Reporting these
/// was a real regression caught by the harness, not a hypothetical.
#[test]
fn an_actual_matched_through_an_alias_is_silent() {
    let dir = common::scratch_dir().expect("scratch dir");
    let common_file = dir.join("Common.kt");
    let platform = dir.join("Platform.kt");
    std::fs::write(
        &common_file,
        "package plib
         
         expect class S
         expect fun f(value: S): S
         expect val S.tag: S
",
    )
    .unwrap();
    std::fs::write(
        &platform,
        "package plib
         
         actual fun f(value: String): String = value
         actual val String.tag: String get() = this
         actual typealias S = String
",
    )
    .unwrap();
    let out = std::process::Command::new(common::krusty_binary())
        .args([
            "-XXLanguage:+MultiPlatformProjects",
            "-no-stdlib",
            "-no-jdk",
            "-cp",
        ])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir)
        .arg(&common_file)
        .arg(&platform)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        !report.contains(SENTENCE),
        "an `actual` paired through an alias is silent:\n{report}"
    );
}

/// An `actual` in a file whose `expect` lives in ANOTHER file of the same source set is matched:
/// the question is about the source set, not about one file.
#[test]
fn the_expect_may_live_in_another_file() {
    let dir = common::scratch_dir().expect("scratch dir");
    let header = dir.join("Header.kt");
    let platform = dir.join("Platform.kt");
    std::fs::write(&header, "package plib\n\nexpect fun helper(): Int\n").unwrap();
    std::fs::write(&platform, "package plib\n\nactual fun helper(): Int = 1\n").unwrap();
    let out = std::process::Command::new(common::krusty_binary())
        .args([
            "-XXLanguage:+MultiPlatformProjects",
            "-no-stdlib",
            "-no-jdk",
            "-cp",
        ])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir)
        .arg(&header)
        .arg(&platform)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        !report.contains(SENTENCE),
        "an `actual` matched across files is silent:\n{report}"
    );
}

/// Without the multiplatform feature the modifier is rejected outright, and this check does not
/// pile a second sentence on top of that one.
#[test]
fn the_check_needs_the_multiplatform_feature() {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join("Main.kt");
    std::fs::write(&file, "package plib\n\nactual fun helper(): Int = 1\n").unwrap();
    let out = std::process::Command::new(common::krusty_binary())
        .args(["-no-stdlib", "-no-jdk", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir)
        .arg(&file)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        report.contains("can be used only in multiplatform projects"),
        "the feature gate still reports:\n{report}"
    );
    assert!(
        !report.contains(SENTENCE),
        "and nothing else is added on top of it:\n{report}"
    );
}

/// A MEMBER `actual` is the documented remainder: the reference compiler reports one and krusty
/// does not yet. Pinned so the gap is visible and its closing is a test change, not a surprise.
#[test]
fn a_member_actual_is_not_reported_yet() {
    let (reference, krusty) = both(
        "package plib\n\
         \n\
         actual class Holder {\n\
         \x20   actual fun member(): Int = 3\n\
         }\n",
        "Members",
    );
    assert_eq!(
        reference.len(),
        2,
        "the reference compiler reports the class AND its member: {reference:?}"
    );
    assert_eq!(
        krusty.len(),
        1,
        "krusty reports only the top-level classifier: {krusty:?}"
    );
    assert_eq!(
        krusty[0], reference[0],
        "and what it does report is rendered identically"
    );
}
