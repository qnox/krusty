//! The class-file facts a `try` produces: its debug lines, and its protected ranges.
//!
//! A `try` is three separate debug-line facts, none of which krusty recorded.
//!
//! kotlinc opens every protected region with a `nop` carrying the `try` keyword's own line, so the
//! exception table's `from` is an instruction belonging to the region rather than the body's first
//! one. A `finally` is then emitted twice — inline on the normal path and again in the catch-all
//! handler — and the handler's entry belongs to the finalizer copy it introduces, so it opens on the
//! finalizer's first line. Finally, a `return` inside a `try` parks its value while the finalizer
//! runs, which changes the line in effect, so the return instruction restores the return's own line.
use super::common;

/// The same source through both compilers, disassembled: `(kotlinc, krusty)`.
fn disassemble_both(name: &str, src: &str, class: &str) -> (String, String) {
    let dir = common::scratch_dir()
        .unwrap_or_else(|| panic!("{name}: no scratch directory for the differential"));
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference dir");
    std::fs::create_dir_all(&krusty_dir).expect("output dir");
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        // Both sides must be one target: krusty emits its default major 52 here, so kotlinc
        // compiles for 1.8 too. A target difference forks codegen (indy string concatenation, for
        // one), and two differently-targeted classes are not an oracle for each other.
        "-jvm-target".to_string(),
        "1.8".to_string(),
        source.to_string_lossy().into_owned(),
    ])
    .unwrap_or_else(|| panic!("{name}: reference kotlinc unavailable under the test harness"));
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let jdk = common::jdk_modules();
    let classes =
        common::compile_in_process(src, name, &[common::stdlib_jar()], Some(jdk.as_path()))
            .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("class dir");
        }
        std::fs::write(path, bytes).expect("write class");
    }
    let reference = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &reference_dir.to_string_lossy(),
        class,
    ])
    .unwrap_or_else(|| panic!("{name}: javap unavailable for the reference class"));
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &krusty_dir.to_string_lossy(),
        class,
    ])
    .unwrap_or_else(|| panic!("{name}: javap unavailable for the emitted class"));
    let _ = std::fs::remove_dir_all(dir);
    (reference, krusty)
}

/// The rows of a method's disassembly between two `javap` section headers.
fn method_section(text: &str, signature: &str, section: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(signature))
        .skip_while(|line| !line.starts_with(section))
        .skip(1)
        .take_while(|line| {
            !line.is_empty() && !line.ends_with(':') && !line.starts_with("Start  Length")
        })
        .map(str::to_string)
        .collect()
}

/// The offset-keyed rows of a `javap` section: the instructions under `Code:`, or the ranges under
/// `Exception table:`. Each section opens with a header line (`stack=…`, `from  to  target type`)
/// and ends at the first row that no longer starts with an offset.
fn numeric_rows(text: &str, signature: &str, section: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(signature))
        .skip_while(|line| !line.starts_with(section))
        .skip(1)
        .skip_while(|line| !line.starts_with(|c: char| c.is_ascii_digit()))
        .take_while(|line| line.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn method_lines(text: &str, signature: &str) -> Vec<String> {
    method_section(text, signature, "LineNumberTable")
        .into_iter()
        .filter(|line| line.starts_with("line "))
        .collect()
}

/// A `try`/`finally` whose body returns: the complete table, covering all three facts at once —
/// the `try` line on the opening `nop`, the finalizer's line on the handler entry, and the return's
/// own line restored at the return instruction after the finalizer changed it.
#[test]
fn a_try_finally_maps_every_line_like_kotlinc() {
    let src = "class Guarded {\n\
               \x20   fun step() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try {\n\
               \x20           return x + 1\n\
               \x20       } finally {\n\
               \x20           step()\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("TryFinallyLines", src, "Guarded");
    let want = method_lines(&reference, "int run(int)");
    assert_eq!(
        want,
        vec![
            "line 4: 0".to_string(),
            "line 5: 1".to_string(),
            "line 7: 5".to_string(),
            "line 5: 10".to_string(),
            "line 7: 11".to_string(),
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(method_lines(&krusty, "int run(int)"), want, "run lines");
}

/// The opening `nop` is part of the protected region, not merely a line anchor: the exception
/// table's `from` is its offset, so the region starts one instruction before the body.
#[test]
fn a_protected_region_starts_at_the_opening_nop() {
    let src = "class Caught {\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try {\n\
               \x20           return x + 1\n\
               \x20       } catch (e: Exception) {\n\
               \x20           return 0\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("TryCatchRegion", src, "Caught");
    let code = numeric_rows(&krusty, "int run(int)", "Code:");
    assert_eq!(
        code.first().map(String::as_str),
        Some("0: nop"),
        "krusty run code: {code:?}"
    );
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        numeric_rows(&reference, "int run(int)", "Exception table:"),
        "run exception table"
    );
}

/// A `try` written entirely on one line adds no entry beyond the one already in effect: the rules
/// are about a line CHANGING, and the `nop` is emitted either way.
#[test]
fn a_single_line_try_adds_no_entry() {
    let src = "class OneLine {\n\
               \x20   fun step() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try { return x + 1 } finally { step() }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("SingleLineTry", src, "OneLine");
    let want = method_lines(&reference, "int run(int)");
    assert_eq!(want, vec!["line 4: 0".to_string()], "kotlinc's own table");
    assert_eq!(method_lines(&krusty, "int run(int)"), want, "run lines");
}

/// A `return` out of a `try` inlines a copy of the finalizer in the MIDDLE of the protected region.
/// That copy must not be protected by the handler it belongs to: an exception raised while the
/// finalizer runs would re-enter the same handler and run the finalizer a second time. kotlinc
/// closes the region ahead of the copy and protects the handler's own entry separately.
#[test]
fn a_returning_finally_keeps_its_own_copy_out_of_its_region() {
    let src = "class Guarded {\n\
               \x20   fun step() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try {\n\
               \x20           return x + 1\n\
               \x20       } finally {\n\
               \x20           step()\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("TryFinallyRegion", src, "Guarded");
    let want = numeric_rows(&reference, "int run(int)", "Exception table:");
    assert_eq!(
        want,
        vec![
            "0     5    11   any".to_string(),
            "11    12    11   any".to_string()
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        want,
        "run exception table"
    );
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Code:"),
        numeric_rows(&reference, "int run(int)", "Code:"),
        "run code"
    );
}

/// The same rule where the body falls through instead of returning: the finalizer copy sits after
/// the body, so the region simply ends before it — but the handler's own entry is still protected.
#[test]
fn a_falling_through_finally_protects_its_handler_entry() {
    let src = "class Falls {\n\
               \x20   fun step() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       var k = 0\n\
               \x20       try {\n\
               \x20           k = x + 1\n\
               \x20       } finally {\n\
               \x20           step()\n\
               \x20       }\n\
               \x20       return k\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("TryFinallyFallThrough", src, "Falls");
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        numeric_rows(&reference, "int run(int)", "Exception table:"),
        "run exception table"
    );
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Code:"),
        numeric_rows(&reference, "int run(int)", "Code:"),
        "run code"
    );
}

