//! kotlinc maps a `LineNumberTable` entry to every sub-expression that starts on a new source line,
//! not only to statement roots. A call written over several lines gets one entry per argument.
//!
//! krusty emitted one entry for the whole body, so any multi-line call — which is most calls in
//! formatted Kotlin — carried a line table a debugger reads differently from kotlinc's. It is the
//! most common single difference in the corpus this is measured against, present in roughly three
//! quarters of the classes that differ at all.
use super::common;

/// The same source through both compilers, disassembled: `(kotlinc, krusty)`.
///
/// A line table is not a byte comparison — the rest of the class need not match for this contract
/// to be testable, and on a multi-line call it does not yet.
fn disassemble_both(name: &str, src: &str, class: &str) -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process(src, name, &[common::stdlib_jar()], None)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }
    let reference = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &reference_dir.to_string_lossy(),
        class,
    ])?;
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &krusty_dir.to_string_lossy(),
        class,
    ])?;
    let _ = std::fs::remove_dir_all(dir);
    Some((reference, krusty))
}

fn method_lines(text: &str, signature: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(signature))
        .skip_while(|line| !line.starts_with("LineNumberTable"))
        .skip(1)
        .take_while(|line| line.starts_with("line "))
        .map(str::to_string)
        .collect()
}

/// The arguments of a multi-line call each map to their own line, and an argument on the line
/// already in effect adds nothing.
#[test]
fn a_multi_line_calls_arguments_each_map_to_their_line() {
    let src = "class P(val a: Int, val b: String, val c: Int)\n\
               \n\
               fun make(x: Int, y: String): P = P(\n\
               \x20   a = x,\n\
               \x20   b = y,\n\
               \x20   c = x + 1,\n\
               )\n";
    let Some(built) = disassemble_both("ExpressionLines", src, "ExpressionLinesKt") else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let argument_lines = |entries: Vec<String>| {
        entries
            .into_iter()
            .filter(|entry| {
                entry.starts_with("line 4:")
                    || entry.starts_with("line 5:")
                    || entry.starts_with("line 6:")
            })
            .collect::<Vec<_>>()
    };
    let want = argument_lines(method_lines(&built.0, "make(int, java.lang.String)"));
    assert_eq!(want.len(), 3, "kotlinc maps all three arguments: {want:?}");
    let got = argument_lines(method_lines(&built.1, "make(int, java.lang.String)"));
    assert_eq!(
        got, want,
        "complete argument-line projection, including bytecode offsets"
    );
}

/// With the dispatch and return rules too, the whole class matches: the arguments map to their own
/// lines, the constructor call maps BACK to the line it is written on, and the `return` maps to
/// where the expression ends.
#[test]
fn a_multi_line_construction_is_byte_identical_to_kotlinc() {
    let src = "class P(val a: Int, val b: String, val c: Int)\n\
               \n\
               fun make(x: Int, y: String): P = P(\n\
               \x20   a = x,\n\
               \x20   b = y,\n\
               \x20   c = x + 1,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "MultiLineConstruction",
        src,
        "MultiLineConstructionKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("MultiLineConstructionKt byte-identical to kotlinc");
}

/// A single-line call adds no entries: the rules are about a line CHANGING, not about calls.
#[test]
fn a_single_line_call_is_unchanged() {
    let src = "class Q(val a: Int)\n\
               \n\
               fun one(): Q = Q(1)\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "SingleLineCall",
        src,
        "SingleLineCallKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("SingleLineCallKt byte-identical to kotlinc");
}

