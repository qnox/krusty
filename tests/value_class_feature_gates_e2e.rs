//! The language features kotlinc's value-class declaration checker consults, against the reference
//! compiler of the release under test:
//!
//! - `JvmInlineMultiFieldValueClasses` (2.4.0 and 2.4.10 only) decides whether a `@JvmInline`
//!   value class may declare more than one primary-constructor parameter. 2.4.20 removed the
//!   feature: such a class is rejected, and one without `@JvmInline` asks for `FullValueClasses`.
//! - `CustomEqualsInValueClasses` lets a value class declare `equals` and `hashCode`, and makes an
//!   `operator fun equals` taking the class itself its typed equality.
//! - `AllowExpectValueClassesWithNoPrimaryConstructor` (2.4.20 only) lets an `expect` value class
//!   leave its primary constructor to the `actual`.
//!
//! Each rejection compares the complete error ledger, in order, with kotlinc's and with the
//! expected one written here; each accepted fixture compiles and runs with both compilers.

use super::common;
use krusty::kotlin_version::KotlinVersion;

/// Whether the release under test has `JvmInlineMultiFieldValueClasses`.
fn has_multi_field_feature() -> bool {
    krusty::features::LangFeatures::default()
        .table()
        .get("JvmInlineMultiFieldValueClasses")
        .is_some()
}

fn is_2_4_20() -> bool {
    krusty::kotlin_version::target() >= KotlinVersion::V2_4_20
}

/// Both compilers' complete error ledgers for `sources` must be `expected`. Language directives in
/// the sources go to kotlinc as `-XXLanguage:` arguments.
fn assert_ledger(sources: &[(&str, &str)], expected: &[String]) {
    let arguments = sources
        .iter()
        .flat_map(|(_, source)| common::language_directives::kotlinc_args(source))
        .collect::<Vec<_>>();
    assert_eq!(
        common::reference_error_ledger(sources, &arguments),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(sources, &arguments),
        expected
    );
}

fn at(entries: &[(&str, &str)]) -> Vec<String> {
    entries
        .iter()
        .map(|(position, message)| format!("Main.kt:{position}: {message}"))
        .collect()
}

const VALUE_CLASSES: &str = "@JvmInline
value class P(val x: Int, val y: Int)
@JvmInline
value class E()
@JvmInline
value class D(val x: Int = 1, val y: Int = 2)
value class Q(val x: Int, val y: Int)
@JvmInline
value class V(var x: Int, val y: Unit = Unit, z: Int = 3)
";

const FULL_VALUE_CLASSES: &str =
    "the feature \"full value classes\" is experimental and should be \
     enabled explicitly. This can be done by supplying the compiler argument \
     '-XXLanguage:+FullValueClasses', but note that no stability guarantees are provided.";
const WITHOUT_JVM_INLINE: &str =
    "value classes without '@JvmInline' annotation are not yet supported.";
const NOT_FINAL_READ_ONLY: &str =
    "value class primary constructor must only have final read-only ('val') property parameters.";
const MULTI_FIELD_DEFAULT: &str =
    "default parameters are not supported in the primary constructor of a multi-field value class.";

/// Without `JvmInlineMultiFieldValueClasses` (or on 2.4.20, where it no longer exists) a value
/// class represented inline has exactly one primary-constructor parameter. A `value class` with
/// more and no `@JvmInline` is a `FullValueClasses` use on 2.4.20.
#[test]
fn value_classes_have_one_parameter_without_multi_field_value_classes() {
    let expected = if is_2_4_20() {
        let one = "value class must have exactly one primary constructor parameter.";
        at(&[
            ("2:14", one),
            ("4:14", one),
            ("6:14", one),
            ("7:1", FULL_VALUE_CLASSES),
            ("9:14", one),
        ])
    } else {
        let one = "inline class must have exactly one primary constructor parameter.";
        at(&[
            ("2:14", one),
            ("4:14", one),
            ("6:14", one),
            ("7:1", WITHOUT_JVM_INLINE),
            ("7:14", one),
            ("9:14", one),
        ])
    };
    assert_ledger(&[("Main.kt", VALUE_CLASSES)], &expected);
}

