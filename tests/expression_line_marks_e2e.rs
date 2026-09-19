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
    let classes = common::compile_in_process(src, name, &[common::stdlib_jar()], None)
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

/// As [`disassemble_both`], for a source set compiled as ONE module.
fn disassemble_both_files(name: &str, sources: &[(&str, &str)], class: &str) -> (String, String) {
    let dir = common::scratch_dir()
        .unwrap_or_else(|| panic!("{name}: no scratch directory for the differential"));
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference dir");
    std::fs::create_dir_all(&krusty_dir).expect("output dir");
    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "1.8".to_string(),
    ];
    for (stem, text) in sources {
        let path = dir.join(format!("{stem}.kt"));
        std::fs::write(&path, text).expect("write source");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) = common::kotlinc_compile(&arguments)
        .unwrap_or_else(|| panic!("{name}: reference kotlinc unavailable under the test harness"));
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_files(sources, &[common::stdlib_jar()], None)
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
    let built = disassemble_both("ExpressionLines", src, "ExpressionLinesKt");
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
    let result = common::byte_diff_against_kotlinc_cp(
        "MultiLineConstruction",
        src,
        "MultiLineConstructionKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("MultiLineConstructionKt byte-identical to kotlinc");
}

/// A single-line call adds no entries: the rules are about a line CHANGING, not about calls.
#[test]
fn a_single_line_call_is_unchanged() {
    let src = "class Q(val a: Int)\n\
               \n\
               fun one(): Q = Q(1)\n";
    let result = common::byte_diff_against_kotlinc_cp(
        "SingleLineCall",
        src,
        "SingleLineCallKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
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
    let result = common::byte_diff_against_kotlinc_cp(
        "ExplicitMultiLineReturn",
        src,
        "ExplicitMultiLineReturnKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("explicit multi-line return byte-identical to kotlinc");
}
/// A finalizer changes the active line while the return value is parked in a local. The eventual
/// return instruction must restore the explicit return's source line, not inherit the finalizer's.
///
/// The call is a CONSTRUCTOR so the whole table is owned here: an ordinary method's dispatch
/// restoration arrives with the method-call PR above this one. The finalizer stays the inline
/// `println("done")` it was written with — the entries this test exists for sit past it, and every
/// LINE in the table is now kotlinc's. The two offsets that differ are the inline splice's, not
/// this contract's: kotlinc keeps the argument on the stack and `swap`s the receiver under it
/// (`ldc; getstatic; swap; invokevirtual`) where krusty round-trips it through two locals, seven
/// bytes more. Both complete tables are spelled out so either compiler moving is visible here.
#[test]
fn an_explicit_return_after_finally_restores_its_line() {
    let src = "class R(val a: Int, val b: String, val c: Int)\n\
               class FinallySink {\n\
               \x20   fun run(x: Int, y: String): R {\n\
               \x20       try {\n\
               \x20           return R(\n\
               \x20               a = x,\n\
               \x20               b = y,\n\
               \x20               c = x + 1,\n\
               \x20           )\n\
               \x20       } finally {\n\
               \x20           println(\"done\")\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("ReturnAfterFinally", src, "FinallySink");
    let want = method_lines(&reference, "R run(int, java.lang.String)");
    assert_eq!(
        want,
        vec![
            "line 4: 6".to_string(),
            "line 5: 7".to_string(),
            "line 6: 11".to_string(),
            "line 7: 12".to_string(),
            "line 8: 13".to_string(),
            "line 5: 16".to_string(),
            "line 11: 20".to_string(),
            "line 5: 30".to_string(),
            "line 11: 31".to_string(),
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    let ours = method_lines(&krusty, "R run(int, java.lang.String)");
    assert_eq!(
        ours.iter()
            .map(|row| row.split(':').next().unwrap_or(row))
            .collect::<Vec<_>>(),
        want.iter()
            .map(|row| row.split(':').next().unwrap_or(row))
            .collect::<Vec<_>>(),
        "every line, in kotlinc's order"
    );
    assert_eq!(
        ours,
        vec![
            "line 4: 6".to_string(),
            "line 5: 7".to_string(),
            "line 6: 11".to_string(),
            "line 7: 12".to_string(),
            "line 8: 13".to_string(),
            "line 5: 16".to_string(),
            "line 11: 20".to_string(),
            // +7: the inline `println` splice, past everything this test pins.
            "line 5: 37".to_string(),
            "line 11: 38".to_string(),
        ],
        "complete run LineNumberTable"
    );
}

/// A BARE `return` through a `finally`. It emits nothing of its own, so without an anchor its line
/// is claimed by the finalizer's first instruction and the restore at the physical return never
/// happens: the whole table collapsed to the finalizer's single line. kotlinc anchors it on a `nop`
/// ahead of the transfer and restores it at the `return`.
#[test]
fn a_bare_return_through_a_finally_restores_its_line() {
    let src = "var t = 0\n\
               fun step() { t++ }\n\
               fun run() {\n\
               \x20   try {\n\
               \x20       return\n\
               \x20   } finally {\n\
               \x20       step()\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("BareReturnFinally", src, "BareReturnFinallyKt");
    let want = method_lines(&reference, "void run()");
    assert_eq!(
        want,
        vec![
            "line 4: 0".to_string(),
            "line 5: 1".to_string(),
            "line 7: 2".to_string(),
            "line 5: 5".to_string(),
            "line 7: 6".to_string(),
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(
        method_lines(&krusty, "void run()"),
        want,
        "complete run LineNumberTable"
    );
}

/// A VALUE return through a `finally`: its own expression anchors the line, and the reload before
/// the physical return restores it. No `nop` — the value's first instruction already carries it.
#[test]
fn a_value_return_through_a_finally_restores_its_line() {
    let src = "var t = 0\n\
               fun step() { t++ }\n\
               fun run(): Int {\n\
               \x20   try {\n\
               \x20       return 1\n\
               \x20   } finally {\n\
               \x20       step()\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("ValueReturnFinally", src, "ValueReturnFinallyKt");
    let want = method_lines(&reference, "int run()");
    assert_eq!(
        want,
        vec![
            "line 4: 0".to_string(),
            "line 5: 1".to_string(),
            "line 7: 3".to_string(),
            "line 5: 7".to_string(),
            "line 7: 8".to_string(),
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(
        method_lines(&krusty, "int run()"),
        want,
        "complete run LineNumberTable"
    );
}

/// An IMPLICIT `Unit` return after a `try`/`finally` that falls through. The `goto` leaving the
/// normal-path finalizer copy carries the `finally` block's CLOSING line — without it the
/// finalizer's own line stays in effect into the catch-all handler, whose identical mark then
/// deduplicates away, so one missing entry costs two.
#[test]
fn an_implicit_unit_return_after_a_finally_keeps_every_line() {
    let src = "var t = 0\n\
               fun f() { t++ }\n\
               fun g() { t += 2 }\n\
               fun run() {\n\
               \x20   try {\n\
               \x20       f()\n\
               \x20   } finally {\n\
               \x20       g()\n\
               \x20   }\n\
               }\n";
    let (reference, krusty) = disassemble_both("ImplicitUnitFinally", src, "ImplicitUnitFinallyKt");
    let want = method_lines(&reference, "void run()");
    assert_eq!(
        want,
        vec![
            "line 5: 0".to_string(),
            "line 6: 1".to_string(),
            "line 8: 4".to_string(),
            "line 9: 7".to_string(),
            "line 8: 10".to_string(),
            "line 10: 16".to_string(),
        ],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(
        method_lines(&krusty, "void run()"),
        want,
        "complete run LineNumberTable"
    );
}
#[test]
fn a_multi_line_method_call_is_byte_identical_to_kotlinc() {
    let src = "class Velarium\n\
               class Sink {\n\
               \x20   fun take(a: Int, b: Velarium, c: Int): Int = a + c\n\
               }\n\
               \n\
               fun run(s: Sink, x: Int, y: Velarium): Int = s.take(\n\
               \x20   a = x,\n\
               \x20   b = y,\n\
               \x20   c = x + 1,\n\
               )\n";
    let result = common::byte_diff_against_kotlinc_cp(
        "MultiLineMethodCall",
        src,
        "MultiLineMethodCallKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("MultiLineMethodCallKt byte-identical to kotlinc");
}

/// A defaulted member dispatch uses its static `$default` entry point but keeps the source call's
/// line contract.
#[test]
fn a_multi_line_defaulted_method_call_is_byte_identical_to_kotlinc() {
    let src = "class DefaultSink {\n\
               \x20   fun consumeDefaultsForFixture(a: Int, b: Int = 2): Int = a + b\n\
               }\n\
               \n\
               fun run(s: DefaultSink, x: Int): Int = s.consumeDefaultsForFixture(\n\
               \x20   a = x,\n\
               )\n";
    let result = common::byte_diff_against_kotlinc_cp(
        "MultiLineDefaultedMethodCall",
        src,
        "MultiLineDefaultedMethodCallKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("defaulted multi-line method call byte-identical to kotlinc");
}

/// An argument supplied BETWEEN two omitted ones splits the call's synthesized operands in two: the
/// placeholder, then that argument's own line, then the mask and marker. Each synthesized group
/// carries the call's line, so the table returns to it twice — a single mark at the invoke, or one
/// at the first placeholder alone, both get this wrong.
#[test]
fn an_omitted_argument_before_a_supplied_one_is_byte_identical_to_kotlinc() {
    let src = "class MiddleSink {
                   fun consumeDefaultsForFixture(a: Int, b: Int = 2, c: Int = 3): Int = a + b + c
               }

               fun run(s: MiddleSink, x: Int, z: Int): Int = s.consumeDefaultsForFixture(
                   a = x,
                   c = z,
               )
";
    let result = common::byte_diff_against_kotlinc_cp(
        "OmittedMiddleArgument",
        src,
        "OmittedMiddleArgumentKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("omitted middle argument byte-identical to kotlinc");
}

/// A same-file TOP-LEVEL default call reaches its target through the facade's `$default` synthetic,
/// a different realization from a member dispatch. The synthesized operands are the call's on every
/// one of these paths, so the rule is the same: the call's line goes back into effect at the start
/// of each run of them.
#[test]
fn a_same_file_top_level_defaulted_call_is_byte_identical_to_kotlinc() {
    let src = "fun consumeDefaultsForFixture(a: Int = 1, b: Int, c: Int = 3): Int = a + b + c\n\
               \n\
               fun run(x: Int): Int = consumeDefaultsForFixture(\n\
               \x20   b = x,\n\
               )\n";
    let result = common::byte_diff_against_kotlinc_cp(
        "TopLevelDefaultedCall",
        src,
        "TopLevelDefaultedCallKt",
        &[common::stdlib_jar()],
    )
    .unwrap_or_else(|| panic!("reference kotlinc unavailable under the test harness"));
    result.expect("same-file top-level defaulted call byte-identical to kotlinc");
}

/// The cross-file realization, which reaches the target through another file's facade. Its omitted
/// `a` is supplied before `b`, so the call's line is restored twice: once for the leading
/// placeholder and once for the mask/marker group after the supplied argument.
#[test]
fn a_cross_file_defaulted_call_maps_its_lines_like_kotlinc() {
    let sources: &[(&str, &str)] = &[
        (
            "CrossDefaultTarget",
            "fun consumeDefaultsForFixture(a: Int = 1, b: Int, c: Int = 3): Int = a + b + c\n",
        ),
        (
            "CrossDefaultCaller",
            "fun run(x: Int): Int = consumeDefaultsForFixture(\n\x20   b = x,\n)\n",
        ),
    ];
    let (reference, krusty) =
        disassemble_both_files("CrossFileDefault", sources, "CrossDefaultCallerKt");
    let want = method_lines(&reference, "int run(int)");
    assert_eq!(
        want,
        ["line 1: 0", "line 2: 1", "line 1: 2", "line 3: 8"],
        "kotlinc's own table, spelled out so a reference change is visible here"
    );
    assert_eq!(method_lines(&krusty, "int run(int)"), want, "run lines");
}

/// The complete `LineNumberTable` of one method, from both compilers, asserted equal — and the
/// reference's own table spelled out so a change in it is visible in the diff rather than silently
/// redefining the contract.
fn assert_line_table(name: &str, src: &str, class: &str, signature: &str, want: &[&str]) {
    let (reference, krusty) = disassemble_both(name, src, class);
    let reference_lines = method_lines(&reference, signature);
    assert_eq!(
        reference_lines, want,
        "{name}: kotlinc's own table for `{signature}`"
    );
    assert_eq!(
        method_lines(&krusty, signature),
        reference_lines,
        "{name}: complete line table for `{signature}`, offsets included"
    );
}

/// A FUNCTION VALUE's invocation is a dispatch like any other: `Function2.invoke` returns to the
/// line the call is written on, after its operands have each marked their own.
#[test]
fn a_multi_line_function_value_invocation_returns_to_its_line() {
    assert_line_table(
        "FunctionValueDispatch",
        "fun run(f: (Int, Int) -> Int, x: Int): Int = f(\n\
         \x20   x,\n\
         \x20   x + 1,\n\
         )\n",
        "FunctionValueDispatchKt",
        "int run(kotlin.jvm.functions.Function2<? super java.lang.Integer, ? super java.lang.Integer, java.lang.Integer>, int)",
        &["line 1: 6", "line 2: 7", "line 3: 11", "line 1: 17", "line 4: 28"],
    );
}

/// `enumValueOf<E>(…)` keeps TWO entries at one offset: the call's own line, then the operand's.
/// A mark that replaced the entry already at that offset could not reproduce it.
#[test]
fn a_multi_line_enum_value_of_keeps_both_entries_at_its_dispatch() {
    assert_line_table(
        "EnumValueOfDispatch",
        "enum class E { A, B }\n\
         \n\
         fun run(s: String): E = enumValueOf<E>(\n\
         \x20   s\n\
         )\n",
        "EnumValueOfDispatchKt",
        "E run(java.lang.String)",
        &["line 3: 6", "line 4: 6", "line 5: 10"],
    );
}

/// The member realization of the same operation, `E.valueOf(…)`.
#[test]
fn a_multi_line_enum_member_value_of_returns_to_its_line() {
    assert_line_table(
        "EnumMemberValueOfDispatch",
        "enum class E { A, B }\n\
         \n\
         fun run(s: String): E = E.valueOf(\n\
         \x20   s\n\
         )\n",
        "EnumMemberValueOfDispatchKt",
        "E run(java.lang.String)",
        &["line 4: 6", "line 3: 7", "line 5: 10"],
    );
}

/// A defaulted CONSTRUCTOR takes the same synthesized-operand rule as a defaulted method call: the
/// call's line returns at the FIRST placeholder, not at the `invokespecial` after the whole group.
#[test]
fn a_defaulted_construction_maps_its_lines_like_kotlinc() {
    assert_line_table(
        "DefaultedConstruction",
        "class P(val a: Int = 1, val b: Int, val c: Int = 3)\n\
         \n\
         fun run(x: Int): P = P(\n\
         \x20   b = x,\n\
         )\n",
        "DefaultedConstructionKt",
        "P run(int)",
        &["line 3: 0", "line 4: 5", "line 3: 6", "line 5: 12"],
    );
}

/// A primitive `compareTo` is lowered to `Integer.compare`, which is still a real dispatch and
/// still returns to the call's line.
#[test]
fn a_multi_line_primitive_compare_to_returns_to_its_line() {
    assert_line_table(
        "PrimitiveCompareDispatch",
        "fun run(x: Int, y: Int): Int = x.compareTo(\n\
         \x20   y\n\
         )\n",
        "PrimitiveCompareDispatchKt",
        "int run(int, int)",
        &["line 1: 0", "line 2: 1", "line 1: 2", "line 3: 5"],
    );
}

/// The control that keeps the rule from becoming "mark every intrinsic": `String.get` is lowered to
/// a real `charAt` dispatch and already agrees with kotlinc, and an array read is no dispatch at
/// all. Both tables must stay exactly as they are.
#[test]
fn a_multi_line_string_get_and_array_get_are_unchanged() {
    assert_line_table(
        "StringGetDispatch",
        "fun run(s: String, i: Int): Char = s.get(\n\
         \x20   i\n\
         )\n",
        "StringGetDispatchKt",
        "char run(java.lang.String, int)",
        &["line 1: 6", "line 2: 7", "line 1: 8", "line 3: 11"],
    );
    assert_line_table(
        "ArrayGetDispatch",
        "fun run(a: IntArray, i: Int): Int = a.get(\n\
         \x20   i\n\
         )\n",
        "ArrayGetDispatchKt",
        "int run(int[], int)",
        &["line 1: 6", "line 2: 7", "line 3: 9"],
    );
}

/// A property READ through a computed accessor dispatches to the getter, so the accessor's line
/// returns at that dispatch rather than partway through the physical receiver sequence.
#[test]
fn a_multi_line_property_read_returns_to_its_accessor_line() {
    assert_line_table(
        "PropertyGetDispatch",
        "class Box(private val x: Int) {\n\
         \x20   val value: Int get() = x + 1\n\
         }\n\
         \n\
         fun run(b: Box): Int =\n\
         \x20   b\n\
         \x20       .value\n",
        "PropertyGetDispatchKt",
        "int run(Box)",
        &["line 6: 6", "line 7: 7"],
    );
}

/// And a property WRITE: the assignment's own line returns before the setter invocation, after the
/// value expression has marked its line.
#[test]
fn a_multi_line_property_write_returns_to_its_accessor_line() {
    assert_line_table(
        "PropertySetDispatch",
        "class Box { var value: Int = 0 }\n\
         \n\
         fun run(b: Box, x: Int) {\n\
         \x20   b\n\
         \x20       .value =\n\
         \x20       x\n\
         }\n",
        "PropertySetDispatchKt",
        "void run(Box, int)",
        &["line 4: 6", "line 6: 7", "line 5: 8", "line 7: 11"],
    );
}

/// The other realization of the same source operation: inside a `reified` inline function the
/// classifier is not yet known, so the emitted template carries the reified marker instead of a
/// resolved `valueOf`. It is still an inline call SITE, and takes the same rule.
#[test]
fn a_reified_enum_value_of_template_marks_its_call_site() {
    assert_line_table(
        "ReifiedEnumValueOfDispatch",
        "enum class E { A, B }\n\
         \n\
         inline fun <reified T : Enum<T>> lookup(s: String): T = enumValueOf<T>(\n\
         \x20   s\n\
         )\n\
         \n\
         fun run(s: String): E = lookup(s)\n",
        "ReifiedEnumValueOfDispatchKt",
        "T lookup(java.lang.String)",
        &["line 3: 8", "line 4: 8", "line 5: 21"],
    );
}
