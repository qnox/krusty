//! A function value passed where a different function type is expected is converted the way kotlinc
//! converts it: a regular value to a `suspend` function type (suspend conversion) and, under
//! `+UnitConversionsOnArbitraryExpressions` (KT-84393, from 2.4.20), a value returning non-`Unit`
//! to one returning `Unit` (unit conversion). Both, alone or together, compile to one carrier: a
//! synthetic `FunctionReferenceImpl` bound to the value, reflecting `Intrinsics.Kotlin`'s
//! `suspendConversion<N>`, whose `invoke` calls the value's `FunctionN.invoke` (discarding its result
//! for a unit conversion). The carrier's class is named in the local-class sequence of the scope
//! it is written in, and `N` counts the conversions of its innermost callable.
use super::common;

const UNIT_CONVERSIONS: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n";

/// Compile `source` with both compilers and require `class` to be kotlinc's: its header, every
/// member with its code and debug tables, and its `@Metadata`.
fn assert_class_matches_kotlinc(
    stem: &str,
    source: &str,
    class: &str,
) -> common::ReferenceComparison {
    let comparison = common::compare_with_kotlinc_plugin(
        stem,
        source,
        class,
        &[common::stdlib_jar()],
        "17",
        &common::language_directives::kotlinc_args(source),
    )
    .expect("reference kotlinc and javap are provisioned");
    let header = |bytes: &[u8]| {
        let info = krusty::jvm::classreader::parse_class(bytes).expect("a readable class file");
        (
            info.access,
            info.this_class,
            info.super_class,
            info.interfaces(),
            info.signature.clone(),
        )
    };
    assert_eq!(
        header(&comparison.krusty_bytes),
        header(&comparison.reference_bytes),
        "{class}: kotlinc's class header"
    );
    assert_eq!(
        common::member_table(&comparison.krusty_bytes),
        common::member_table(&comparison.reference_bytes),
        "{class}: kotlinc's member table"
    );
    assert_eq!(
        common::member_blocks(&comparison.krusty),
        common::member_blocks(&comparison.reference),
        "{class}: kotlinc's members"
    );
    assert_eq!(
        common::raw_kotlin_metadata(&comparison.krusty_bytes),
        common::raw_kotlin_metadata(&comparison.reference_bytes),
        "{class}: kotlinc's @Metadata"
    );
    comparison
}

/// Require the carriers of `source` and the method holding their use sites (`use_site`, a javap
/// header line of `facade`) to be kotlinc's, then run `box()` under both compilers.
fn assert_conversion_matches_kotlinc(
    stem: &str,
    source: &str,
    carriers: &[&str],
    facade: &str,
    use_site: &str,
) {
    for carrier in carriers {
        assert_class_matches_kotlinc(stem, source, carrier);
    }
    let comparison = common::compare_with_kotlinc_plugin(
        stem,
        source,
        facade,
        &[common::stdlib_jar()],
        "17",
        &common::language_directives::kotlinc_args(source),
    )
    .expect("reference kotlinc and javap are provisioned");
    let reference = common::method_block(&comparison.reference, use_site);
    assert!(!reference.is_empty(), "kotlinc wrote {use_site}");
    assert_eq!(
        common::method_block(&comparison.krusty, use_site),
        reference,
        "{facade}: kotlinc's use site"
    );
    common::expect_box_same_as_kotlinc(source, &format!("{stem}Run"));
}

/// Require krusty to report exactly kotlinc's errors for `source`, entry for entry, as recorded per
/// Kotlin version, and answer whether kotlinc accepted it. kotlinc 2.4.0 and 2.4.10 know
/// `UnitConversionsOnArbitraryExpressions` but convert no arbitrary value (KT-84393), so the same
/// program is rejected there and compiled from 2.4.20 on.
fn accepted_like_kotlinc(source: &str) -> bool {
    let sources = [("Main.kt", source)];
    let expected = common::recorded(|| {
        common::reference_error_ledger(&sources, &common::language_directives::kotlinc_args(source))
    });
    assert_eq!(
        common::krusty_error_ledger(&sources),
        expected,
        "krusty's ledger against kotlinc {}",
        krusty::kotlin_version::target()
    );
    expected.is_empty()
}

