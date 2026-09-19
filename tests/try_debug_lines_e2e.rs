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

/// The complete `LocalVariableTable` of one method, in printed order.
///
/// `columns` says how much of each row to keep: the whole row where both compilers agree byte for
/// byte, or `slot name descriptor` where they do not and the offsets would report a difference this
/// projection is not about.
fn local_variable_table(
    text: &str,
    signature: &str,
    columns: std::ops::Range<usize>,
) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(signature))
        .skip_while(|line| !line.starts_with("LocalVariableTable"))
        .skip(2)
        .take_while(|line| line.starts_with(|c: char| c.is_ascii_digit()))
        .map(|line| {
            line.split_whitespace()
                .skip(columns.start)
                .take(columns.len())
                .collect::<Vec<_>>()
                .join(" ")
        })
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

/// A `finally` that contains a `try`/`catch` of its own records StackMapTable frames WHILE the
/// outer handler's caught exception is still parked — it is re-raised after the finalizer runs. So
/// that slot has to be typed in those frames, or the trailing `aload; athrow` reads what the
/// verifier calls `top` and the class does not load.
///
/// The parked exception is a backend temporary and holds a lease for exactly that span. Comparing
/// the complete frame list against kotlinc is what pins it: a missing lease shows up there before
/// it shows up as a verify error.
#[test]
fn a_finally_with_its_own_handler_types_the_parked_exception() {
    let src = "class Parked {\n\
               \x20   fun risky(): Int = 1\n\
               \x20   fun note() {}\n\
               \x20   fun run(x: Int): Int {\n\
               \x20       try {\n\
               \x20           return x + 1\n\
               \x20       } finally {\n\
               \x20           try {\n\
               \x20               risky()\n\
               \x20           } catch (e: Exception) {\n\
               \x20               note()\n\
               \x20           }\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("ParkedExceptionFrames", src, "Parked");
    // Complete, in kotlinc's order, offsets included. The guarded ranges used to be compared by
    // catch TYPE alone and the frames with their `top` padding stripped, because krusty reserved
    // this `try`'s two slots — the parked return value and the parked exception — where they were
    // first used rather than where the `try` opens, so the finalizer's own locals sat underneath
    // them and everything the `try` parks moved one slot up. The copies were also three bytes
    // longer for a `goto` to the next instruction. Both are closed, so nothing here is projected
    // away.
    let table = |text: &str| numeric_rows(text, "int run(int)", "Exception table:");
    let want = table(&reference);
    assert_eq!(
        want,
        [
            "5    11    14   Class java/lang/Exception",
            "23    29    32   Class java/lang/Exception",
            "0     5    22   any",
            "22    23    22   any",
        ],
        "kotlinc's complete exception table"
    );
    assert_eq!(table(&krusty), want, "run exception table");
    let frames = |text: &str| method_section(text, "int run(int)", "StackMapTable");
    let want = frames(&reference);
    assert_eq!(
        want,
        [
            "frame_type = 255 /* full_frame */",
            "offset_delta = 14",
            "locals = [ class Parked, int, int ]",
            "stack = [ class java/lang/Exception ]",
            "frame_type = 5 /* same */",
            "frame_type = 255 /* full_frame */",
            "offset_delta = 1",
            "locals = [ class Parked, int ]",
            "stack = [ class java/lang/Throwable ]",
            "frame_type = 255 /* full_frame */",
            "offset_delta = 9",
            "locals = [ class Parked, int, top, class java/lang/Throwable ]",
            "stack = [ class java/lang/Exception ]",
            "frame_type = 5 /* same */",
        ],
        "kotlinc's complete frame list: the parked exception is slot 3, typed while the \
         finalizer's own handler records frames over it"
    );
    assert_eq!(frames(&krusty), want, "run frames");
}

