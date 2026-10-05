//! Multiplatform `expect`/`actual` (JVM model): a platform source set and its `dependsOn` chain
//! compile as ONE set; compact-header actualization excludes every `expect` subtree a matching
//! `actual` (or `actual typealias`) replaces. Gated on
//! `// LANGUAGE: +MultiPlatformProjects`, like kotlinc.

use super::common;

fn run(src: &str) {
    let Some(got) = common::compile_and_run_with_stdlib(src, "MainKt") else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(got, "OK");
}

/// Expect fun + expect val (extension receivers distinguish the two `k`s) replaced by actuals.
#[test]
fn expect_fun_and_extension_val_actualized() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun greet(): String
expect val Int.k: String
expect val String.k: String

actual fun greet(): String = "O"
actual val Int.k: String get() = "K"
actual val String.k: String get() = ""

fun box(): String = greet() + 1.k + "".k
"#);
}

/// `expect class` replaced by an `actual class`; common code constructs and calls it.
#[test]
fn expect_class_actualized_by_class() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class Box() {
    fun value(): String
}

fun common(): String = Box().value()

actual class Box actual constructor() {
    actual fun value(): String = "OK"
}

fun box(): String = common()
"#);
}

/// `expect class` replaced by an `actual typealias` to an existing class.
#[test]
fn expect_class_actualized_by_typealias() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class S

fun use(s: S): S = s

actual typealias S = String

fun box(): String = use("OK")
"#);
}

/// Overloaded expect funs match by arity.
#[test]
fn expect_overloads_match_by_arity() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun f(): String
expect fun f(n: Int): String

actual fun f(): String = "O"
actual fun f(n: Int): String = "K"

fun box(): String = f() + f(1)
"#);
}

/// An UNMATCHED `expect` stays in the tree and fails checking — skip, never mis-grade.
#[test]
fn unmatched_expect_fails_compile() {
    let src = r#"// LANGUAGE: +MultiPlatformProjects
expect fun missing(): String

fun box(): String = missing()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "an expect without an actual must not compile"
    );
}

/// Without the language flag, `expect` gets no special treatment (body-less fun fails).
#[test]
fn expect_without_flag_is_not_stripped() {
    let src = r#"expect fun greet(): String
actual fun greet(): String = "OK"

fun box(): String = greet()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "expect/actual outside +MultiPlatformProjects must not resolve"
    );
}

/// The `open val` accessor of an actualized class stays non-final so a common-code subclass can
/// override it (the property analog of the open-member finality rule).
#[test]
fn open_property_accessor_overridable_across_expect_actual() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
class C2 : C1() {
    override val k = "K"
}

expect open class C1() {
    open val k: String
}

actual open class C1 {
    actual open val k = "FAIL"
}

fun box(): String = if (C2().k == "K") "OK" else "FAIL"
"#);
}

/// Parameter defaults live on the `expect` when the actual leaves them bare (kotlinc forbids an
/// actual default unless that diagnostic is suppressed). Stable actualization retains the expect
/// declaration as the Pass-2 expression provider for the actual ABI.
#[test]
fn expect_defaults_are_realized_by_actual() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun foo(a: String, b: String = "K"): String

actual fun foo(a: String, b: String): String = a + b

fun box(): String = foo("O")
"#);
}

/// An actual that writes its own defaults (the suppressed `ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS`
/// form) is the expression provider. The expect values are not substituted back in.
#[test]
fn actual_defaults_replace_expect_defaults() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun pick(a: Int = 1, b: Int = 2): Int

@Suppress("ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS")
actual fun pick(a: Int = 10, b: Int = 20): Int = a + b

expect fun <T> Array<out T>.copyInto(
    destination: Array<T>, destinationOffset: Int = 0, startIndex: Int = 0, endIndex: Int = size
): Array<T>

@Suppress("ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS")
actual fun <T> Array<out T>.copyInto(
    destination: Array<T>, destinationOffset: Int = 42, startIndex: Int = 43, endIndex: Int = size + 44
): Array<T> {
    destination as Array<Int>
    destination[0] = destinationOffset
    destination[1] = startIndex
    destination[2] = endIndex
    return destination
}

fun box(): String {
    val copied = arrayOf(0, 1, 2).copyInto(arrayOf(0, 1, 2))
    return if (pick() == 30 && copied[0] == 42 && copied[1] == 43 && copied[2] == 47) "OK"
        else "FAIL ${pick()} ${copied[0]} ${copied[1]} ${copied[2]}"
}
"#);
}

