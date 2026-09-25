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
use common::Reported;

/// The sentence this check emits. It is never used to SELECT report lines — the ledgers below are
/// complete — only to state that a fixture the check must stay silent about produced none.
/// The files one differential compiles, and the extra reference-compiler arguments a split source
/// set needs.
struct Args<'a> {
    sources: &'a [std::path::PathBuf],
    reference: &'a [&'a str],
    /// Whether both compilers are told this is a multiplatform project. `false` is a differential
    /// of its own: `expect`/`actual` outside one is rejected at the modifier by both compilers,
    /// and this check must not pile a second sentence on top of that.
    multiplatform: bool,
}

/// Run both compilers over the files already written into `dir` and return both complete ledgers.
fn both_reports(
    dir: &std::path::Path,
    stem: &str,
    args: Args<'_>,
) -> (Vec<Reported>, Vec<Reported>) {
    let reference_out = dir.join(format!("{stem}-reference"));
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let mut reference_args: Vec<String> = args
        .multiplatform
        .then(|| "-Xmulti-platform".to_string())
        .into_iter()
        .collect();
    reference_args.extend(
        args.reference
            .iter()
            .map(|argument| (*argument).to_string()),
    );
    reference_args.extend([
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
    ]);
    reference_args.extend(
        args.sources
            .iter()
            .map(|source| source.to_string_lossy().into_owned()),
    );
    let (status, reference) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc available");

    let out = std::process::Command::new(common::krusty_binary())
        .args(
            args.multiplatform
                .then_some("-XXLanguage:+MultiPlatformProjects")
                .into_iter()
                .chain(["-no-stdlib", "-no-jdk", "-cp"]),
        )
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(dir.join(format!("{stem}-krusty")))
        .args(args.sources)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));
    // The two compilers must agree on whether the source is REJECTED, not only on what they
    // printed. A krusty run that died before reaching the check, or a fixture that stopped
    // provoking anything, otherwise compares two empty ledgers and passes.
    assert_eq!(
        status != 0,
        !out.status.success(),
        "the compilers disagree on whether this source is rejected\nreference:\n{reference}\nkrusty:\n{krusty}"
    );

    (common::reported(&reference), common::reported(&krusty))
}

/// Compile one single-file `source` with both compilers and return both complete ledgers.
fn both(source: &str, stem: &str) -> (Vec<Reported>, Vec<Reported>) {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join(format!("{stem}.kt"));
    std::fs::write(&file, source).expect("write the fixture");
    both_reports(
        &dir,
        stem,
        Args {
            sources: std::slice::from_ref(&file),
            reference: &[],
            multiplatform: true,
        },
    )
}

/// Compile a SPLIT source set — a common fragment and a platform one — with both compilers.
///
/// The reference compiler rejects an `expect` and its `actual` in the same module before it reaches
/// any of the questions here, so a single-file fixture can only ever measure the unmatched half.
/// `-Xcommon-sources` is how kotlinc is told which of the files it is compiling are the common
/// fragment; `-Xexpect-actual-classes` silences its beta warning for classifiers.
fn both_split(
    stem: &str,
    common: &[(&str, &str)],
    platform: &[(&str, &str)],
) -> (Vec<Reported>, Vec<Reported>) {
    let dir = common::scratch_dir().expect("scratch dir");
    let write = |files: &[(&str, &str)]| {
        files
            .iter()
            .map(|(name, source)| {
                let path = dir.join(name);
                std::fs::write(&path, source).expect("write a fragment");
                path
            })
            .collect::<Vec<_>>()
    };
    let common_paths = write(common);
    let mut sources = common_paths.clone();
    sources.extend(write(platform));
    let common_sources = format!(
        "-Xcommon-sources={}",
        common_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(",")
    );
    both_reports(
        &dir,
        stem,
        Args {
            sources: &sources,
            reference: &["-Xexpect-actual-classes", &common_sources],
            multiplatform: true,
        },
    )
}