const UNIT_SOURCE: &str = "fun consume(f: () -> Unit) {\n\
    \x20   f()\n\
    }\n\
    var effects = \"\"\n\
    fun produce(): String {\n\
    \x20   effects += \"OK\"\n\
    \x20   return \"ignored\"\n\
    }\n\
    fun pass(g: () -> String) {\n\
    \x20   consume(g)\n\
    }\n\
    fun box(): String {\n\
    \x20   pass { produce() }\n\
    \x20   return effects\n\
    }\n";

#[test]
fn unit_conversion_of_a_value_is_kotlincs_bound_carrier() {
    let source = format!("{UNIT_CONVERSIONS}{UNIT_SOURCE}");
    if accepted_like_kotlinc(&source) {
        assert_conversion_matches_kotlinc(
            "UnitValue",
            &source,
            &["UnitValueKt$pass$1"],
            "UnitValueKt",
            "public static final void pass(kotlin.jvm.functions.Function0<java.lang.String>);",
        );
    }
}

const UNIT_AND_SUSPEND_SOURCE: &str = "import kotlin.coroutines.*\n\
    fun launch(f: suspend () -> Unit) {\n\
    \x20   f.startCoroutine(Completion)\n\
    }\n\
    object Completion : Continuation<Unit> {\n\
    \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
    \x20   override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
    }\n\
    var effects = \"\"\n\
    fun pass(g: () -> String) {\n\
    \x20   launch(g)\n\
    }\n\
    fun box(): String {\n\
    \x20   pass { effects += \"OK\"; \"ignored\" }\n\
    \x20   return effects\n\
    }\n";

#[test]
fn unit_and_suspend_conversion_of_a_value_is_one_carrier() {
    let source = format!("{UNIT_CONVERSIONS}{UNIT_AND_SUSPEND_SOURCE}");
    if accepted_like_kotlinc(&source) {
        assert_conversion_matches_kotlinc(
            "UnitSuspendValue",
            &source,
            &["UnitSuspendValueKt$pass$1"],
            "UnitSuspendValueKt",
            "public static final void pass(kotlin.jvm.functions.Function0<java.lang.String>);",
        );
    }
}

const SUBTYPE_SOURCE: &str = "abstract class Source : () -> String {\n\
    \x20   override fun invoke(): String {\n\
    \x20       effects += \"OK\"\n\
    \x20       return \"ignored\"\n\
    \x20   }\n\
    }\n\
    fun consume(f: () -> Unit) {\n\
    \x20   f()\n\
    }\n\
    var effects = \"\"\n\
    fun box(): String {\n\
    \x20   val source: Source = object : Source() {}\n\
    \x20   consume(source)\n\
    \x20   return effects\n\
    }\n";

/// A value whose class implements the function type is converted as that function type: the
/// carrier reflects and casts to `Function0`, and is named beside the anonymous object's class.
#[test]
fn unit_conversion_of_a_function_subtype_converts_its_function_supertype() {
    let source = format!("{UNIT_CONVERSIONS}{SUBTYPE_SOURCE}");
    if accepted_like_kotlinc(&source) {
        assert_conversion_matches_kotlinc(
            "SubtypeValue",
            &source,
            &["SubtypeValueKt$box$1"],
            "SubtypeValueKt",
            "public static final java.lang.String box();",
        );
    }
}