/// Default-expression ownership is per parameter. The actual owns every default it writes, while
/// an omitted actual default is inherited from the matching expect declaration. An override keeps
/// that merged provider set instead of collapsing it back to either declaration.
#[test]
fn mixed_actual_and_expect_defaults_reach_direct_and_override_calls() {
    const SRC: &str = r#"// LANGUAGE: +MultiPlatformProjects
expect fun choose(a: Int = 1, b: Int = 2): Int

@Suppress("ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS")
actual fun choose(a: Int = 10, b: Int): Int = a + b

expect open class Base() {
    open fun text(a: String = "expect-a", b: String = "expect-b"): String
}

actual open class Base {
    @Suppress("ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS")
    actual open fun text(a: String = "actual-a", b: String): String = a + ":" + b
}

class Derived : Base() {
    override fun text(a: String, b: String): String = a + ":" + b
}

fun box(): String {
    if (choose() != 12) return "top:" + choose()
    if (Base().text() != "actual-a:expect-b") return "base:" + Base().text()
    if (Derived().text() != "actual-a:expect-b") return "derived:" + Derived().text()
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "MixedActualExpectDefaults");
}

/// An interface actual's default is what an override's call site inserts. The expect string is not.
#[test]
fn actual_interface_default_reaches_an_override() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect interface I {
    fun test(source: String = "expect")
}

expect interface J : I

@Suppress("NO_ACTUAL_CLASS_MEMBER_FOR_EXPECTED_CLASS")
actual interface I {
    @Suppress("ACTUAL_FUNCTION_WITH_DEFAULT_ARGUMENTS")
    actual fun test(source: String = "actual")
}

actual interface J : I

interface K : J {
    override fun test(source: String) {
        if (source != "actual") throw AssertionError(source)
    }
}

class L : K

fun box(): String {
    L().test()
    return "OK"
}
"#);
}

/// An expect-owned default may reference a PRIOR parameter — kotlinc guarantees the actual's parameter
/// names match the expect's, so the name binds to the same position.
#[test]
fn expect_default_referencing_prior_parameter() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun rep(s: String, t: String = s): String

actual fun rep(s: String, t: String): String = s + t

fun box(): String = if (rep("X") == "XX") "OK" else "FAIL"
"#);
}

/// Constructor defaults keep their expect declaration as the Pass-2 expression provider while
/// the checked FIR and emitted default ABI belong to the actual constructor.
#[test]
fn expect_constructor_default_is_realized_by_actual_constructor() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class C(value: String = "OK") {
    fun get(): String
}

actual class C actual constructor(private val value: String) {
    actual fun get(): String = value
}

fun box(): String = C().get()
"#);
}

/// Body-less `expect val`/`var` (incl. extension receivers) parse as headers and strip away.
#[test]
fn expect_properties_including_extensions() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect val v: String
expect val Char.extensionVal: String
expect var String.extensionVar: Char

actual val v: String = ""
actual val Char.extensionVal: String
    get() = toString()
actual var String.extensionVar: Char
    get() = this[0]
    set(value) {}

fun box(): String = v + 'O'.extensionVal + "K".extensionVar
"#);
}

/// An UNMATCHED body-less `expect val` (parser now accepts the header shape) must still fail
/// downstream — never silently emit a property with no backing.
#[test]
fn unmatched_expect_property_fails_compile() {
    let src = r#"// LANGUAGE: +MultiPlatformProjects
expect val v: String

fun box(): String = v
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "an expect val without an actual must not compile"
    );
}

/// Defaults on an `expect class` member are checked for the matching actual member, and an
/// overriding subclass inherits them at call sites.
#[test]
fn expect_class_member_defaults_are_realized_by_actual() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect open class C() {
    open fun f(p: Int = 2): String
}

actual open class C {
    actual open fun f(p: Int): String = "C" + p
}

open class D : C() {
    override fun f(p: Int): String = "D" + p
}

fun box(): String {
    if (C().f() != "C2") return "FAIL0"
    if (C().f(9) != "C9") return "FAIL1"
    if (D().f() != "D2") return "FAIL2"
    return "OK"
}
"#);
}

