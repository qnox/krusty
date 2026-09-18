//! Which CLASSPATH inline functions krusty splices, and which it declines.
//!
//! A declined splice is not a miscompile: the library's real method exists for Java interop, so the
//! call links and runs. It is simply not what kotlinc emits, which inlines the body at every one of
//! these call sites. These are the shapes that decide it, pinned as emitted code rather than as
//! successful execution — a test that only ran the program would pass either way.
use super::common;

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
    suspend fun runSuspend(n: Int): Int = twiceSuspend(n) { it * 2 }\n";

/// krusty's `MainKt`, disassembled, compiled against a kotlinc-built library.
///
/// The library's JVM target is pinned at 25 deliberately: string concatenation compiles to
/// `invokedynamic makeConcatWithConstants` only from target 9, and to `StringBuilder` calls below
/// it. On a lower target the concatenating body contains no `invokedynamic` at all and krusty
/// splices it happily — so an unpinned library would quietly test the opposite of the intent.
fn krusty_main() -> Option<String> {
    let root = common::scratch_dir()?;
    let library = root.join("lib");
    std::fs::create_dir_all(&library).ok()?;
    let source = root.join("Lib.kt");
    std::fs::write(&source, LIB).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed to build the library: {stderr}");
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        MAIN,
        "Main",
        &[library, common::stdlib_jar()],
        Some(jdk.as_path()),
    )?;
    let dir = root.join("out");
    std::fs::create_dir_all(&dir).ok()?;
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }
    let text = common::javap(&["-p", "-c", "-cp", &dir.to_string_lossy(), "MainKt"]);
    let _ = std::fs::remove_dir_all(root);
    text
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
    let Some(text) = krusty_main() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert!(
        !calls(&text, "int runPlainInt(int)", "fixture/LibKt.twice"),
        "expected the body to be spliced:\n{text}"
    );
}

/// String concatenation compiles to `invokedynamic makeConcatWithConstants` on JVM target 9 and
/// above. Its bootstrap is self-contained — a recipe string and constants — so the entry can be
/// re-interned in the host and the body splices.
#[test]
fn a_classpath_inline_body_that_concatenates_is_spliced() {
    let Some(text) = krusty_main() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
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

/// A `suspend inline` function declines for a different reason and with no `invokedynamic` in its
/// body at all: a classpath body is spliced from bytecode at emit, after suspend lowering has
/// already built the state machine, so there is nowhere left to put a suspending body. This is the
/// shape the measured corpus is built from.
#[test]
fn a_classpath_suspend_inline_body_declines() {
    let Some(text) = krusty_main() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert!(
        calls(&text, "runSuspend", "fixture/LibKt.twiceSuspend"),
        "expected a real call, the splice having declined:\n{text}"
    );
}
