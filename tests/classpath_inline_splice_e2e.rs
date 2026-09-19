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
            let classes = common::compile_in_process_metadata_cp_module_target(
                MAIN,
                "Main",
                &[library(), common::stdlib_jar()],
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
        common::javap(&["-p", "-c", "-cp", &dir.to_string_lossy(), "MainKt"])
            .expect("javap must be available to this regression")
    })
    .clone()
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