/// An INNER finalizer's copy is ordinary code as far as the outer `try` is concerned: it stays
/// inside the outer region, and only the outer's own copies are cut out of it. A rule that excluded
/// every finalizer copy from every enclosing region would drop the outer's cover of the inner one.
#[test]
fn a_nested_finally_copy_stays_inside_the_outer_region() {
    let src = "class Nested {\n\
               \x20   fun a() {}\n\
               \x20   fun b() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try {\n\
               \x20           try {\n\
               \x20               return x + 1\n\
               \x20           } finally {\n\
               \x20               a()\n\
               \x20           }\n\
               \x20       } finally {\n\
               \x20           b()\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("NestedFinallyRegion", src, "Nested");
    let want = numeric_rows(&reference, "int run(int)", "Exception table:");
    assert_eq!(
        want,
        vec![
            "1     6    16   any".to_string(),
            "16    17    16   any".to_string(),
            "0    10    23   any".to_string(),
            "16    23    23   any".to_string(),
            "23    24    23   any".to_string(),
        ],
        "kotlinc's own table: the outer region covers the inner finalizer copy at 6..10"
    );
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        want,
        "run exception table"
    );
    // The handler temporaries are leased, so the outer handler reuses the dead inner one's slot and
    // its store keeps kotlinc's one-byte form. Comparing the code as well keeps that honest: a
    // regression there would move the table's offsets rather than its structure.
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Code:"),
        numeric_rows(&reference, "int run(int)", "Code:"),
        "run code"
    );
}

/// Kotlin runs a `finally` when control leaves its `try` by ANY route, not only by `return`.
/// `break` and `continue` jumped straight to their loop labels, so the finalizer never ran — a
/// silent wrong answer, not a byte difference.
///
/// The loop below leaves its `try` by `continue` on the first iteration and by `break` on the
/// second, so a correct compiler appends `F` twice.
#[test]
fn a_loop_transfer_out_of_a_try_runs_its_finally() {
    let src = "fun box(): String {\n\
               \x20   val sb = StringBuilder()\n\
               \x20   for (i in 0..1) {\n\
               \x20       try {\n\
               \x20           sb.append(\"T\")\n\
               \x20           if (i == 0) continue\n\
               \x20           break\n\
               \x20       } finally {\n\
               \x20           sb.append(\"F\")\n\
               \x20       }\n\
               \x20   }\n\
               \x20   return sb.toString()\n\
               }\n";
    let jdk = common::jdk_modules();
    let Some(out) = common::compile_and_run_box(
        src,
        "LoopTransferFinally",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    ) else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "TFTF");
}

/// A `finally` that a loop transfer leaves is inlined on that path too, so its copy must be cut out
/// of the protected region exactly as a `return`'s copy is. Compared against kotlinc rather than
/// pinned, so the rule stays tied to the reference compiler.
///
/// A `while` loop, not `for (i in a..b)`: a counted range loop spills its bound into a local where
/// kotlinc re-reads the parameter, which shifts every offset and would fail this comparison for a
/// reason that has nothing to do with protected regions.
#[test]
fn a_loop_transfer_finalizer_copy_leaves_the_protected_region() {
    let src = "class Looping {\n\
               \x20   fun step() {}\n\
               \x20   fun run(n: Int): Int {\n\
               \x20       var seen = 0\n\
               \x20       var i = 0\n\
               \x20       while (i < n) {\n\
               \x20           i += 1\n\
               \x20           try {\n\
               \x20               seen += i\n\
               \x20               continue\n\
               \x20           } finally {\n\
               \x20               step()\n\
               \x20           }\n\
               \x20       }\n\
               \x20       return seen\n\
               \x20   }\n\
               }\n";
    let Some((reference, krusty)) = disassemble_both("LoopTransferRegion", src, "Looping") else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        numeric_rows(&reference, "int run(int)", "Exception table:"),
        "run exception table"
    );
}
