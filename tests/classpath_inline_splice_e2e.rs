//! Which CLASSPATH inline functions krusty splices, and which it declines.
//!
//! A declined splice is not a miscompile: the library's real method exists for Java interop, so the
//! call links and runs. It is simply not what kotlinc emits, which inlines the body at every one of
//! these call sites. These are the shapes that decide it, pinned as emitted code rather than as
//! successful execution — a test that only ran the program would pass either way. The concatenating
//! shape is pinned BOTH ways: the emitted form, and a run, because an `invokedynamic` relocated
//! into another class fails at bootstrap LINKAGE, which no disassembly can observe.
//!
//! Every step here fails closed. A missing reference compiler, a failed krusty compile or an
//! unavailable JVM is a test failure, not a skip: this file is a repository-owned regression, and a
//! silent skip would report the splice as still working after it stopped.
use super::common;
use std::path::PathBuf;
use std::sync::OnceLock;

const LIB: &str = "package fixture\n\
    \n\
    suspend fun fetchInt(v: Int): Int = v\n\
    \n\
    inline fun twice(x: Int, block: (Int) -> Int): Int {\n\
    \x20   val y = x + 1\n\
    \x20   return block(y)\n\
    }\n\
    \n\
    inline fun tagPlain(tag: String, block: (String) -> String): String {\n\
    \x20   val prefix = \"[\" + tag + \"]\"\n\
    \x20   return block(prefix)\n\
    }\n\
    \n\
    suspend inline fun twiceSuspend(x: Int, block: (Int) -> Int): Int {\n\
    \x20   val y = x + 1\n\
    \x20   val got = fetchInt(y)\n\
    \x20   return block(got)\n\
    }\n";

const MAIN: &str = "import fixture.twice\n\
    import fixture.tagPlain\n\
    import fixture.twiceSuspend\n\
    \n\
    fun runPlainInt(n: Int): Int = twice(n) { it * 2 }\n\
    fun runConcat(t: String): String = tagPlain(t) { it + \"!\" }\n\
    suspend fun runSuspend(n: Int): Int = twiceSuspend(n) { it * 2 }\n\
    \n\
    fun box(): String {\n\
    \x20   val plain = runPlainInt(3)\n\
    \x20   if (plain != 8) return \"FAIL plain=$plain\"\n\
    \x20   val concat = runConcat(\"x\")\n\
    \x20   if (concat != \"[x]!\") return \"FAIL concat=$concat\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// The kotlinc `-jvm-target` BOTH sides compile for, and the class-file major it selects.
///
/// Pinned at 25 deliberately: string concatenation compiles to `invokedynamic
/// makeConcatWithConstants` only from target 9, and to `StringBuilder` calls below it. On a lower
/// target the concatenating body contains no `invokedynamic` at all and krusty splices it happily —
/// so an unpinned library would quietly test the opposite of the intent. The HOST must be built for
/// the same target: a relocated JVM-9+ bootstrap in a class advertising Java 8 is not a form either
/// compiler produces, and testing it would pin a contract that does not exist.
const TARGET: &str = "25";
const TARGET_MAJOR: u16 = 69;

/// The library, compiled once per test process by the reference compiler at [`TARGET`].
fn library() -> PathBuf {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let root = common::scratch_dir().expect("a scratch directory for the fixture library");
            let out = root.join("lib");
            std::fs::create_dir_all(&out).expect("create the library output directory");
            let source = root.join("Lib.kt");
            std::fs::write(&source, LIB).expect("write the library source");
            let (code, stderr) = common::kotlinc_compile(&[
                "-d".to_string(),
                out.to_string_lossy().into_owned(),
                "-jvm-target".to_string(),
                TARGET.to_string(),
                source.to_string_lossy().into_owned(),
            ])
            .expect("the reference compiler must be available to this regression");
            assert_eq!(code, 0, "kotlinc failed to build the library: {stderr}");
            out
        })
        .clone()
}