/// With `JvmInlineMultiFieldValueClasses` the parameter count is any positive number, and the
/// checks of each parameter run: a multi-field class has no default arguments. On 2.4.20 the
/// directive names an unknown feature and changes nothing.
#[test]
fn multi_field_value_classes_check_their_parameters() {
    let source = format!("// LANGUAGE: +JvmInlineMultiFieldValueClasses\n{VALUE_CLASSES}");
    let expected = if has_multi_field_feature() {
        at(&[
            (
                "5:14",
                "value class must have at least one primary constructor parameter.",
            ),
            ("7:28", MULTI_FIELD_DEFAULT),
            ("7:44", MULTI_FIELD_DEFAULT),
            ("8:1", WITHOUT_JVM_INLINE),
            ("10:15", NOT_FINAL_READ_ONLY),
            (
                "10:34",
                "value class cannot have value parameter of type 'kotlin/Unit'.",
            ),
            ("10:47", NOT_FINAL_READ_ONLY),
        ])
    } else {
        let one = "value class must have exactly one primary constructor parameter.";
        at(&[
            ("3:14", one),
            ("5:14", one),
            ("7:14", one),
            ("8:1", FULL_VALUE_CLASSES),
            ("10:14", one),
        ])
    };
    assert_ledger(&[("Main.kt", source.as_str())], &expected);
}

/// 2.4.20 with `FullValueClasses`: a `@JvmInline` class names itself so in the arity message, and
/// a `value class` without the annotation is a full value class of any arity.
#[test]
fn full_value_classes_name_the_jvm_inline_kind() {
    if !is_2_4_20() {
        // `FullValueClasses` is not a feature of the earlier releases' tables.
        return;
    }
    let source = format!("// LANGUAGE: +FullValueClasses\n{VALUE_CLASSES}");
    let one = "@JvmInline value class must have exactly one primary constructor parameter.";
    let expected = at(&[("3:14", one), ("5:14", one), ("7:14", one), ("10:14", one)]);
    assert_ledger(&[("Main.kt", source.as_str())], &expected);
}