/// Assert that krusty's report is the reference compiler's except for an exact, version-recorded
/// list of diagnostics krusty does not implement. The implemented ledger must remain an ordered
/// subsequence, and both the number and complete text of the gaps are pinned.
fn assert_identical_except(source: &str, stem: &str, unimplemented_count: usize) {
    let (reference, krusty) = both(source, stem);
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
    let expected = reference
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let actual = krusty.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut actual_at = 0;
    let mut unimplemented = Vec::new();
    for entry in &expected {
        if actual.get(actual_at) == Some(entry) {
            actual_at += 1;
        } else {
            unimplemented.push(entry.clone());
        }
    }
    assert_eq!(
        actual_at,
        actual.len(),
        "krusty's ledger is not a subsequence"
    );
    assert_eq!(unimplemented.len(), unimplemented_count, "unexpected gaps");
    let recorded = common::recorded_named("unimplemented", || unimplemented.clone());
    assert_eq!(
        unimplemented, recorded,
        "the exact excused diagnostics changed"
    );
    assert_eq!(
        actual.len() + unimplemented.len(),
        expected.len(),
        "krusty's complete ledger plus the named gaps must be the reference compiler's"
    );
}

/// Assert the reference compiler still reports `reference` as recorded for the running test under
/// this Kotlin version, and that it holds `count` entries.
fn assert_recorded_reference(reference: Vec<String>, count: usize, what: &str) {
    assert_eq!(reference.len(), count, "{what}: {reference:#?}");
    let recorded = common::recorded(|| reference.clone());
    assert_eq!(reference, recorded, "{what}");
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
         class Parcel<T>\n\
         \n\
         actual fun simple(): Int = 1\n\
         actual fun inferred() = 1\n\
         actual fun unitRet() { }\n\
         actual fun nullableRet(): String? = null\n\
         actual fun vararged(vararg xs: Int) { }\n\
         actual fun defaulted(a: Int = 1) { }\n\
         actual fun <T> generic(t: T): T = t\n\
         actual fun <T : Parcel<T>> bounded(t: T): T = t\n\
         actual fun Parcel<Int>.receiver(): Int = 1\n\
         actual fun functionParam(f: (Int) -> String): Int = 1\n\
         actual suspend fun susp(): Int = 1\n\
         actual inline fun inl(): Int = 1\n\
         internal actual fun internalFun(): Int = 1\n\
         private actual fun privateFun(): Int = 1\n\
         actual fun manyParams(a: Int, b: String?, c: Parcel<Int>, vararg rest: Long): Int = 1\n",
        "Callables",
    );
}