/// krusty's classes for [`MAIN`], compiled against that library AT THE SAME TARGET.
fn krusty_classes() -> Vec<(String, Vec<u8>)> {
    static CLASSES: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    CLASSES
        .get_or_init(|| {
            // The JDK jimage is on the compile classpath, not just the bootclasspath, because the
            // splice decides whether a relocated `BootstrapMethods` entry's members are reachable
            // and must be able to READ `java/lang/invoke/StringConcatFactory` to answer. Without
            // it the owner is simply unknown and the entry declines — correctly, but for a reason
            // the fixture invented rather than one the shape has.
            let classes = common::compile_in_process_metadata_cp_module_target(
                MAIN,
                "Main",
                &[library(), common::stdlib_jar(), common::jdk_modules()],
                "main",
                Some(TARGET_MAJOR),
            )
            .expect("krusty must compile the fixture against the reference-built library");
            let (_, bytes) = classes
                .iter()
                .find(|(internal, _)| internal == "MainKt")
                .expect("krusty must emit MainKt");
            // The target contract, asserted rather than assumed: the whole point of the fixture is
            // that both sides speak one `-jvm-target`, and a class-file major that drifted back to
            // the default (52) would make the comparison meaningless without failing anything.
            let major = u16::from_be_bytes([bytes[6], bytes[7]]);
            assert_eq!(
                major, TARGET_MAJOR,
                "MainKt must be emitted for -jvm-target {TARGET}"
            );
            classes
        })
        .clone()
}

/// `MainKt` disassembled, for the assertions about which bodies were spliced.
fn main_disassembly() -> String {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        let root = common::scratch_dir().expect("a scratch directory for the disassembly");
        let dir = root.join("out");
        for (internal, bytes) in &krusty_classes() {
            let path = dir.join(format!("{internal}.class"));
            std::fs::create_dir_all(path.parent().expect("a class file has a parent directory"))
                .expect("create the class output directory");
            std::fs::write(path, bytes).expect("write the emitted class");
        }
        // `-v` as well as `-c`: the `BootstrapMethods` table is a verbose-only section, and the
        // differential below compares it.
        common::javap(&["-p", "-c", "-v", "-cp", &dir.to_string_lossy(), "MainKt"])
            .expect("javap must be available to this regression")
    })
    .clone()
}

/// kotlinc's own `MainKt`, compiled against the same library at the same target — the reference
/// half of the differential below.
fn reference_disassembly() -> String {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        let root = common::scratch_dir().expect("a scratch directory for the reference");
        let out = root.join("ref");
        std::fs::create_dir_all(&out).expect("create the reference output directory");
        let source = root.join("Main.kt");
        std::fs::write(&source, MAIN).expect("write the reference source");
        let (code, stderr) = common::kotlinc_compile(&[
            "-d".to_string(),
            out.to_string_lossy().into_owned(),
            "-jvm-target".to_string(),
            TARGET.to_string(),
            "-classpath".to_string(),
            format!("{}:{}", library().display(), common::stdlib_jar().display()),
            source.to_string_lossy().into_owned(),
        ])
        .expect("the reference compiler must be available to this regression");
        assert_eq!(code, 0, "kotlinc failed to build the reference: {stderr}");
        common::javap(&["-p", "-c", "-v", "-cp", &out.to_string_lossy(), "MainKt"])
            .expect("javap must be available to this regression")
    })
    .clone()
}

/// A class's methods in DECLARATION order, as javap prints their signatures.
fn methods(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(");") && !line.starts_with("descriptor:"))
        .map(str::to_string)
        .collect()
}