/// A multi-field `@JvmInline` value class is accepted with the feature. krusty's JVM backend does
/// not lower one yet, so the running fixture declares it beside the class it uses.
#[test]
fn multi_field_value_classes_are_accepted_with_the_feature() {
    if !has_multi_field_feature() {
        return;
    }
    common::expect_box_same_as_kotlinc(
        "// LANGUAGE: +JvmInlineMultiFieldValueClasses
@JvmInline
value class P(val x: Int, val y: String)
@JvmInline
value class S(val s: String)
fun box(): String = S(\"OK\").s
",
        "MultiFieldValueClasses",
    );
}

const EQUALS: &str = "@JvmInline
value class A(val x: Int) {
    override fun hashCode() = 42
    override fun equals(other: Any?) = other is A && other.x == x
}
@JvmInline
value class B(val x: Int) {
    operator fun equals(other: B): Boolean = x == other.x
}
@JvmInline
value class G<T>(val x: T) {
    operator fun equals(other: G<String>): Boolean = true
    fun <R> equals(other: G<*>): Boolean = true
}
interface I {
    fun box(): Int = 1
}
@JvmInline
value class C(val x: Int) : I {
    fun unbox(): Int = x
}
class N {
    operator fun equals(other: N): Boolean = true
}
operator fun Int.equals(other: Any?): Boolean = true
@JvmInline
value class H<T>(val x: T) {
    operator fun equals(other: H<String>): Boolean = true
}
";

const MUST_OVERRIDE: &str =
    "'operator' modifier is not applicable to function: must override 'equals()' in Any.";
const MUST_BE_MEMBER: &str =
    "'operator' modifier is not applicable to function: must be a member function.";
const BOX_FROM_I: &str =
    "member name 'box' is reserved for future releases but is implemented in supertype 'I'.";

fn reserved(name: &str) -> String {
    format!("member name '{name}' is reserved for future releases.")
}

/// Without `CustomEqualsInValueClasses`, `equals` and `hashCode` are reserved in a value class
/// like `box` and `unbox`, and an `operator fun equals` must override `Any.equals`.
#[test]
fn equals_and_hash_code_are_reserved_without_custom_equals() {
    let (equals, hash_code, unbox) = (reserved("equals"), reserved("hashCode"), reserved("unbox"));
    let expected = at(&[
        ("3:18", &hash_code),
        ("4:18", &equals),
        ("8:5", MUST_OVERRIDE),
        ("8:18", &equals),
        ("12:5", MUST_OVERRIDE),
        ("12:18", &equals),
        ("13:13", &equals),
        ("19:7", BOX_FROM_I),
        ("20:9", &unbox),
        ("23:5", MUST_OVERRIDE),
        ("25:1", MUST_BE_MEMBER),
        ("28:5", MUST_OVERRIDE),
        ("28:18", &equals),
    ]);
    assert_ledger(&[("Main.kt", EQUALS)], &expected);
}

/// With `CustomEqualsInValueClasses` the typed equality takes no type parameters and only
/// star-projected type arguments; `box` and `unbox` stay reserved.
#[test]
fn typed_equals_is_checked_with_custom_equals() {
    let source = format!("// LANGUAGE: +CustomEqualsInValueClasses\n{EQUALS}");
    let unbox = reserved("unbox");
    let expected = at(&[
        ("14:9", "type parameters are prohibited here."),
        ("20:7", BOX_FROM_I),
        ("21:9", &unbox),
        ("24:5", MUST_OVERRIDE),
        ("26:1", MUST_BE_MEMBER),
        (
            "29:32",
            "type arguments for typed value class equals must all be star projections.",
        ),
    ]);
    assert_ledger(&[("Main.kt", source.as_str())], &expected);
}

/// Overriding `Any.equals` without a typed equality is reported as inefficient, a warning. The
/// complete warning ledger is kotlinc's.
#[test]
fn equals_of_any_without_typed_equals_is_inefficient() {
    let source = "// LANGUAGE: +CustomEqualsInValueClasses
@JvmInline
value class A(val x: Int) {
    override fun equals(other: Any?) = other is A && other.x == x
}
@JvmInline
value class G<T>(val x: T) {
    override fun equals(other: Any?) = true
}
";
    let sources = [("Main.kt", source)];
    let result = common::compiler_diagnostics_with_reference_args(
        &sources,
        &[],
        &common::language_directives::kotlinc_args(source),
    );
    let warnings = |output: &str| {
        common::compiler_warnings(output)
            .into_iter()
            .map(|warning| {
                format!(
                    "{}:{}:{}: {}",
                    warning.file, warning.line, warning.column, warning.message
                )
            })
            .collect::<Vec<_>>()
    };
    let inefficient = |ty: &str| {
        format!(
            "overriding 'equals' from 'Any' in value class without operator 'equals(other: {ty}): \
             Boolean' leads to boxing on every equality comparison."
        )
    };
    let expected = at(&[("4:18", &inefficient("A")), ("8:18", &inefficient("G<*>"))]);
    assert_eq!(
        (result.reference_code, warnings(&result.reference_stderr)),
        (0, expected.clone())
    );
    assert_eq!(
        (result.krusty_code, warnings(&result.krusty_stderr)),
        (0, expected)
    );
}

/// With `CustomEqualsInValueClasses` a value class overrides `equals` and `hashCode`, and `==`
/// between its values runs that `equals`. (A typed `operator fun equals(other: B)` is accepted
/// too, but krusty's JVM backend does not yet realize it as the class's `equals-impl0`.)
#[test]
fn custom_equals_and_hash_code_run_with_the_feature() {
    common::expect_box_same_as_kotlinc(
        "// LANGUAGE: +CustomEqualsInValueClasses
@JvmInline
value class B(val x: Int) {
    override fun equals(other: Any?): Boolean = other is B && x % 10 == other.x % 10
    override fun hashCode(): Int = x % 10
}
fun box(): String = if (B(1) == B(11) && B(1).hashCode() == B(21).hashCode()) \"OK\" else \"fail\"
",
        "CustomEqualsInValueClasses",
    );
}

const COMMON: &str = "expect value class CommonUSize : Comparable<CommonUSize> {
    override operator fun compareTo(other: CommonUSize): Int
}
fun compareUSize(a: CommonUSize, b: CommonUSize) = a.compareTo(b)
expect value class CommonSomething
";