/// An all-`expect` common FILE legitimately emits ZERO classes after stripping (kotlinc's JVM-MPP
/// model: the common source set has no bytecode of its own) — the module must still compile and
/// run. The box-corpus survey used to misreport this shape as an emit bail (its top skip-bucket);
/// this pins the compiler behavior the survey now mirrors.
#[test]
fn all_expect_common_file_module_compiles_and_runs() {
    let Some(out) = common::compile_and_run_files_with_stdlib(&[
        (
            "Common",
            r#"// LANGUAGE: +MultiPlatformProjects
expect class S
expect fun make(): S
"#,
        ),
        (
            "Main",
            r#"actual typealias S = String
actual fun make(): S = "OK"
fun box(): String = make()
"#,
        ),
    ]) else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(out, "OK");
}

/// A typealias-only FILE in a source set emits no classes of its own — the set must still compile
/// and run (the survey's whole-set counterpart is `typealias_only_set_reports_precise_reason`).
#[test]
fn typealias_only_file_in_set_compiles() {
    let Some(out) = common::compile_and_run_files_with_stdlib(&[
        ("Alias", "typealias Greeting = String"),
        ("Main", r#"fun box(): Greeting = "OK""#),
    ]) else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(out, "OK");
}

/// The same `expect class` / `actual typealias` pair split across TWO files. Pass 1 actualizes the
/// expect declaration away, but Pass 2 reparses raw source, so the removal has to be replayed there
/// — otherwise the checker walks a declaration whose signature was deliberately never collected and
/// the compiler aborts instead of compiling.
#[test]
fn expect_class_actualized_by_typealias_across_files() {
    let Some(got) = common::compile_and_run_files_with_stdlib(&[
        (
            "Common",
            "// LANGUAGE: +MultiPlatformProjects\nexpect class S\nexpect fun f0(s: S): S\n",
        ),
        (
            "Main",
            "// LANGUAGE: +MultiPlatformProjects\nactual typealias S = String\n\
             actual fun f0(s: S): S = s\nfun box(): String = f0(\"OK\")\n",
        ),
    ]) else {
        panic!("expected the multi-file expect/actual box to compile and run");
    };
    assert_eq!(got, "OK");
}

/// An `expect class` that writes no constructor has none to actualize (kotlinc gives it no default
/// constructor), so its `actual` may declare any constructor, here with a property parameter.
#[test]
fn expect_class_without_constructor_actualized_by_constructor_property() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class Holder {
    val t: String
}

fun Holder.read(): String = t

actual class Holder constructor(actual val t: String)

fun box(): String = Holder("OK").read()
"#);
}

/// An `expect` member is actualized by a member the `actual` class inherits from an ordinary
/// supertype, as a fake override.
#[test]
fn expect_member_actualized_by_inherited_member() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class A() {
    fun foo(s: String): String
    val bar: String
}

fun common(): String = A().foo("O") + A().bar

open class Base {
    fun foo(s: String) = s
    val bar: String = "K"
}

actual class A : Base()

fun box(): String = common()
"#);
}

/// A member the `actual` class declares hides only the inherited member it overrides: a different
/// overload of the same name and arity stays in the member scope and actualizes its `expect`.
/// Differential: the reference compiler accepts the split source set, and krusty compiles and runs
/// it.
#[test]
fn inherited_overload_beside_a_declared_one_actualizes_its_expect() {
    const COMMON: &str = "// LANGUAGE: +MultiPlatformProjects\n\
        expect class A() {\n\
        \x20   fun choose(value: Int): String\n\
        }\n\
        fun common(): String = A().choose(1)\n";
    const PLATFORM: &str = "// LANGUAGE: +MultiPlatformProjects\n\
        open class Base {\n\
        \x20   fun choose(value: Int): String = \"OK\"\n\
        }\n\
        actual class A : Base() {\n\
        \x20   fun choose(value: String): String = value\n\
        }\n\
        fun box(): String = common()\n";
    let dir = common::scratch_dir().expect("scratch dir");
    let common_path = dir.join("Common.kt");
    let platform_path = dir.join("Platform.kt");
    std::fs::write(&common_path, COMMON).expect("write the common fragment");
    std::fs::write(&platform_path, PLATFORM).expect("write the platform fragment");
    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (code, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        format!("-Xcommon-sources={}", common_path.to_string_lossy()),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        common_path.to_string_lossy().into_owned(),
        platform_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        (code, reference.as_str()),
        (0, ""),
        "the reference compiler accepts the split source set"
    );
    let got = common::compile_and_run_files_with_stdlib(&[
        ("Common.kt", COMMON),
        ("Platform.kt", PLATFORM),
    ])
    .expect("krusty compiles and runs the split source set");
    assert_eq!(got, "OK");
}