const SUSPEND_SOURCE: &str = "import kotlin.coroutines.*\n\
    fun launch(f: suspend () -> String) {\n\
    \x20   f.startCoroutine(Completion)\n\
    }\n\
    fun launchUnit(f: suspend () -> Unit) {\n\
    \x20   f.startCoroutine(UnitCompletion)\n\
    }\n\
    var effects = \"\"\n\
    object Completion : Continuation<String> {\n\
    \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
    \x20   override fun resumeWith(result: Result<String>) { effects += result.getOrThrow() }\n\
    }\n\
    object UnitCompletion : Continuation<Unit> {\n\
    \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
    \x20   override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
    }\n\
    fun pass(text: () -> String, effect: () -> Unit) {\n\
    \x20   launch(text)\n\
    \x20   launchUnit(effect)\n\
    }\n\
    fun box(): String {\n\
    \x20   pass({ \"O\" }, { effects += \"K\" })\n\
    \x20   return effects\n\
    }\n";

/// Suspend conversion of a value needs no language feature and shares the carrier; the
/// conversions of one callable are numbered in source order.
#[test]
fn suspend_conversion_of_a_value_is_kotlincs_bound_carrier() {
    assert_conversion_matches_kotlinc(
        "SuspendValue",
        SUSPEND_SOURCE,
        &["SuspendValueKt$pass$1", "SuspendValueKt$pass$2"],
        "SuspendValueKt",
        "public static final void pass(kotlin.jvm.functions.Function0<java.lang.String>, kotlin.jvm.functions.Function0<kotlin.Unit>);",
    );
}

const NAMING_SOURCE: &str = "fun consume(f: () -> Unit) { f() }\n\
    fun counted(f: () -> Unit): Int { f(); return 1 }\n\
    fun later(f: suspend () -> String) {}\n\
    fun names(g: () -> String) {\n\
    \x20   consume(g)\n\
    \x20   later { \"lambda\" }\n\
    \x20   consume(g)\n\
    \x20   fun local() { consume(g) }\n\
    \x20   later(g)\n\
    \x20   val stored = counted(g)\n\
    \x20   consume(g)\n\
    }\n\
    class Holder(g: () -> String) {\n\
    \x20   init { consume(g) }\n\
    \x20   val first = counted(g)\n\
    \x20   init { consume(g) }\n\
    \x20   constructor(g: () -> String, unused: Int) : this(g) { consume(g) }\n\
    \x20   val computed: Int get() { val h = { \"\" }; return counted(h) }\n\
    }\n";

/// The class name and reflected `suspendConversion<N>` of every conversion carrier in `classes`.
fn conversion_carriers(classes: &[(String, Vec<u8>)]) -> Vec<(String, String)> {
    let mut carriers = classes
        .iter()
        .filter_map(|(name, bytes)| {
            let text = String::from_utf8_lossy(bytes);
            let start = text.find("suspendConversion")?;
            let reflected = text[start..]
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect::<String>();
            Some((name.clone(), reflected))
        })
        .collect::<Vec<_>>();
    carriers.sort();
    carriers
}

/// A conversion takes the next position of the local-class sequence it is written in, shifting
/// every later lambda and reference there, and `N` restarts in each function, local function,
/// property initializer, accessor and secondary constructor, while a class's `init` blocks share one.
#[test]
fn conversion_carriers_take_kotlincs_names() {
    let source = format!("{UNIT_CONVERSIONS}{NAMING_SOURCE}");
    if accepted_like_kotlinc(&source) {
        assert_carrier_names_match_kotlinc(&source);
    }
}

/// Compile `source` with both compilers and require krusty's conversion carriers to be kotlinc's,
/// class name and reflected `suspendConversion<N>` alike.
fn assert_carrier_names_match_kotlinc(source: &str) {
    let dir = common::scratch_dir().expect("scratch directory");
    let source_path = dir.join("Names.kt");
    std::fs::write(&source_path, source).expect("write fixture");
    let mut arguments = vec![
        "-d".to_string(),
        dir.join("ref").to_string_lossy().into_owned(),
    ];
    arguments.extend(common::language_directives::kotlinc_args(source));
    arguments.push(source_path.to_string_lossy().into_owned());
    let (code, stderr) =
        common::kotlinc_compile(&arguments).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let reference = std::fs::read_dir(dir.join("ref"))
        .expect("kotlinc output")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path
                .file_name()?
                .to_str()?
                .strip_suffix(".class")?
                .to_string();
            Some((name, std::fs::read(&path).ok()?))
        })
        .collect::<Vec<_>>();
    let _ = std::fs::remove_dir_all(dir);
    let ours = common::compile_in_process_metadata_cp(source, "Names", &[common::stdlib_jar()])
        .expect("krusty compiles the conversions");
    let expected = conversion_carriers(&reference);
    assert_eq!(expected.len(), 11, "kotlinc's carriers: {expected:?}");
    assert_eq!(conversion_carriers(&ours), expected);
}

/// Without the language feature a value returning `String` is not a `() -> Unit`, and a regular
/// value converted to a suspend type is reported as that suspend type.
#[test]
fn unit_conversion_of_a_value_requires_the_language_feature() {
    const SOURCE: &str = "fun consume(f: () -> Unit) { f() }\n\
        fun launch(f: suspend () -> Unit) {}\n\
        fun pass(g: () -> String) {\n\
        \x20   consume(g)\n\
        \x20   launch(g)\n\
        }\n";
    common::assert_errors_match_kotlinc(&[("Main.kt", SOURCE)], &[]);
}

/// kotlinc converts a function value only where it is passed as a call argument. Assigned to a
/// variable of a `Unit`-returning or `suspend` function type it is a type mismatch, with or without
/// the language feature.
#[test]
fn a_function_value_is_not_converted_when_assigned() {
    const SOURCE: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n\
        fun assign(g: () -> String) {\n\
        \x20   var unit: () -> Unit = {}\n\
        \x20   unit = g\n\
        \x20   var suspending: suspend () -> String = { \"\" }\n\
        \x20   suspending = g\n\
        \x20   var both: suspend () -> Unit = {}\n\
        \x20   both = g\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SOURCE)],
        &common::language_directives::kotlinc_args(SOURCE),
    );
}

/// A function value returned where the declared result is a `Unit`-returning or `suspend` function
/// type is a return type mismatch, whether returned explicitly or as an expression body.
#[test]
fn a_function_value_is_not_converted_when_returned() {
    const SOURCE: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n\
        fun unit(g: () -> String): () -> Unit {\n\
        \x20   return g\n\
        }\n\
        fun suspending(g: () -> String): suspend () -> String {\n\
        \x20   return g\n\
        }\n\
        fun both(g: () -> String): suspend () -> Unit = g\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SOURCE)],
        &common::language_directives::kotlinc_args(SOURCE),
    );
}