/// The same renderings over STDLIB classifiers, as a named integration case.
///
/// The mechanism cases above deliberately use a fixture-owned `Parcel<T>`: a stdlib classifier can
/// agree through builtin type handling rather than through the declaration renderer under test, so
/// it proves parity for those declarations and not that the renderer is general. This is where
/// that parity is measured, and it is not where a rendering rule is established.
#[test]
fn stdlib_shaped_declarations_render_the_same_way() {
    assert_identical(
        "package plib\n\
         \n\
         actual fun <T : Comparable<T>> bounded(t: T): T = t\n\
         actual fun Int.receiver(): Int = this\n\
         actual fun listParam(c: List<Int>): Int = 1\n\
         actual val <T> List<T>.ext: Int get() = 1\n\
         actual typealias GenericAlias<T> = Array<T>\n",
        "Stdlib",
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
         actual inline operator fun Int.times(other: Int): Int = other\n",
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
         class Parcel<T>\n\
         \n\
         actual val prop: Int = 2\n\
         actual var mutable: String = \"\"\n\
         actual val lambdaProp: (Int) -> Unit = {}\n\
         actual val <T> Parcel<T>.ext: Int get() = 1\n\
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
         actual class BoundedParams<T : Base, U>\n\
         actual object Obj\n\
         actual interface Iface\n\
         actual sealed interface SealedIface\n\
         actual fun interface FunIface { fun call(): Int }\n\
         actual open class Op\n\
         actual abstract class Ab\n\
         actual sealed class Se\n\
         actual data class Data(val x: Int)\n\
         @JvmInline actual value class Wrapped(val x: Int)\n\
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
    assert_identical_except(
        "package plib\n\
         \n\
         class Parcel<T>\n\
         \n\
         actual typealias Alias = String\n\
         actual typealias GenericAlias<T> = Parcel<T>\n\
         actual typealias FunAlias = (Int) -> String\n",
        "Aliases",
        // A function type is a classifier with declaration-site variance, so no function-type
        // alias can avoid these two diagnostics. Their complete versioned lines are recorded.
        2,
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
    let (reference, krusty) = both_split(
        "Matched",
        &[(
            "MatchedCommon.kt",
            "package plib\n\
             \n\
             expect fun helper(): Int\n\
             expect class Holder\n\
             expect val prop: Int\n\
             expect class Aliased\n",
        )],
        &[(
            "MatchedPlatform.kt",
            "package plib\n\
             \n\
             actual fun helper(): Int = 1\n\
             actual class Holder\n\
             actual val prop: Int = 2\n\
             actual typealias Aliased = String\n\
             \n\
             fun ordinary(): Int = helper()\n",
        )],
    );
    assert!(
        reference.is_empty(),
        "the fixture must be a source the reference compiler ACCEPTS: {reference:?}"
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
    // `stray` actualizes nothing, so the ledger this compares is NOT empty: the alias-matched
    // declarations are shown to be silent by their ABSENCE from a complete report that names
    // something else, rather than by a report that could be empty because nothing ran.
    let (reference, krusty) = both_split(
        "AliasMatched",
        &[(
            "AliasMatchedCommon.kt",
            "package plib\n\
             \n\
             expect class S\n\
             expect fun f(value: S): S\n\
             expect val S.tag: S\n",
        )],
        &[(
            "AliasMatchedPlatform.kt",
            "package plib\n\
             \n\
             actual fun f(value: String): String = value\n\
             actual val String.tag: String get() = this\n\
             actual typealias S = String\n\
             actual fun stray(): Int = 1\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert_recorded_reference(
        reference.iter().map(ToString::to_string).collect(),
        1,
        "only the unmatched declaration is reported",
    );
}

/// An `actual` in a file whose `expect` lives in ANOTHER file of the same source set is matched:
/// the question is about the source set, not about one file.
///
/// `stray` actualizes nothing, so the complete ledger this compares is not empty and `helper`'s
/// silence is its absence from a report that names something else.
#[test]
fn the_expect_may_live_in_another_file() {
    let (reference, krusty) = both_split(
        "CrossFileMatch",
        &[(
            "CrossFileMatchCommon.kt",
            "package plib\n\nexpect fun helper(): Int\n",
        )],
        &[(
            "CrossFileMatchPlatform.kt",
            "package plib\n\nactual fun helper(): Int = 1\n\nactual fun stray(): Int = 2\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert_recorded_reference(
        reference.iter().map(ToString::to_string).collect(),
        1,
        "only the unmatched declaration is reported",
    );
}

/// Without the multiplatform feature the modifier is rejected outright, and this check does not
/// pile a second sentence on top of that one.
///
/// A differential like every other case here: the reference compiler is run without
/// `-Xmulti-platform` too, so what it reports for the same source is the expectation, and the
/// comparison is each compiler's COMPLETE ledger rather than a probe for one sentence's absence.
#[test]
fn the_check_needs_the_multiplatform_feature() {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join("UngatedActual.kt");
    std::fs::write(&file, "package plib\n\nactual fun helper(): Int = 1\n")
        .expect("write the fixture");
    let (reference, krusty) = both_reports(
        &dir,
        "UngatedActual",
        Args {
            sources: std::slice::from_ref(&file),
            reference: &[],
            multiplatform: false,
        },
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert_eq!(
        reference
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![
            "UngatedActual.kt:3:1: 'expect' and 'actual' declarations can be used only in multiplatform projects. Learn more about Kotlin Multiplatform: https://kotl.in/multiplatform-setup"
                .to_string(),
        ],
        "the feature gate is the whole report: nothing is added on top of it"
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
    let ledger = common::ledger;
    let reference = ledger(&reference);
    assert_recorded_reference(
        reference.clone(),
        5,
        "the reference compiler's whole ledger across the three files",
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

    let reference = common::reported(&reference);
    assert_recorded_reference(
        reference.iter().map(ToString::to_string).collect(),
        2,
        "the owner and the member it does actualize are both silent; the other two are not",
    );
    assert_eq!(
        common::reported(&krusty),
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

/// Members that TIE on actualization's coarse child key still pair, so neither is reported.
///
/// Actualization keys a matched classifier's children by kind, name, receiver and arity, which two
/// overloads differing only in a parameter's TYPE share. Giving up on that tie left BOTH unpaired,
/// and reporting a member by its own outcome then named two members that had actualized perfectly
/// well — `extensionMember`, `extensionFunctionInDelegatedSam`,
/// `delegationToExpectInterface_withNewMembersSameName` and `overloadAlias3-3` in the box corpus.
/// The same parameter-shape comparison the top-level matcher already makes breaks the tie.
///
/// A member EXTENSION function is here for a second reason: it lives in its own receiver-keyed
/// table, so looking for it among the ordinary methods found no resolved signature at all.
#[test]
fn members_that_tie_on_the_child_key_still_actualize() {
    let dir = common::scratch_dir().expect("scratch dir");
    let header = dir.join("TiedHeader.kt");
    let platform = dir.join("TiedBody.kt");
    std::fs::write(
        &header,
        "package plib\n\
         \n\
         class Tally\n\
         class Ledger\n\
         \n\
         expect class Holder {\n\
         \x20   fun absorb(one: Tally): Int\n\
         \x20   fun absorb(one: Ledger): Int\n\
         \x20   fun Tally.counted(): Int\n\
         \x20   val Tally.tallied: Int\n\
         }\n",
    )
    .expect("write the common header");
    std::fs::write(
        &platform,
        "package plib\n\
         \n\
         actual class Holder {\n\
         \x20   actual fun absorb(one: Tally): Int = 1\n\
         \x20   actual fun absorb(one: Ledger): Int = 2\n\
         \x20   actual fun Tally.counted(): Int = 3\n\
         \x20   actual val Tally.tallied: Int get() = 4\n\
         }\n",
    )
    .expect("write the platform body");

    let reference_out = dir.join("tied-reference");
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
        .arg(dir.join("tied-krusty"))
        .arg(&header)
        .arg(&platform)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        common::reported(&reference),
        vec![],
        "every member actualizes one of the expect members"
    );
    assert_eq!(
        common::reported(&krusty),
        vec![],
        "and krusty agrees — a tie on the child key is broken, not abandoned"
    );
    // A dropped diagnostic would also produce an empty report, so the fixture's own health is
    // stated: krusty compiled it, rather than failing before the check could run.
    assert!(
        !krusty.contains("internal error"),
        "no member is left without a resolved record:\n{krusty}"
    );
}

/// A plain declaration of the same shape does NOT actualize an `expect`.
///
/// Actualization used to accept every non-`expect` declaration as a candidate implementation,
/// because compact headers carried no record of the modifier. A declaration that happens to have
/// the `expect`'s package, kind, name, receiver and arity then filled it — which silences the
/// unmatched-`expect` error, excludes the `expect` subtree from the module and hands the
/// declaration the `expect`'s defaults, all from a coincidence.
#[test]
fn an_ordinary_declaration_of_the_same_shape_does_not_actualize() {
    let (reference, krusty) = both_split(
        "PlainShape",
        &[(
            "PlainShapeCommon.kt",
            "package plib\n\
             \n\
             expect fun paired(value: Int): Int\n\
             expect class Held\n",
        )],
        &[(
            "PlainShapePlatform.kt",
            "package plib\n\
             \n\
             fun paired(value: Int): Int = value\n\
             class Held\n",
        )],
    );
    assert!(
        !reference.is_empty(),
        "the reference compiler must reject an `expect` nothing actualizes"
    );
    assert_eq!(
        krusty, reference,
        "krusty's complete ledger must be the reference compiler's"
    );
}

/// The same classifier written two ways — imported in one fragment, fully qualified in the other —
/// is ONE classifier, and the pair matches.
///
/// A comparison of the two declarations' unresolved type PATHS sees `Tally` against
/// `plib.model.Tally` and refuses them.
#[test]
fn an_imported_and_a_qualified_spelling_of_one_classifier_match() {
    let (reference, krusty) = both_split(
        "SpellingMatch",
        &[
            (
                "SpellingMatchModel.kt",
                "package plib.model\n\nclass Tally\n",
            ),
            (
                "SpellingMatchCommon.kt",
                "package plib\n\
                 \n\
                 expect fun takes(value: plib.model.Tally): Int\n",
            ),
        ],
        &[(
            "SpellingMatchPlatform.kt",
            "package plib\n\
             \n\
             import plib.model.Tally\n\
             \n\
             actual fun takes(value: Tally): Int = 1\n",
        )],
    );
    assert!(
        reference.is_empty(),
        "the reference compiler accepts the pair: {reference:?}"
    );
    assert!(
        krusty.is_empty(),
        "and so must krusty, rather than reporting the `actual`: {krusty:?}"
    );
}

/// Two classifiers with the same SIMPLE name in different packages are two classifiers, and the
/// pair does not match.
///
/// This is the other half of the spelling question: a comparison that resolves nothing sees
/// `Tally` against `Tally` and pairs declarations that have nothing to do with each other.
#[test]
fn identical_simple_names_from_different_packages_do_not_match() {
    let (reference, krusty) = both_split(
        "SpellingClash",
        &[
            ("SpellingClashLeft.kt", "package plib.left\n\nclass Tally\n"),
            (
                "SpellingClashRight.kt",
                "package plib.right\n\nclass Tally\n",
            ),
            (
                "SpellingClashCommon.kt",
                "package plib\n\
                 \n\
                 import plib.left.Tally\n\
                 \n\
                 expect fun takes(value: Tally): Int\n",
            ),
        ],
        &[(
            "SpellingClashPlatform.kt",
            "package plib\n\
             \n\
             import plib.right.Tally\n\
             \n\
             actual fun takes(value: Tally): Int = 1\n",
        )],
    );
    assert!(
        !reference.is_empty(),
        "the reference compiler must refuse the pair"
    );
    assert_eq!(
        krusty, reference,
        "krusty's complete ledger must be the reference compiler's"
    );
}

/// An import ALIAS renames a classifier; it does not rename the classifier's identity.
///
/// The scope this matcher resolves in is the one the parser published, aliases included, so
/// `import plib.model.Tally as Ledger` puts `plib.model.Tally` under the name `Ledger` and the two
/// sides pair. Reading the last segment of the import path as the name it brings into scope — a
/// second scope rebuilt from the types a file mentions could do nothing else — left `Ledger`
/// unresolved and paired it with nothing.
#[test]
fn an_import_alias_names_the_classifier_it_renames() {
    let (reference, krusty) = both_split(
        "ImportAlias",
        &[
            ("ImportAliasModel.kt", "package plib.model\n\nclass Tally\n"),
            (
                "ImportAliasCommon.kt",
                "package plib\n\
                 \n\
                 import plib.model.Tally\n\
                 \n\
                 expect fun takes(value: Tally): Int\n",
            ),
        ],
        &[(
            "ImportAliasPlatform.kt",
            "package plib\n\
             \n\
             import plib.model.Tally as Ledger\n\
             \n\
             actual fun takes(value: Ledger): Int = 1\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert_eq!(
        reference
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "one classifier under two names is one classifier, so the pair matches in silence"
    );
}

/// A WILDCARD import supplies a classifier the file names without qualifying it.
#[test]
fn a_star_import_supplies_the_classifier_it_brings_into_scope() {
    let (reference, krusty) = both_split(
        "StarImport",
        &[
            ("StarImportModel.kt", "package plib.model\n\nclass Tally\n"),
            (
                "StarImportCommon.kt",
                "package plib\n\
                 \n\
                 import plib.model.Tally\n\
                 \n\
                 expect fun takes(value: Tally): Int\n",
            ),
        ],
        &[(
            "StarImportPlatform.kt",
            "package plib\n\
             \n\
             import plib.model.*\n\
             \n\
             actual fun takes(value: Tally): Int = 1\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert_eq!(
        reference
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "the wildcard says which classifier `Tally` is, so the pair matches"
    );
}

/// A wildcard import brings in ANOTHER package's classifier of the same simple name, and the pair
/// must not match.
///
/// This is the wildcard half of the spelling question. A matcher that resolves nothing sees
/// `Tally` against `Tally` and pairs two declarations that name different classifiers; one that
/// reads only explicit imports sees the common file's `plib.left.Tally` against a bare `Tally` on
/// the platform side and pairs them just as wrongly. Only reading the WILDCARD says the platform
/// declaration means `plib.right.Tally`.
#[test]
fn a_star_imported_classifier_of_another_package_does_not_pair() {
    let (reference, krusty) = both_split(
        "StarImportClash",
        &[
            (
                "StarImportClashLeft.kt",
                "package plib.left\n\nclass Tally\n",
            ),
            (
                "StarImportClashRight.kt",
                "package plib.right\n\nclass Tally\n",
            ),
            (
                "StarImportClashCommon.kt",
                "package plib\n\
                 \n\
                 import plib.left.Tally\n\
                 \n\
                 expect fun takes(value: Tally): Int\n",
            ),
        ],
        &[(
            "StarImportClashPlatform.kt",
            "package plib\n\
             \n\
             import plib.right.*\n\
             \n\
             actual fun takes(value: Tally): Int = 1\n",
        )],
    );
    assert!(
        !reference.is_empty(),
        "the reference compiler must refuse the pair"
    );
    assert_eq!(
        krusty, reference,
        "krusty's complete ledger must be the reference compiler's"
    );
}

/// A member EXTENSION whose receiver is the member's OWN type parameter still pairs.
///
/// `val <S> S.kept: S` declares an `S` that shadows its owner's, so the receiver names no
/// classifier at all — its identity is positional within the declaration. Refusing to key such a
/// member left the `expect` and an `actual` written exactly like it pairing with nothing, and the
/// implementation reported as actualizing nothing; box
/// `multiplatform/k2/basic/expectActualFakeOverridesWithTypeParameters.kt` caught it.
///
/// `stray` actualizes nothing, so the complete ledger this compares is not empty and `kept`'s
/// silence is its absence from a report that names something else.
#[test]
fn a_member_extension_on_its_own_type_parameter_matches() {
    let (reference, krusty) = both_split(
        "OwnTypeParameterReceiver",
        &[(
            "OwnTypeParameterReceiverCommon.kt",
            "package plib\n\
             \n\
             expect class Holder<S> {\n\
             \x20   val <S> S.kept: S\n\
             }\n",
        )],
        &[(
            "OwnTypeParameterReceiverPlatform.kt",
            "package plib\n\
             \n\
             actual class Holder<S> {\n\
             \x20   actual val <S> S.kept: S get() = this\n\
             \x20   actual val <S> S.stray: S get() = this\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
}

/// A member extension on the OWNER's type parameter is the same category as one on its own.
///
/// `val O.kept: Int` names a type parameter the classifier declares. No scope binds it to a
/// classifier either, so it keys the same way — but through the enclosing half of the positional
/// map rather than the declaration's own.
#[test]
fn a_member_extension_on_its_owners_type_parameter_matches() {
    let (reference, krusty) = both_split(
        "OwnerTypeParameterReceiver",
        &[(
            "OwnerTypeParameterReceiverCommon.kt",
            "package plib\n\
             \n\
             expect class Owned<O> {\n\
             \x20   val O.kept: Int\n\
             }\n",
        )],
        &[(
            "OwnerTypeParameterReceiverPlatform.kt",
            "package plib\n\
             \n\
             actual class Owned<O> {\n\
             \x20   actual val O.kept: Int get() = 1\n\
             \x20   actual val O.stray: Int get() = 2\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
}

/// A member extension FUNCTION on a type-parameter receiver keys like the property does.
///
/// It reaches the other input-shape comparison — the one that also counts value parameters — so it
/// is a distinct source form and is asserted separately.
#[test]
fn a_member_extension_function_on_a_type_parameter_matches() {
    let (reference, krusty) = both_split(
        "TypeParameterReceiverFunction",
        &[(
            "TypeParameterReceiverFunctionCommon.kt",
            "package plib\n\
             \n\
             expect class Calls<O> {\n\
             \x20   fun <S> S.kept(): S\n\
             }\n",
        )],
        &[(
            "TypeParameterReceiverFunctionPlatform.kt",
            "package plib\n\
             \n\
             actual class Calls<O> {\n\
             \x20   actual fun <S> S.kept(): S = this\n\
             \x20   actual fun <S> S.stray(): S = this\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
}

/// Two type-parameter receivers that differ only by their UPPER BOUND are different declarations.
///
/// A receiver written as a type parameter binds to no classifier, so the key says only that. The
/// bound is the whole of what such a receiver says about the values it admits, and without
/// comparing it an implementation with an unrelated bound paired with the `expect` and the check
/// went silent where the reference compiler does not. Asserted on a TOP-LEVEL pair, which reaches
/// the same input-shape comparison without also asking what a classifier reports for a member it
/// never had actualized.
#[test]
fn a_type_parameter_receiver_compares_its_bound() {
    let (reference, krusty) = both_split(
        "TypeParameterReceiverBound",
        &[(
            "TypeParameterReceiverBoundCommon.kt",
            "package plib\n\
             \n\
             expect fun <S : Number> S.kept(): Int\n",
        )],
        &[(
            "TypeParameterReceiverBoundPlatform.kt",
            "package plib\n\
             \n\
             actual fun <S : CharSequence> S.kept(): Int = 1\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "an unrelated upper bound is not an implementation of the expectation"
    );
}

/// A member extension property renders EVERY own formal, with its bound.
///
/// The `<S>` case is reached through a receiver no scope binds. This one is on a bound receiver
/// and declares two formals, one of them bounded, so the rendering is exercised where the keying
/// is not in question — and a renderer that dropped a formal or mis-indexed a bound would differ
/// from the reference compiler here.
#[test]
fn a_member_extension_property_renders_its_own_formals() {
    let (reference, krusty) = both_split(
        "MemberExtensionFormals",
        &[(
            "MemberExtensionFormalsCommon.kt",
            "package plib\n\
             \n\
             expect class Rendered\n",
        )],
        &[(
            "MemberExtensionFormalsPlatform.kt",
            "package plib\n\
             \n\
             actual class Rendered {\n\
             \x20   actual val <S : Comparable<S>, T> Map<S, T>.stray: Int get() = 1\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "the fixture must make the reference compiler report something"
    );
}

/// A classifier that implements NONE of its expectation's members is reported on the classifier.
///
/// A member actualizes by its own identity, so a matched owner says nothing about them, and an
/// owner that implements none of them was otherwise accepted in silence — the module compiled. The
/// reference compiler names the implementation once, at its own name, rather than reporting each
/// `expect` member as unfilled from the side that did not get it wrong.
///
/// The fixture declares every member shape the listing under that line has to render — a nullable
/// type, a generic argument, a star projection, a function type, a default, a `vararg`, a bounded
/// own formal, an extension receiver and a `suspend` modifier. Only the first line reaches this
/// comparison: the listing follows a newline inside the same diagnostic and the reference
/// compiler's own output interleaves source echoes, so the two cannot be compared as text. The
/// shapes are asserted here so that a renderer which panics or drops one is still caught.
#[test]
fn a_classifier_owing_expected_members_is_reported() {
    let (reference, krusty) = both_split(
        "OwedMembers",
        &[(
            "OwedMembersCommon.kt",
            "package plib\n\
             \n\
             expect class Owed<T> {\n\
             \x20   val simple: Int\n\
             \x20   var mutable: String?\n\
             \x20   fun unitFun()\n\
             \x20   fun takes(a: Int, b: List<String>): Int\n\
             \x20   fun defaulted(a: Int = 1): Int\n\
             \x20   fun <S : Number> generic(s: S): S\n\
             \x20   val T.onReceiver: Int\n\
             \x20   val <S> S.own: S\n\
             \x20   fun higher(f: (Int) -> String): Int\n\
             \x20   val starred: List<*>\n\
             \x20   fun varargs(vararg xs: Int): Int\n\
             \x20   suspend fun suspends(): Int\n\
             }\n",
        )],
        &[(
            "OwedMembersPlatform.kt",
            "package plib\n\
             \n\
             actual class Owed<T> {\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "a classifier that implements none of its expectation's members is an error"
    );
}

/// A RENAMED type parameter is an incompatibility between counterparts, not a refusal to pair.
///
/// The reference compiler requires an `expect`/`actual` pair to spell its type parameters alike,
/// and reports a rename between two declarations it already considers each other's — saying the
/// implementation answered for nothing names the wrong fault, and says nothing about the owner
/// owing the member either.
#[test]
fn a_renamed_type_parameter_is_an_incompatibility() {
    let (reference, krusty) = both_split(
        "RenamedTypeParameter",
        &[(
            "RenamedTypeParameterCommon.kt",
            "package plib\n\
             \n\
             expect class Renamed<S> {\n\
             \x20   val <S> S.kept: S\n\
             }\n",
        )],
        &[(
            "RenamedTypeParameterPlatform.kt",
            "package plib\n\
             \n\
             actual class Renamed<S> {\n\
             \x20   actual val <T> T.kept: T get() = this\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "a renamed type parameter is an error, not an accepted pair"
    );
}

/// A MEMBER whose upper bound differs is not its expectation's counterpart at all.
///
/// The distinction is the reference compiler's: a rename is an incompatibility BETWEEN
/// counterparts, while an unrelated upper bound means no counterpart was found — so the owner is
/// left owing the member and the implementation answers for nothing. Both are asserted, because a
/// rule that reported one of them as the other is right on one case and wrong on the other.
#[test]
fn a_member_whose_bound_differs_is_not_a_counterpart() {
    let (reference, krusty) = both_split(
        "MemberBoundDiffers",
        &[(
            "MemberBoundDiffersCommon.kt",
            "package plib\n\
             \n\
             expect class Bounded {\n\
             \x20   val <S : Number> S.kept: Int\n\
             }\n",
        )],
        &[(
            "MemberBoundDiffersPlatform.kt",
            "package plib\n\
             \n\
             actual class Bounded {\n\
             \x20   actual val <S : CharSequence> S.kept: Int get() = 1\n\
             }\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "an unrelated upper bound is not an implementation of the expectation"
    );
}

/// A `typealias` that actualizes an `expect class` also answers for the members keyed on it.
///
/// A member EXTENSION on the expect classifier is keyed by its receiver, and the platform fragment
/// writes that receiver as the alias's TARGET — so the child key only agrees once the alias has
/// been followed.
#[test]
fn a_member_extension_on_an_actualized_alias_matches() {
    let (reference, krusty) = both_split(
        "AliasReceiver",
        &[(
            "AliasReceiverCommon.kt",
            "package plib\n\
             \n\
             expect class Carried\n\
             \n\
             expect fun Carried.carried(): Int\n",
        )],
        &[(
            "AliasReceiverPlatform.kt",
            "package plib\n\
             \n\
             actual typealias Carried = String\n\
             \n\
             actual fun String.carried(): Int = length\n",
        )],
    );
    assert!(
        reference.is_empty(),
        "the reference compiler accepts the pair: {reference:?}"
    );
    assert!(krusty.is_empty(), "and so must krusty: {krusty:?}");
}

/// A file's `actual typealias`es are reported where the SOURCE writes them, not after everything
/// else it declares.
///
/// The aliases live in their own parser list, and a report that emptied one list after the other
/// put every alias last however the source interleaved them. Two aliases with a callable between
/// them, and a third after it, is the smallest source that can tell the two orders apart.
#[test]
fn an_alias_is_reported_where_the_source_writes_it() {
    assert_identical(
        "package plib\n\
         \n\
         actual typealias First = String\n\
         actual fun between(): Int = 1\n\
         actual typealias Second = Int\n\
         actual val trailing: Int get() = 2\n\
         actual typealias Third = Long\n",
        "Interleaved",
    );
}

/// A stdlib classifier written as its SIMPLE name against its fully qualified one — `String`
/// against `kotlin.String` — is one classifier, and the pair matches.
///
/// This is the spelling question on the one import form no source writes: `kotlin.*` is a DEFAULT
/// import, in scope without an `import` line, so the file's own import list cannot supply it. A
/// resolver that reaches the simple name only through a written import answers nothing for
/// `String` here and pairs the two declarations with nothing, where
/// `an_imported_and_a_qualified_spelling_of_one_classifier_match` — whose classifier IS reached
/// through a written import — passes either way.
///
/// Both directions are asserted, because a rule that canonicalizes only the qualified side is
/// right on one of them and wrong on the other.
#[test]
fn a_default_imported_classifier_matches_its_qualified_spelling() {
    let (reference, krusty) = both_split(
        "DefaultImportSpelling",
        &[(
            "DefaultImportSpellingCommon.kt",
            "package plib\n\
             \n\
             expect fun takes(value: String): Int\n\
             expect fun gives(value: kotlin.String): Int\n",
        )],
        &[(
            "DefaultImportSpellingPlatform.kt",
            "package plib\n\
             \n\
             actual fun takes(value: kotlin.String): Int = 1\n\
             actual fun gives(value: String): Int = 2\n\
             actual fun stray(value: String): Int = 3\n",
        )],
    );
    assert_eq!(krusty, reference, "the complete ledgers must agree");
    assert!(
        !reference.is_empty(),
        "`stray` actualizes nothing, so `takes` and `gives` are silent in a report that names \
         something else rather than in an empty one"
    );
}