/// A member's header type resolves in its classifier's scope before the file's: `E` in the `expect`
/// constructor names the nested `C.E`, not the top-level `E`, on both sides, so the constructors
/// pair and a call omitting the argument takes the `expect` default `E.O`
/// (multiplatform/k2/defaultArguments/nestedEnumEntryValue.kt). Differential: the reference compiler
/// accepts the split source set, and krusty compiles and runs it.
#[test]
fn an_expect_constructor_parameter_names_a_nested_classifier() {
    const COMMON: &str = "// LANGUAGE: +MultiPlatformProjects\n\
        class E\n\
        expect class C(e: E = E.O) {\n\
        \x20   enum class E {\n\
        \x20       O, K\n\
        \x20   }\n\
        }\n";
    const PLATFORM: &str = "// LANGUAGE: +MultiPlatformProjects\n\
        actual class C actual constructor(e: E) {\n\
        \x20   val result = e\n\
        \x20   actual enum class E {\n\
        \x20       O, K\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (C().result == C.E.O && C(C.E.K).result == C.E.K) \"OK\" else \"fail\"\n";
    let dir = common::scratch_dir().expect("scratch dir");
    let common_path = dir.join("Common.kt");
    let platform_path = dir.join("Platform.kt");
    std::fs::write(&common_path, COMMON).expect("write the common fragment");
    std::fs::write(&platform_path, PLATFORM).expect("write the platform fragment");
    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (code, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        format!("-Xcommon-sources={}", common_path.to_string_lossy()),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        common_path.to_string_lossy().into_owned(),
        platform_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        (code, reference.as_str()),
        (0, ""),
        "the reference compiler accepts the split source set"
    );
    let got = common::compile_and_run_files_with_stdlib(&[
        ("Common.kt", COMMON),
        ("Platform.kt", PLATFORM),
    ])
    .expect("krusty compiles and runs the split source set");
    assert_eq!(got, "OK");
}

/// A `companion { … }` block member claims its `expect` with the `actual` it writes itself: the
/// member is hoisted to a file declaration, and its modifier travels with it
/// (multiplatform/k2/expectStatic.kt). Differential: the reference compiler accepts the split
/// source set, and krusty compiles and runs it.
#[test]
fn a_companion_block_member_actualizes_its_expect() {
    const COMMON: &str = "// LANGUAGE: +MultiPlatformProjects, +CompanionBlocksAndExtensions\n\
        class Input\n\
        enum class Mark { Block, Fail }\n\
        expect class A {\n\
        \x20   companion {\n\
        \x20       val a: Mark\n\
        \x20       fun foo(input: Input): Mark\n\
        \x20   }\n\
        }\n\
        fun common(): Mark = if (A.a == Mark.Block) A.foo(Input()) else Mark.Fail\n";
    const PLATFORM: &str = "// LANGUAGE: +MultiPlatformProjects, +CompanionBlocksAndExtensions\n\
        actual class A {\n\
        \x20   companion {\n\
        \x20       actual val a = Mark.Block\n\
        \x20       actual fun foo(input: Input): Mark = Mark.Block\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (common() == Mark.Block) \"OK\" else \"fail\"\n";
    let dir = common::scratch_dir().expect("scratch dir");
    let common_path = dir.join("Common.kt");
    let platform_path = dir.join("Platform.kt");
    std::fs::write(&common_path, COMMON).expect("write the common fragment");
    std::fs::write(&platform_path, PLATFORM).expect("write the platform fragment");
    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (code, reference) = common::kotlinc_compile(&[
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        "-XXLanguage:+CompanionBlocksAndExtensions".to_string(),
        format!("-Xcommon-sources={}", common_path.to_string_lossy()),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        common_path.to_string_lossy().into_owned(),
        platform_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        (code, common::reported(&reference)),
        (0, Vec::new()),
        "the reference compiler accepts the split source set"
    );
    let got = common::compile_and_run_files_with_stdlib(&[
        ("Common.kt", COMMON),
        ("Platform.kt", PLATFORM),
    ])
    .expect("krusty compiles and runs the split source set");
    assert_eq!(got, "OK");
}