/// The `BootstrapMethods` table in order: each entry's handle and its static arguments, with
/// constant-pool indices erased because their numbering is an emission-order artifact.
fn bootstrap_methods(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("BootstrapMethods:"))
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split_whitespace()
                .filter(|word| !word.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The complete ordered method list and `BootstrapMethods` table, against kotlinc's own, at one
/// target.
///
/// Both sides are spelled out rather than asserted equal, because they are NOT equal and this is
/// where the difference is recorded rather than described.
///
/// The spliced shapes agree: `runPlainInt`, `runConcat` and `box` carry no lambda implementation
/// method on either side, so the bodies AND the lambdas passed to them were inlined. What remains
/// is the DECLINED case — `runSuspend`, whose `suspend inline` callee is spliced after suspend
/// lowering has already built the state machine — and its lambda is the one method krusty has that
/// kotlinc does not, along with the `LambdaMetafactory` entry that binds it.
///
/// Pinning both sides means neither direction can move silently: a splice that regressed to a real
/// call and a splice that grew to cover the suspend case both fail this test.
#[test]
fn the_emitted_shape_against_kotlincs_own_at_the_same_target() {
    assert_eq!(
        methods(&reference_disassembly()),
        vec![
            "public static final int runPlainInt(int);",
            "public static final java.lang.String runConcat(java.lang.String);",
            "public static final java.lang.Object runSuspend(int, kotlin.coroutines.Continuation<? super java.lang.Integer>);",
            "public static final java.lang.String box();",
        ],
        "the reference emits no lambda implementation methods at all"
    );
    assert_eq!(
        methods(&main_disassembly()),
        vec![
            "public static final int runPlainInt(int);",
            "public static final java.lang.String runConcat(java.lang.String);",
            "public static final java.lang.Object runSuspend(int, kotlin.coroutines.Continuation<? super java.lang.Integer>);",
            "public static final java.lang.String box();",
            "private static final int runSuspend$lambda$0(int);",
        ],
        "the one extra method is the DECLINED suspend splice's lambda"
    );

    // The tables, by what each entry's handle names. Counting the handles rather than comparing
    // the tables verbatim is deliberate: the recipe strings are the SAME on both sides but their
    // order follows emission, and pinning that order here would assert an artifact.
    let handles =
        |rows: Vec<String>, factory: &str| rows.iter().filter(|row| row.contains(factory)).count();
    let reference = bootstrap_methods(&reference_disassembly());
    assert_eq!(
        handles(reference.clone(), "StringConcatFactory"),
        4,
        "the reference has four concatenations — the library body's, the lambda's, and the two \
         `box()` reports:\n{reference:#?}"
    );
    assert_eq!(
        handles(reference.clone(), "LambdaMetafactory"),
        0,
        "and binds no lambda through a bootstrap at all:\n{reference:#?}"
    );

    let krusty = bootstrap_methods(&main_disassembly());
    assert_eq!(
        handles(krusty.clone(), "StringConcatFactory"),
        4,
        "krusty has all four too — the two relocated out of the library body and its lambda, and \
         the two compiled from `box()`'s own templates:\n{krusty:#?}"
    );
    assert_eq!(
        handles(krusty.clone(), "LambdaMetafactory"),
        1,
        "and binds exactly one lambda: the one whose splice DECLINED:\n{krusty:#?}"
    );
}

/// Whether `method`'s body still calls `callee` — i.e. the splice was declined.
fn calls(text: &str, method: &str, callee: &str) -> bool {
    text.lines()
        .skip_while(|line| !line.contains(method))
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("public"))
        .any(|line| line.contains(callee))
}

/// The control: an ordinary classpath inline function with no `invokedynamic` in its body IS
/// spliced, so nothing else here can be blamed on classpath inlining generally.
#[test]
fn an_ordinary_classpath_inline_body_is_spliced() {
    let text = main_disassembly();
    assert!(
        !calls(&text, "int runPlainInt(int)", "fixture/LibKt.twice"),
        "expected the body to be spliced:\n{text}"
    );
}

/// String concatenation compiles to `invokedynamic makeConcatWithConstants` on JVM target 9 and
/// above. Its bootstrap reaches nothing the host may not reference, so the entry can be re-interned
/// in the host and the body splices.
#[test]
fn a_classpath_inline_body_that_concatenates_is_spliced() {
    let text = main_disassembly();
    assert!(
        !calls(
            &text,
            "java.lang.String runConcat(java.lang.String)",
            "fixture/LibKt.tagPlain"
        ),
        "expected the body to be spliced:\n{text}"
    );
    assert!(
        text.contains("makeConcatWithConstants"),
        "expected the relocated bootstrap in the host:\n{text}"
    );
}

/// The relocated bootstrap LINKS. A `BootstrapMethods` entry re-interned into another class is
/// resolved the first time its `invokedynamic` executes, so a handle or static argument the host
/// may not reference throws `BootstrapMethodError` at that instruction and at no earlier point —
/// invisible to every structural comparison. This runs it.
#[test]
fn the_relocated_concatenation_bootstrap_links_and_runs() {
    let classes = krusty_classes();
    let box_class = classes
        .iter()
        .map(|(internal, _)| internal.as_str())
        .find(|internal| *internal == "MainKt")
        .expect("krusty must emit MainKt");
    let output = common::run_box(&classes, box_class, &[library(), common::stdlib_jar()])
        .expect("a JVM must be available to this regression");
    assert_eq!(output, "OK", "the spliced bodies must produce their values");
}

/// A `suspend inline` function declines for a different reason and with no `invokedynamic` in its
/// body at all: a classpath body is spliced from bytecode at emit, after suspend lowering has
/// already built the state machine, so there is nowhere left to put a suspending body. This is the
/// shape the measured corpus is built from.
#[test]
fn a_classpath_suspend_inline_body_declines() {
    let text = main_disassembly();
    assert!(
        calls(&text, "runSuspend", "fixture/LibKt.twiceSuspend"),
        "expected a real call, the splice having declined:\n{text}"
    );
}
