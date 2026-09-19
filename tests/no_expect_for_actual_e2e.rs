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
#[derive(Debug, Eq, PartialEq)]
struct Reported {
    line: u32,
    column: u32,
    rendered: String,
}

const SENTENCE: &str = "has no corresponding expected declaration";

/// Every reported line, in the order the compiler printed it.
///
/// The sequence is NOT sorted. Diagnostic order is part of what this compares: both compilers
/// report these in source order, and a sort would hide a regression that reordered them — which is
/// exactly what happened while members were collected as all functions followed by all properties.
fn reported(report: &str, stem: &str) -> Vec<Reported> {
    report
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
        .collect::<Vec<_>>()
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

/// The MEMBERS of an unmatched `actual` classifier are reported too, each at its own name — a
/// member of a classifier that actualized nothing cannot itself have actualized anything.
///
/// A member's modality is the part that is not `final`: an `override` of an `open` member renders
/// `open`, an interface member renders `abstract` without a body and `open` with one. All measured.
#[test]
fn the_members_of_an_unmatched_actual_classifier_are_reported() {
    assert_identical(
        "package plib\n\
         \n\
         open class Base {\n\
         \x20   open fun inherited(): Int = 0\n\
         \x20   open suspend fun both(): Int = 0\n\
         }\n\
         \n\
         actual class Derived : Base() {\n\
         \x20   fun plain(): Int = 1\n\
         \x20   actual fun marked(): Int = 2\n\
         \x20   actual val markedProp: Int = 3\n\
         \x20   actual var settable: Int = 4\n\
         \x20   actual override fun inherited(): Int = 5\n\
         \x20   actual override suspend fun both(): Int = 6\n\
         \x20   actual operator fun plus(other: Int): Int = 7\n\
         \x20   actual lateinit var late: String\n\
         \x20   internal actual fun internalMember(): Int = 8\n\
         \x20   protected actual fun prot(): Int = 9\n\
         \x20   actual fun <T> generic(t: T): T = t\n\
         }\n\
         \n\
         actual abstract class Abs {\n\
         \x20   actual abstract fun abstractFun(): Int\n\
         }\n\
         \n\
         actual interface Iface {\n\
         \x20   actual val absProp: Int\n\
         \x20   actual fun withDefault(): Int = 1\n\
         }\n\
         \n\
         actual object Singleton {\n\
         \x20   actual fun objFun(): Int = 2\n\
         }\n",
        "MemberShapes",
    );
}

/// Every member shape that is a place the member walk has to GO rather than a rendering it gets
/// wrong: a member extension property, a nested `companion object` (anonymous and named), a member
/// of that companion, a nested class and object, and a CONSTRUCTOR property.
///
/// The receivers and names are fixture-owned (`Tally`, `Holder`, `memberExt`, `companionFun`): a
/// stdlib-shaped receiver such as `Int` risks the rendering passing through a builtin path rather
/// than through the declaration renderer under test, and a stdlib-shaped callable name risks
/// agreeing for a reason that has nothing to do with this check.
#[test]
fn every_nested_and_extension_member_shape_is_reported() {
    assert_identical(
        "package plib\n\
         \n\
         class Tally\n\
         \n\
         actual class Holder {\n\
         \x20   actual val Tally.memberExt: Int get() = 1\n\
         \x20   actual var Tally.settableExt: Int\n\
         \x20       get() = 2\n\
         \x20       set(value) {}\n\
         \x20   actual class Nested\n\
         \x20   actual object Solo\n\
         \x20   actual companion object {\n\
         \x20       actual fun companionFun(): Int = 3\n\
         \x20       actual val companionSlot: Int = 4\n\
         \x20   }\n\
         }\n\
         \n\
         actual class Named {\n\
         \x20   actual companion object Registry {\n\
         \x20       actual fun registered(): Int = 5\n\
         \x20   }\n\
         }\n\
         \n\
         actual class Carried(actual val kept: Int, actual var tally: Tally, plain: Int) {\n\
         \x20   val derived: Int = plain\n\
         }\n\
         \n\
         actual annotation class Anno(actual val x: Int)\n",
        "NestedShapes",
    );
}

/// A SECONDARY constructor and an enum-entry owner's member.
///
/// A constructor writes no name, so the reference compiler underlines the whole declaration — from
/// its first modifier through its delegation — and renders no modality slot, with the declaring
/// classifier standing in for the result type. Its `private` case is measured beside the default
/// one because the visibility slot is the only one it has, and a `vararg` and a default parameter
/// beside them because those are the two facts the resolved shape alone cannot tell.
#[test]
fn a_secondary_constructor_is_reported_at_its_whole_declaration() {
    assert_identical(
        "package plib\n\
         \n\
         class Tally\n\
         \n\
         actual class Owner {\n\
         \x20   actual constructor(one: Tally)\n\
         \x20   actual constructor(one: Tally, two: Int) : this(one)\n\
         \x20   private actual constructor(vararg tallies: Tally, spare: Int = 1) : this(Tally())\n\
         \x20   constructor(unmarked: Int) : this(Tally())\n\
         }\n\
         \n\
         actual enum class Palette {\n\
         \x20   RUBY;\n\
         \x20   actual fun shade(): Int = 1\n\
         }\n",
        "Constructors",
    );
}

/// Two member overloads that TIE on name and arity, differing only in their parameter types.
///
/// This is the shape a name/arity lookup cannot answer: it finds two candidates for one source
/// declaration and can only guess or drop both. Each member is selected by its own stable
/// declaration identity, so both render — with their own parameter types — and the comparison is
/// the complete ordered sequence rather than a count.
#[test]
fn overloads_that_tie_on_name_and_arity_each_render_their_own_types() {
    assert_identical(
        "package plib\n\
         \n\
         class Tally\n\
         class Ledger\n\
         \n\
         actual class Holder {\n\
         \x20   actual fun absorb(one: Tally): Int = 1\n\
         \x20   actual fun absorb(one: Ledger): Int = 2\n\
         \x20   actual fun absorb(one: Tally, two: Ledger): Int = 3\n\
         \x20   actual fun absorb(one: Ledger, two: Tally): Int = 4\n\
         }\n",
        "TiedOverloads",
    );
}

/// An unmarked member of an unmatched `actual` classifier stays silent: the diagnostic is about the
/// modifier, not about the classifier's contents.
#[test]
fn an_unmarked_member_is_silent_among_marked_ones() {
    let (reference, krusty) = both(
        "package plib\n\
         \n\
         actual class Owner(val plainParam: Int, actual val markedParam: Int) {\n\
         \x20   fun plain(): Int = 1\n\
         \x20   val plainProp: Int = 2\n\
         \x20   class PlainNested\n\
         \x20   companion object {\n\
         \x20       fun plainCompanionFun(): Int = 3\n\
         \x20   }\n\
         }\n",
        "UnmarkedAmong",
    );
    assert_eq!(
        reference
            .iter()
            .map(|entry| format!("{}:{}", entry.line, entry.column))
            .collect::<Vec<_>>(),
        ["3:14", "3:52"],
        "only the classifier and the one marked constructor property: {reference:?}"
    );
    assert_eq!(krusty, reference, "and krusty agrees, in order");
}

/// The file a member diagnostic belongs to is the file that DECLARES it.
///
/// A single-file fixture cannot catch a member inheriting whichever file the previous phase left
/// current, because there is only one file to inherit. Here the unmatched `actual` classifier is
/// the FIRST of three compiled files, with another file's declarations reported after it, so a
/// member that took the active file rather than its own would name the wrong source — and the
/// comparison is each report's complete ordered sequence of file, line, column and message.
#[test]
fn a_member_names_its_own_file_when_several_are_compiled() {
    let dir = common::scratch_dir().expect("scratch dir");
    let first = dir.join("First.kt");
    let middle = dir.join("Middle.kt");
    let last = dir.join("Last.kt");
    std::fs::write(
        &first,
        "package plib\n\
         \n\
         class Tally\n\
         \n\
         actual class Holder {\n\
         \x20   actual fun held(): Int = 1\n\
         \x20   actual val Tally.tallied: Int get() = 2\n\
         }\n",
    )
    .expect("write the first file");
    std::fs::write(&middle, "package plib\n\nfun ordinary(): Int = 3\n")
        .expect("write the middle file");
    std::fs::write(
        &last,
        "package plib\n\
         \n\
         actual class Trailing {\n\
         \x20   actual fun trailed(): Int = 4\n\
         }\n",
    )
    .expect("write the last file");

    let reference_out = dir.join("multi-reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (_, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        first.to_string_lossy().into_owned(),
        middle.to_string_lossy().into_owned(),
        last.to_string_lossy().into_owned(),
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
        .arg(dir.join("multi-krusty"))
        .arg(&first)
        .arg(&middle)
        .arg(&last)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));

    // Each report's own complete sequence, file included — the coordinate a single-file fixture
    // cannot state.
    let ledger = |report: &str| {
        report
            .lines()
            .filter(|line| line.contains(SENTENCE))
            .filter_map(|line| {
                let (position, message) = line.split_once(": error: ")?;
                let mut position = position.rsplit(':');
                let column = position.next()?;
                let number = position.next()?;
                let path = std::path::Path::new(position.next()?);
                let file = path.file_name()?.to_string_lossy().into_owned();
                Some(format!("{file}:{number}:{column}: {}", message.trim_end()))
            })
            .collect::<Vec<_>>()
    };
    let reference = ledger(&reference);
    assert_eq!(
        reference,
        [
            "First.kt:5:14: 'public final actual class Holder : Any' has no corresponding expected declaration",
            "First.kt:6:16: 'public final actual fun held(): Int' has no corresponding expected declaration",
            "First.kt:7:22: 'public final actual val Tally.tallied: Int' has no corresponding expected declaration",
            "Last.kt:3:14: 'public final actual class Trailing : Any' has no corresponding expected declaration",
            "Last.kt:4:16: 'public final actual fun trailed(): Int' has no corresponding expected declaration",
        ],
        "the reference compiler's whole ledger across the three files"
    );
    assert_eq!(ledger(&krusty), reference, "and krusty's is the same one");
}