const PLATFORM: &str = "actual typealias CommonUSize = UInt
@JvmInline
actual value class CommonSomething(val value: Int)
fun box(): String {
    if (compareUSize(20.toUInt(), 30.toUInt()) >= 0) return \"fail\"
    return if (CommonSomething(10).value == 10) \"OK\" else \"fail\"
}
";

/// The complete error ledgers of kotlinc, given `common` as its common sources, and of krusty for
/// one multiplatform module of `common` and `platform`, under the `-XXLanguage:` `features`.
fn split_ledgers(
    common_source: &str,
    platform_source: &str,
    features: &[&str],
) -> (Vec<String>, Vec<String>) {
    let dir = common::scratch_dir().expect("scratch dir");
    let common_path = dir.join("Common.kt");
    let platform_path = dir.join("Platform.kt");
    std::fs::write(&common_path, common_source).expect("write the common fragment");
    std::fs::write(&platform_path, platform_source).expect("write the platform fragment");
    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let mut reference_args = vec![
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        format!("-Xcommon-sources={}", common_path.to_string_lossy()),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
    ];
    reference_args.extend(features.iter().map(|feature| feature.to_string()));
    reference_args.push(common_path.to_string_lossy().into_owned());
    reference_args.push(platform_path.to_string_lossy().into_owned());
    let (_, reference) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc is provisioned");
    let output = std::process::Command::new(common::krusty_binary())
        .arg("-XXLanguage:+MultiPlatformProjects")
        .args(features)
        .args(["-no-reflect", "-d"])
        .arg(dir.join("krusty"))
        .arg(&common_path)
        .arg(&platform_path)
        .output()
        .expect("run krusty");
    let _ = std::fs::remove_dir_all(dir);
    (
        common::ledger(&reference),
        common::ledger(&String::from_utf8_lossy(&output.stderr)),
    )
}

/// Without `AllowExpectValueClassesWithNoPrimaryConstructor` (and on the releases without it) an
/// `expect` value class requires a primary constructor, also when its `actual` supplies one.
#[test]
fn expect_value_classes_require_a_primary_constructor() {
    let required = "primary constructor is required for value classes.";
    let expected = vec![
        format!("Common.kt:1:8: {required}"),
        format!("Common.kt:5:8: {required}"),
    ];
    let (reference, krusty) = split_ledgers(COMMON, PLATFORM, &[]);
    assert_eq!(
        reference,
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(krusty, expected);
}

/// With the feature an `expect` value class leaves its constructor to the `actual`, which runs;
/// it may then declare no secondary constructor.
#[test]
fn expect_value_classes_without_primary_constructor_with_the_feature() {
    if !is_2_4_20() {
        return;
    }
    let feature = "-XXLanguage:+AllowExpectValueClassesWithNoPrimaryConstructor";
    assert_eq!(
        split_ledgers(COMMON, PLATFORM, &[feature]),
        (Vec::new(), Vec::new())
    );
    let directive =
        "// LANGUAGE: +MultiPlatformProjects +AllowExpectValueClassesWithNoPrimaryConstructor\n";
    let got = common::compile_and_run_files_with_stdlib(&[
        ("Common.kt", &format!("{directive}{COMMON}")),
        ("Platform.kt", &format!("{directive}{PLATFORM}")),
    ])
    .expect("krusty compiles and runs the split source set");
    assert_eq!(got, "OK");

    let with_secondary = "expect value class W {
    constructor(x: Int)
}
";
    let expected = vec![
        "Common.kt:2:5: expect value class without primary constructor cannot have secondary constructors."
            .to_string(),
    ];
    let (reference, krusty) = split_ledgers(with_secondary, "fun box() = \"OK\"\n", &[feature]);
    assert_eq!(reference, expected);
    assert_eq!(krusty, expected);
}