/// Neither a declaration's initializer nor a lambda's result converts a function value either; the
/// lambda's mismatch is reported at its result expression.
#[test]
fn a_function_value_is_not_converted_by_an_initializer_or_lambda_result() {
    const SOURCE: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n\
        fun declare(g: () -> String) {\n\
        \x20   val unit: () -> Unit = g\n\
        \x20   val suspending: suspend () -> String = g\n\
        }\n\
        fun produce(g: () -> String): () -> () -> Unit = { g }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SOURCE)],
        &common::language_directives::kotlinc_args(SOURCE),
    );
}

/// A named argument and a conditional argument convert like a positional value.
#[test]
fn named_and_conditional_arguments_convert_a_function_value() {
    const SOURCE: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n\
        var calls = 0\n\
        fun consume(f: () -> Unit) { f() }\n\
        fun box(): String {\n\
        \x20   val g: () -> String = { calls++; \"ignored\" }\n\
        \x20   val h: () -> String = { \"ignored\" }\n\
        \x20   consume(f = g)\n\
        \x20   consume(if (calls > 0) g else h)\n\
        \x20   return if (calls == 2) \"OK\" else \"calls: $calls\"\n\
        }\n";
    if accepted_like_kotlinc(SOURCE) {
        common::expect_box_same_as_kotlinc(SOURCE, "ArgumentFormsRun");
    }
}