/// A member that actualizes nothing under an owner that DID actualize.
///
/// The owner's outcome is not the member's: `kept` fills the `expect` member and is silent, while
/// `extra` beside it fills nothing and is reported. This needs a real `expect`/`actual` split,
/// because the reference compiler rejects an `expect` and its `actual` in the same module outright
/// and never reaches the question — so the header goes through `-Xcommon-sources`, which is how
/// kotlinc is told that one of the files it is compiling is the common fragment.
#[test]
fn a_member_that_actualizes_nothing_under_a_matched_owner_is_reported() {
    let dir = common::scratch_dir().expect("scratch dir");
    let header = dir.join("CommonHeader.kt");
    let platform = dir.join("PlatformBody.kt");
    std::fs::write(
        &header,
        "package plib\n\
         \n\
         expect class Holder {\n\
         \x20   fun kept(): Int\n\
         }\n",
    )
    .expect("write the common header");
    std::fs::write(
        &platform,
        "package plib\n\
         \n\
         actual class Holder {\n\
         \x20   actual fun kept(): Int = 1\n\
         \x20   actual fun extra(): Int = 2\n\
         \x20   actual val spare: Int = 3\n\
         }\n",
    )
    .expect("write the platform body");

    let reference_out = dir.join("split-reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (_, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        format!("-Xcommon-sources={}", header.to_string_lossy()),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        header.to_string_lossy().into_owned(),
        platform.to_string_lossy().into_owned(),
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
        .arg(dir.join("split-krusty"))
        .arg(&header)
        .arg(&platform)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));

    let reference = reported(&reference, "PlatformBody");
    assert_eq!(
        reference
            .iter()
            .map(|entry| format!("{}:{}: {}", entry.line, entry.column, entry.rendered))
            .collect::<Vec<_>>(),
        [
            "5:16: 'public final actual fun extra(): Int' has no corresponding expected declaration",
            "6:16: 'public final actual val spare: Int' has no corresponding expected declaration",
        ],
        "the owner and the member it does actualize are both silent; the other two are not"
    );
    assert_eq!(
        reported(&krusty, "PlatformBody"),
        reference,
        "and krusty's whole report is the same one"
    );
}

/// A declaration with CONTEXT PARAMETERS renders them, ahead of everything else it says.
///
/// The names are the declaration's own and the types are the resolved ones, so this is the same
/// hybrid every other rendering here is — and the group is printed before the visibility slot,
/// which is the one position no other modifier occupies. A property with context parameters used
/// to be passed over entirely rather than rendered.
#[test]
fn context_parameters_are_rendered_before_the_visibility() {
    assert_identical(
        "package plib\n\
         \n\
         class Tally\n\
         class Ledger\n\
         \n\
         context(tally: Tally)\n\
         actual val slotted: Int get() = 1\n\
         \n\
         context(tally: Tally, ledger: Ledger)\n\
         actual var counted: Int\n\
         \x20   get() = 2\n\
         \x20   set(value) {}\n\
         \n\
         context(tally: Tally)\n\
         actual fun folded(): Int = 3\n\
         \n\
         context(tally: Tally, ledger: Ledger)\n\
         actual fun weighed(spare: Int): Int = spare\n",
        "ContextParameters",
    );
}
