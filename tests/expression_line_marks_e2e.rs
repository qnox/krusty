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
    let lines = |text: &str| {
        text.lines()
            .map(str::trim)
            .skip_while(|line| !line.contains("make(int, java.lang.String)"))
            .skip_while(|line| !line.starts_with("LineNumberTable"))
            .skip(1)
            .take_while(|line| line.starts_with("line "))
            .map(str::to_string)
            .collect::<Vec<_>>()
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
    let want = argument_lines(lines(&built.0));
    assert_eq!(want.len(), 3, "kotlinc maps all three arguments: {want:?}");
    let got = argument_lines(lines(&built.1));
    assert_eq!(
        got, want,
        "complete argument-line projection, including bytecode offsets"
    );
}