/// An explicit return keeps the call line in effect; only the return synthesized for an expression
/// body maps to the returned expression's closing line.
#[test]
fn an_explicit_multi_line_return_is_byte_identical_to_kotlinc() {
    let src = "class R(val a: Int, val b: String, val c: Int)\n\
               \n\
               fun make(x: Int, y: String): R {\n\
               \x20   return R(\n\
               \x20       a = x,\n\
               \x20       b = y,\n\
               \x20       c = x + 1,\n\
               \x20   )\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "ExplicitMultiLineReturn",
        src,
        "ExplicitMultiLineReturnKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("explicit multi-line return byte-identical to kotlinc");
}
/// A finalizer changes the active line while the return value is parked in a local. The eventual
/// return instruction must restore the explicit return's source line, not inherit the finalizer's.
///
/// The finalizer here is `println`, an `inline` stdlib function. krusty's splice of an inline body
/// parks the argument in a local where kotlinc keeps it on the stack and swaps, so the splice is
/// seven bytes longer and every offset after it differs for a reason this test is not about. The
/// LINE SEQUENCE is compared exactly against kotlinc, which is the contract — and it is what was
/// wrong before the fix, the restored return line being absent rather than misplaced.
/// `an_explicit_return_after_a_plain_call_finally_restores_its_line` covers the same contract with
/// the offsets included.
#[test]
fn an_explicit_return_after_finally_restores_its_line() {
    let src = "class FinallySink {\n\
               \x20   fun take(a: Int, b: String, c: Int): Int = a + c\n\
               \x20   fun run(x: Int, y: String): Int {\n\
               \x20       try {\n\
               \x20           return take(\n\
               \x20               a = x,\n\
               \x20               b = y,\n\
               \x20               c = x + 1,\n\
               \x20           )\n\
               \x20       } finally {\n\
               \x20           println(\"done\")\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let Some((reference, krusty)) = disassemble_both("ReturnAfterFinally", src, "FinallySink")
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let lines_only = |entries: Vec<String>| {
        entries
            .into_iter()
            .map(|entry| {
                entry
                    .split(':')
                    .next()
                    .expect("a LineNumberTable row is `line N: offset`")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let want = lines_only(method_lines(&reference, "run(int, java.lang.String)"));
    assert_eq!(
        want,
        [
            "line 4", "line 5", "line 6", "line 7", "line 8", "line 5", "line 11", "line 5",
            "line 11"
        ],
        "kotlinc's own line sequence, spelled out so a reference change is visible here"
    );
    assert_eq!(
        lines_only(method_lines(&krusty, "run(int, java.lang.String)")),
        want,
        "complete run line sequence"
    );
}

/// The same contract with the offsets included, the finalizer calling a declared method so that no
/// inline splice sits between the entries under test.
#[test]
fn an_explicit_return_after_a_plain_call_finally_restores_its_line() {
    let src = "class PlainFinallySink {\n\
               \x20   fun take(a: Int, b: String, c: Int): Int = a + c\n\
               \x20   fun note() {}\n\
               \x20   fun run(x: Int, y: String): Int {\n\
               \x20       try {\n\
               \x20           return take(\n\
               \x20               a = x,\n\
               \x20               b = y,\n\
               \x20               c = x + 1,\n\
               \x20           )\n\
               \x20       } finally {\n\
               \x20           note()\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let Some((reference, krusty)) =
        disassemble_both("ReturnAfterPlainFinally", src, "PlainFinallySink")
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_lines(&reference, "run(int, java.lang.String)");
    assert!(!want.is_empty(), "kotlinc run LineNumberTable");
    assert_eq!(
        method_lines(&krusty, "run(int, java.lang.String)"),
        want,
        "complete run LineNumberTable"
    );
}

/// An ordinary METHOD call, not just a constructor: the same rule at every callee kind.
#[test]
fn a_multi_line_method_call_is_byte_identical_to_kotlinc() {
    let src = "class Sink {\n\
               \x20   fun take(a: Int, b: String, c: Int): Int = a + c\n\
               }\n\
               \n\
               fun run(s: Sink, x: Int, y: String): Int = s.take(\n\
               \x20   a = x,\n\
               \x20   b = y,\n\
               \x20   c = x + 1,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "MultiLineMethodCall",
        src,
        "MultiLineMethodCallKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("MultiLineMethodCallKt byte-identical to kotlinc");
}

/// A defaulted member dispatch uses its static `$default` entry point but keeps the source call's
/// line contract.
#[test]
fn a_multi_line_defaulted_method_call_is_byte_identical_to_kotlinc() {
    let src = "class DefaultSink {\n\
               \x20   fun take(a: Int, b: Int = 2): Int = a + b\n\
               }\n\
               \n\
               fun run(s: DefaultSink, x: Int): Int = s.take(\n\
               \x20   a = x,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "MultiLineDefaultedMethodCall",
        src,
        "MultiLineDefaultedMethodCallKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("defaulted multi-line method call byte-identical to kotlinc");
}