/// Kotlin runs a `finally` when control leaves its `try` by ANY route, not only by `return`.
/// `break` and `continue` jumped straight to their loop labels, so the finalizer never ran — a
/// silent wrong answer, not a byte difference.
///
/// The loop below leaves its `try` by `continue` on the first iteration and by `break` on the
/// second, so a correct compiler appends `F` twice.
#[test]
fn a_loop_transfer_out_of_a_try_runs_its_finally() {
    // The recorder is REPOSITORY-OWNED. `StringBuilder.append`/`toString` is a stdlib shape, and a
    // finalizer that ran could be observed through an intrinsic or builtin path rather than through
    // the ordinary call this test is about; `Velarium` cannot be reached any way but the one the
    // source writes.
    let src = "class Velarium {\n\
               \x20   private var trail = \"\"\n\
               \x20   fun record(mark: String) { trail = trail + mark }\n\
               \x20   fun trace(): String = trail\n\
               }\n\
               \n\
               fun box(): String {\n\
               \x20   val velarium = Velarium()\n\
               \x20   for (i in 0..1) {\n\
               \x20       try {\n\
               \x20           velarium.record(\"T\")\n\
               \x20           if (i == 0) continue\n\
               \x20           break\n\
               \x20       } finally {\n\
               \x20           velarium.record(\"F\")\n\
               \x20       }\n\
               \x20   }\n\
               \x20   return velarium.trace()\n\
               }\n";
    let jdk = common::jdk_modules();
    // Fails CLOSED: a missing JVM runner is a broken harness, not a passing contract.
    let out = common::compile_and_run_box(
        src,
        "LoopTransferFinally",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("a JVM runner is required to observe that the finalizer ran on the transfer path");
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
    let (reference, krusty) = disassemble_both("LoopTransferRegion", src, "Looping");
    assert_eq!(
        numeric_rows(&krusty, "int run(int)", "Exception table:"),
        numeric_rows(&reference, "int run(int)", "Exception table:"),
        "run exception table"
    );
}

/// A typed catch guards the try BODY and not the copies of the finalizer that a transfer out of it
/// inlines inside `[start, end)`.
///
/// The catch used to be bound over one broad interval, so a copy of the `finally` sat physically
/// inside the range that guards the body. An exception of the caught type thrown by that copy —
/// after the finally had already run for that exit — entered the catch and ran it on a path Kotlin
/// had already left. The `finally` catch-all was excluded from its own copies by a segmented
/// region; the typed catches take the body half of that same region now.
///
/// Compared against kotlinc rather than pinned, so the segmentation stays tied to the reference
/// compiler, and a `while` loop rather than `for (i in a..b)` for the reason the neighbouring case
/// gives.
#[test]
fn a_typed_catch_does_not_guard_the_finalizer_copies_inside_its_body() {
    let src = "class Guarded {\n\
               \x20   fun step() {}\n\
               \x20   fun run(n: Int): Int {\n\
               \x20       var seen = 0\n\
               \x20       var i = 0\n\
               \x20       while (i < n) {\n\
               \x20           i += 1\n\
               \x20           try {\n\
               \x20               seen += i\n\
               \x20               continue\n\
               \x20           } catch (e: IllegalStateException) {\n\
               \x20               seen = -1\n\
               \x20           } finally {\n\
               \x20               step()\n\
               \x20           }\n\
               \x20       }\n\
               \x20       return seen\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("TypedCatchRegion", src, "Guarded");
    let table = |text: &str| numeric_rows(text, "int run(int)", "Exception table:");
    let want = table(&reference);
    assert!(
        want.len() > 1,
        "kotlinc splits the guarded range around the inlined copy: {want:?}"
    );
    assert_eq!(table(&krusty), want, "run exception table");
}

/// The same rule, observed by RUNNING it: a finalizer copy that throws the caught type on the
/// transfer path must not re-enter the catch beside it.
///
/// `Tessitura` is repository-owned, so the throw cannot reach the handler through a stdlib or
/// intrinsic path rather than the ordinary one this is about. With the broad range, the copy of
/// `finally` inlined for the `return` threw inside the interval guarding the body, the catch ran,
/// and `box()` answered `CAUGHT`.
#[test]
fn a_finalizer_copy_that_throws_the_caught_type_does_not_re_enter_the_catch() {
    let src = "class Tessitura {\n\
               \x20   var armed = false\n\
               \x20   fun cleanup() { if (armed) throw IllegalStateException(\"from the finally\") }\n\
               }\n\
               \n\
               fun attempt(tessitura: Tessitura): String {\n\
               \x20   try {\n\
               \x20       tessitura.armed = true\n\
               \x20       return \"RETURNED\"\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       return \"CAUGHT\"\n\
               \x20   } finally {\n\
               \x20       tessitura.cleanup()\n\
               \x20   }\n\
               }\n\
               \n\
               fun box(): String {\n\
               \x20   return try {\n\
               \x20       attempt(Tessitura())\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       \"PROPAGATED\"\n\
               \x20   }\n\
               }\n";
    let jdk = common::jdk_modules();
    // Fails CLOSED: a missing JVM runner is a broken harness, not a passing contract.
    let out = common::compile_and_run_box(
        src,
        "FinalizerCopyThrows",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("a JVM runner is required to observe which handler the finalizer's throw reaches");
    assert_eq!(
        out.trim(),
        "PROPAGATED",
        "the finalizer's throw leaves the try it belongs to instead of entering its own catch"
    );
}

/// A `catch (e: E)` parameter is a debug local like any other. It is DECLARED by its `IrCatch`
/// rather than by a variable node, so it has no declaration expression the name and provenance
/// tables could be keyed by, and it used to carry a bare source spelling that JVM emission wrote
/// straight into the table — the one local in the file that never reached the debug-name boundary.
///
/// Here, where nothing is inlined, that is invisible: the spelling IS the name, and both compilers
/// agree on the complete table down to the offsets.
#[test]
fn a_catch_parameter_is_named_where_it_is_declared() {
    let src = "class Ledger {\n\
               \x20   fun record(tag: String): String {\n\
               \x20       try {\n\
               \x20           return \"kept \" + tag\n\
               \x20       } catch (e: IllegalStateException) {\n\
               \x20           return \"caught \" + e.message\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("DeclaredCatchName", src, "Ledger");
    let table = |text: &str| local_variable_table(text, "java.lang.String record(", 0..5);
    let want = table(&reference);
    assert_eq!(
        want,
        [
            "28 23 2 e Ljava/lang/IllegalStateException;",
            "0 51 0 this LLedger;",
            "0 51 1 tag Ljava/lang/String;",
        ],
        "kotlinc's complete table, offsets included"
    );
    assert_eq!(table(&krusty), want, "record local variable table");
}

/// The same binding once it is inlined. kotlinc suffixes it with one `$iv` per expansion it sits
/// inside, exactly as it suffixes an ordinary local, so the binding has to carry the same
/// provenance an ordinary local carries and be rendered through the same boundary — a spelling
/// copied at lowering cannot say how deep the copy that used it ended up.
///
/// `once` and `nested` differ only in how many expansions the catch is cloned into, so a name that
/// were a constant suffix, or no suffix, would fail on one of the two.
///
/// Byte equality is not attainable and the reason is stated rather than worked around: krusty emits
/// no inline-depth markers (`$i$f`, `$i$a`) and no entries for an expansion's own copied locals, so
/// it writes two rows where kotlinc writes five and seven. That is a whole missing table, it is not
/// what this change is about, and pinning both projections is what makes closing it visible here.
/// The offsets are left out for the same reason — they differ because the row counts do.
#[test]
fn an_inlined_catch_parameter_is_named_at_its_expansion_depth() {
    let src = "inline fun guarded(tag: String, block: () -> String): String {\n\
               \x20   try {\n\
               \x20       return block() + tag\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       return \"caught \" + e.message\n\
               \x20   }\n\
               }\n\
               \n\
               inline fun twice(tag: String, block: () -> String): String = guarded(tag, block)\n\
               \n\
               fun once(tag: String): String = guarded(tag) { tag }\n\
               \n\
               fun nested(tag: String): String = twice(tag) { tag }\n";
    let (reference, krusty) = disassemble_both("NestedCatchNames", src, "NestedCatchNamesKt");
    let table = |text: &str, method: &str| local_variable_table(text, method, 2..5);
    assert_eq!(
        table(&reference, "java.lang.String once("),
        [
            "3 $i$a$-guarded-NestedCatchNamesKt$once$1 I",
            "4 e$iv Ljava/lang/IllegalStateException;",
            "2 $i$f$guarded I",
            "1 tag$iv Ljava/lang/String;",
            "0 tag Ljava/lang/String;",
        ],
        "kotlinc's complete table for one expansion"
    );
    assert_eq!(
        table(&krusty, "java.lang.String once("),
        [
            "2 e$iv Ljava/lang/IllegalStateException;",
            "0 tag Ljava/lang/String;",
        ],
        "krusty's complete table for one expansion: the catch parameter carries its frame"
    );
    assert_eq!(
        table(&reference, "java.lang.String nested("),
        [
            "5 $i$a$-twice-NestedCatchNamesKt$nested$1 I",
            "6 e$iv$iv Ljava/lang/IllegalStateException;",
            "4 $i$f$guarded I",
            "3 tag$iv$iv Ljava/lang/String;",
            "2 $i$f$twice I",
            "1 tag$iv Ljava/lang/String;",
            "0 tag Ljava/lang/String;",
        ],
        "kotlinc's complete table for two"
    );
    assert_eq!(
        table(&krusty, "java.lang.String nested("),
        [
            "3 e$iv$iv Ljava/lang/IllegalStateException;",
            "0 tag Ljava/lang/String;",
        ],
        "krusty's complete table for two: one frame per expansion, not a constant suffix"
    );
}
