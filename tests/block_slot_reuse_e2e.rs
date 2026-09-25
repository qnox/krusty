//! Local slots as kotlinc's `FrameMap` hands them out. A block's variables are free again once the
//! block ends, so a sibling branch, the next loop, and the statements after a block reuse them. A
//! variable is entered before its initializer, so the initializer's own locals and temporaries sit
//! above it, and a `try` that is not `Unit` enters its result temporary after its body. Backend
//! temporaries are a stack too: a `return` through a `finally` enters its spill at the `return`, a
//! catch-all enters the caught throwable at its handler, and a released temporary's slot is reused.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// Every method's `LocalVariableTable` rows as `slot name descriptor`, in table order, from
/// `javap -l`. Start and length are left out: they follow instruction offsets, which differ for
/// reasons other than slot choice.
fn local_variable_rows(class_file: &std::path::Path) -> Vec<String> {
    let text = common::javap(&["-c", "-l", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() == 5 && fields[..3].iter().all(|f| f.parse::<u32>().is_ok()))
                .then(|| format!("{} {} {}", fields[2], fields[3], fields[4]))
        })
        .collect()
}

/// Compile `src` with kotlinc and krusty and require the same local-variable slots.
fn same_local_slots(name: &str, src: &str, class: &str, extra_classpath: &[std::path::PathBuf]) {
    same_local_slots_of(name, src, class, extra_classpath, |_| true);
}

/// As [`same_local_slots`], with each row kept once. kotlinc splits a local's range around every
/// inlined copy of a `finally` (`splitLocalVariableRangesByFinallyBlocks`) and krusty does not yet,
/// so a local live across a finalizer copy has two rows there and one here. The slots are what is
/// compared.
fn same_distinct_local_slots(name: &str, src: &str, class: &str) {
    if let Some((emitted, expected)) = local_slot_rows(name, src, class, &[], |_| true) {
        assert_eq!(distinct(emitted), distinct(expected), "{name}");
    }
}

/// `rows` without the repeats of an earlier row, in table order.
fn distinct(rows: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| seen.insert(row.clone()))
        .collect()
}

/// As [`same_local_slots`], for the rows of the locals `compared` selects by name.
fn same_local_slots_of(
    name: &str,
    src: &str,
    class: &str,
    extra_classpath: &[std::path::PathBuf],
    compared: impl Fn(&str) -> bool,
) {
    if let Some((emitted, expected)) = local_slot_rows(name, src, class, extra_classpath, compared)
    {
        assert_eq!(emitted, expected, "{name}");
    }
}

/// krusty's and kotlinc's `LocalVariableTable` rows for `class`, compiled with `extra_classpath`
/// and restricted to the locals `compared` selects; `None` when the reference toolchain is
/// unavailable.
fn local_slot_rows(
    name: &str,
    src: &str,
    class: &str,
    extra_classpath: &[std::path::PathBuf],
    compared: impl Fn(&str) -> bool,
) -> Option<(Vec<String>, Vec<String>)> {
    let rows = |class_file: &std::path::Path| -> Vec<String> {
        local_variable_rows(class_file)
            .into_iter()
            .filter(|row| row.split(' ').nth(1).is_some_and(&compared))
            .collect()
    };
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip ({name}: no scratch directory)");
        return None;
    };
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).unwrap();
    let src_path = dir.join(format!("{name}.kt"));
    std::fs::write(&src_path, src).unwrap();
    let mut args = vec!["-d".to_string(), reference.to_string_lossy().into_owned()];
    if !extra_classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(
            std::env::join_paths(extra_classpath)
                .expect("fixture classpath is valid")
                .to_string_lossy()
                .into_owned(),
        );
    }
    args.push(src_path.to_string_lossy().into_owned());
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip ({name}: reference toolchain unavailable)");
        return None;
    };
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut classpath = extra_classpath.to_vec();
    classpath.push(stdlib);
    let classes = common::compile_in_process(src, name, &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{class} was not emitted"));
    let emitted = dir.join(format!("{class}.class"));
    std::fs::write(&emitted, bytes).unwrap();
    let expected = rows(&reference.join(format!("{class}.class")));
    assert!(
        !expected.is_empty(),
        "{name}: kotlinc's class has no local variables"
    );
    let emitted = rows(&emitted);
    let _ = std::fs::remove_dir_all(&dir);
    Some((emitted, expected))
}

fn run(src: &str) -> String {
    common::compile_and_run_with_stdlib(src, "Main").expect("box() ran")
}

/// `a`, `b` and `z` all take slot 3: each branch's local is left at the end of its branch.
#[test]
fn sibling_branches_and_the_next_statement_share_a_slot() {
    byte_identical(
        "slotReuseSiblings",
        "fun siblings(c: Boolean, n: Int): Int {\n\
    var t = 0\n\
    if (c) {\n\
        val a = n + 1\n\
        t = a\n\
    } else {\n\
        val b = n * 2\n\
        t = b\n\
    }\n\
    val z = t + 3\n\
    return z\n\
}\n",
        "SlotReuseSiblingsKt",
    );
}

/// `after` takes the slot of the loop body's `sq`.
#[test]
fn a_loop_body_local_slot_is_reused_after_the_loop() {
    byte_identical(
        "slotReuseLoops",
        "fun loops(n: Int): Int {\n\
    var s = 0\n\
    var i = 0\n\
    while (i < n) {\n\
        val sq = i * i\n\
        s += sq\n\
        i++\n\
    }\n\
    val after = s\n\
    return after\n\
}\n",
        "SlotReuseLoopsKt",
    );
}

/// A `do` body's local stays live through the condition that reads it, and is left after it.
#[test]
fn a_do_while_body_local_lives_through_the_condition() {
    let src = "fun count(n: Int): Int {\n\
    var s = 0\n\
    do {\n\
        val d = s + 1\n\
        s = d\n\
    } while (d < n)\n\
    val after = s * 10\n\
    return after\n\
}\n\
fun box(): String = if (count(4) == 40) \"OK\" else \"FAIL: ${count(4)}\"\n";
    assert_eq!(run(src), "OK");
}

/// Each catch parameter takes the same slot, and the local after the `try` reuses it.
#[test]
fn catch_parameters_and_the_next_local_share_a_slot() {
    let src = "fun divide(n: Int): String {\n\
    var r = \"\"\n\
    try {\n\
        r = (10 / n).toString()\n\
    } catch (e: ArithmeticException) {\n\
        val m = \"div\"\n\
        r = m\n\
    } catch (e: IllegalStateException) {\n\
        r = \"state\"\n\
    }\n\
    val after = r + \"!\"\n\
    return after\n\
}\n\
fun box(): String {\n\
    val results = divide(0) + divide(5)\n\
    return if (results == \"div!2!\") \"OK\" else \"FAIL: $results\"\n\
}\n";
    assert_eq!(run(src), "OK");
}

/// A `finally`'s parked exception slot is handed to the next `try` again, so the locals declared
/// after the first `try`'s block must not be given it: `r` would otherwise share a slot with the
/// throwable the second `finally` parks, and read it back as an `int`.
#[test]
fn a_local_after_a_try_block_does_not_take_the_parked_exception_slot() {
    let src = "fun parked(n: Int): Int {\n\
    if (n > 0) {\n\
        val a = n\n\
        try {\n\
            println(a)\n\
        } finally {\n\
            println(\"first\")\n\
        }\n\
    }\n\
    val v = n + 1\n\
    var r = 0\n\
    try {\n\
        r = 10 / n\n\
    } finally {\n\
        r += v\n\
    }\n\
    return r\n\
}\n\
fun box(): String {\n\
    val thrown = try { parked(0); \"no throw\" } catch (e: ArithmeticException) { \"thrown\" }\n\
    val value = parked(5)\n\
    return if (thrown == \"thrown\" && value == 8) \"OK\" else \"FAIL: $thrown $value\"\n\
}\n";
    assert_eq!(run(src), "OK");
}

/// A `try` that is not `Unit` enters its result temporary after the body, in kotlinc a `Void`
/// one for a `Nothing` `try`: it takes the slot `x` has just left, so `e` is slot 2, not 1.
#[test]
fn a_catch_parameter_after_a_returning_try_body_sits_above_the_result_temporary() {
    same_local_slots(
        "slotReuseTryReturn",
        "fun tryReturn(n: Int): String {\n\
    try {\n\
        val x = n + 1\n\
        return \"r$x\"\n\
    } catch (e: RuntimeException) {\n\
        return \"e\"\n\
    }\n\
}\n",
        "SlotReuseTryReturnKt",
        &[],
    );
}

#[test]
fn a_catch_parameter_after_a_throwing_try_body_sits_above_the_result_temporary() {
    same_local_slots(
        "slotReuseTryThrow",
        "fun tryThrow(n: Int): Int {\n\
    try {\n\
        val x = n + 1\n\
        throw IllegalStateException(\"m$x\")\n\
    } catch (e: IllegalStateException) {\n\
        return 0\n\
    }\n\
}\n",
        "SlotReuseTryThrowKt",
        &[],
    );
}

/// An inlined `@InlineOnly` body is laid out from the frame size at its call, which the block that
/// ended before the call has lowered, and a block inside one argument frees only its own slots:
/// each argument's locals start at that argument's parameter slot, above the arguments stored
/// before it, and the local after the call takes the slot the block before it left.
#[test]
fn an_inline_call_after_a_block_lays_its_arguments_out_from_the_lowered_frame() {
    let library = common::kotlinc_library(INLINE_LIBRARY)
        .expect("reference compiler must build the inline-slot fixture");
    same_local_slots(
        "slotReuseInlineArguments",
        INLINE_ARGUMENTS,
        "SlotReuseInlineArgumentsKt",
        &[library],
    );
}

#[test]
fn an_inline_call_after_a_block_runs() {
    let src = format!(
        "{INLINE_ARGUMENTS}\
fun box(): String {{\n\
    val r = \"${{bounded(true, 4, 20L)}} ${{bounded(false, 4, 20L)}} ${{bounded(true, -2, 9L)}}\"\n\
    return if (r == \"23 19 -7\") \"OK\" else \"FAIL: $r\"\n\
}}\n"
    );
    let output = common::expect_box_run_against_kotlinc(INLINE_LIBRARY, &src)
        .expect("reference compiler must build the inline-slot fixture");
    assert_eq!(output, "OK");
}

/// A materialized inline body (`Continuation(context) { }` builds an object from its lambda) is
/// spliced from the frame size at the call, as kotlinc's is: `k` takes slot 1, which `a` handed
/// back, and the body's parameters are stored from slot 2 above it, not above the `max_locals`
/// the block reached.
#[test]
fn a_materialized_inline_body_after_a_block_starts_at_the_lowered_frame() {
    let src = "import kotlin.coroutines.*\n\
fun materialized(c: Boolean): Continuation<Int> {\n\
    if (c) {\n\
        val a = 1\n\
        val b = 2L\n\
        println(a + b)\n\
    }\n\
    val k = Continuation<Int>(EmptyCoroutineContext) { r -> println(r) }\n\
    return k\n\
}\n";
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip (no scratch directory)");
        return;
    };
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).unwrap();
    let src_path = dir.join("slotReuseMaterializedInline.kt");
    std::fs::write(&src_path, src).unwrap();
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let classes = common::compile_in_process(
        src,
        "slotReuseMaterializedInline",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles");
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == "SlotReuseMaterializedInlineKt")
        .expect("SlotReuseMaterializedInlineKt was emitted");
    let emitted = dir.join("SlotReuseMaterializedInlineKt.class");
    std::fs::write(&emitted, bytes).unwrap();
    let base = |class: &std::path::Path| {
        let stores = local_stores(class, "materialized(boolean)");
        // The block's own stores come first (`a`, `b` and the sum it prints) and `k`'s last; the
        // spliced body's parameter stores are the ones between.
        assert_eq!(stores[..3], [1, 2, 4], "{}", class.display());
        assert_eq!(stores.last(), Some(&1), "{}", class.display());
        stores[3..stores.len() - 1].iter().copied().min()
    };
    let expected = base(&reference.join("SlotReuseMaterializedInlineKt.class"));
    assert_eq!(expected, Some(2));
    assert_eq!(base(&emitted), expected);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The slot of every local store in `method`, in instruction order, from `javap -c`.
fn local_stores(class_file: &std::path::Path, method: &str) -> Vec<u16> {
    let text = common::javap(&["-c", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    text.lines()
        .skip_while(|line| !line.contains(method))
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("public "))
        .filter_map(|line| {
            let instruction = line.split_whitespace().nth(1)?;
            let (op, inline_slot) = instruction.split_once('_').unwrap_or((instruction, ""));
            if !matches!(op, "istore" | "lstore" | "fstore" | "dstore" | "astore") {
                return None;
            }
            match inline_slot {
                "" => line.split_whitespace().nth(2)?.parse().ok(),
                slot => slot.parse().ok(),
            }
        })
        .collect()
}

const INLINE_LIBRARY: &str = "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
package slotfixture\n\
@kotlin.internal.InlineOnly\n\
inline fun lesser(a: Long, b: Long): Long = if (a < b) a else b\n\
@kotlin.internal.InlineOnly\n\
inline fun assertFlag(value: Boolean) { if (!value) throw IllegalStateException(\"flag\") }\n\
inline fun transform(value: Int, action: (Int) -> Int): Int = action(value)\n";

const INLINE_ARGUMENTS: &str = "import slotfixture.assertFlag\n\
import slotfixture.lesser\n\
fun bounded(c: Boolean, n: Int, m: Long): Long {\n\
    var r = 0L\n\
    if (c) {\n\
        val a = n + 1\n\
        val b = a.toLong() * 2\n\
        r = b\n\
    }\n\
    r += lesser(if (c) { val y = n * 3L; y + 1 } else m, if (n > 0) { val w = m - 1; w } else r)\n\
    assertFlag(if (c) { val t = n; t > -5 } else true)\n\
    val after = r\n\
    return after\n\
}\n";

/// `r` is entered before its initializer, so the `when` subject `s` declared inside it takes the
/// next slot, and `k` reuses it once the initializer's block has ended.
#[test]
fn a_when_subject_in_an_initializer_sits_above_the_variable() {
    same_local_slots(
        "slotOrderWhenSubject",
        "fun peek() = 1\n\
fun whenSubject(n: Int): String {\n\
    val r = when (val s = peek() + n) {\n\
        1 -> \"one\"\n\
        else -> \"other $s\"\n\
    }\n\
    val k = r + \"!\"\n\
    return k\n\
}\n",
        "SlotOrderWhenSubjectKt",
        &[],
    );
}

/// A branch of the initializer declares `t`: it sits above `r`, at slot 3, and `k` takes it back.
#[test]
fn an_initializer_block_local_sits_above_the_variable() {
    same_local_slots(
        "slotOrderBlockInit",
        "fun blockInit(c: Boolean, n: Int): Int {\n\
    val r = if (c) {\n\
        val t = n + 1\n\
        t * 2\n\
    } else 0\n\
    val k = r + 1\n\
    return k\n\
}\n",
        "SlotOrderBlockInitKt",
        &[],
    );
}

/// The same with two-word locals: `d` takes 1-2 before its initializer, so `x` is at 3.
#[test]
fn a_wide_initializer_local_sits_above_the_wide_variable() {
    same_local_slots(
        "slotOrderWideInit",
        "fun wideInit(c: Boolean): Double {\n\
    val d = if (c) {\n\
        val x = 1.5\n\
        x * 2\n\
    } else 0.0\n\
    val after = d + 1\n\
    return after\n\
}\n",
        "SlotOrderWideInitKt",
        &[],
    );
}

/// `s` = 1, the body's `x` = 2, the result temporary entered after the body takes 2 again, and the
/// catch parameter `e` = 3.
#[test]
fn a_valued_try_enters_its_result_temporary_after_its_body() {
    same_local_slots(
        "slotOrderValuedTry",
        "fun valuedTry(n: Int): String {\n\
    val s = try {\n\
        val x = n + 1\n\
        \"a$x\"\n\
    } catch (e: RuntimeException) {\n\
        \"e\"\n\
    }\n\
    return s\n\
}\n",
        "SlotOrderValuedTryKt",
        &[],
    );
}

/// A valued `try` whose body throws still enters its (`String`) result temporary after the body.
#[test]
fn a_valued_try_with_a_throwing_body_enters_its_result_temporary_after_it() {
    same_local_slots(
        "slotOrderThrowingTry",
        "fun throwingTry(n: Int): String {\n\
    val s = try {\n\
        val x = n + 1\n\
        throw IllegalStateException(\"m$x\")\n\
    } catch (e: IllegalStateException) {\n\
        \"e\"\n\
    }\n\
    return s\n\
}\n",
        "SlotOrderThrowingTryKt",
        &[],
    );
}

/// The variable's slot is entered before a branchy initializer but stored only at its end, so every
/// frame recorded inside the initializer — a loop head, a handler, a `when` merge, a safe-call exit
/// — must read it as unassigned, including a two-word one.
#[test]
fn branchy_initializers_verify_with_the_variable_entered_first() {
    let src = "fun parse(s: String?): Int {\n\
    val total: Long = try {\n\
        var acc = 0L\n\
        var i = 0\n\
        while (i < 3) {\n\
            acc += try { (s ?: \"x\").toInt().toLong() } catch (e: NumberFormatException) { -1L }\n\
            i++\n\
        }\n\
        acc\n\
    } catch (e: IllegalStateException) {\n\
        -100L\n\
    }\n\
    val kind = when {\n\
        total > 0 -> { val half = total / 2; \"pos$half\" }\n\
        total < 0 -> if (s != null) \"neg${s.length}\" else \"neg\"\n\
        else -> \"zero\"\n\
    }\n\
    val d = if (kind.length > 3) { val w = kind.length * 1.5; w } else 0.0\n\
    return (total + kind.length + d.toLong()).toInt()\n\
}\n\
fun guarded(n: Int): String {\n\
    val s = try {\n\
        val x = 10 / n\n\
        \"v$x\"\n\
    } finally {\n\
        val f = n + 1\n\
        println(f)\n\
    }\n\
    val after = s + \"!\"\n\
    return after\n\
}\n\
fun box(): String {\n\
    val a = parse(\"4\")\n\
    val b = parse(null)\n\
    val c = guarded(5)\n\
    val thrown = try { guarded(0) } catch (e: ArithmeticException) { \"thrown\" }\n\
    return if (a == 22 && b == 0 && c == \"v2!\" && thrown == \"thrown\") \"OK\" else \"FAIL: $a $b $c $thrown\"\n\
}\n";
    assert_eq!(run(src), "OK");
}

/// An inline call in an initializer is spliced from the frame size at the call, which already
/// holds the variable: `r` takes slot 1 and `transform`'s inlined locals start above it. Only the
/// inlined `$iv` locals and `r` are compared; `k` still sits above the released argument
/// temporaries, and the lambda's `$i$a$` marker is named differently.
#[test]
fn an_inline_call_in_an_initializer_is_spliced_above_the_variable() {
    let library = common::kotlinc_library(INLINE_LIBRARY)
        .expect("reference compiler must build the inline-slot fixture");
    same_local_slots_of(
        "slotOrderInlineInit",
        "import slotfixture.transform\n\
fun transformedInit(n: Int): Int {\n\
    val r = transform(n) { value -> value * 2 }\n\
    val k = r + 1\n\
    return k\n\
}\n",
        "SlotOrderInlineInitKt",
        &[library],
        |local| local == "r" || local.ends_with("$iv"),
    );
}

/// A same-file inline function's argument is held once it is evaluated, as kotlinc's inliner
/// stores it: `r` takes slots 2-3 before its initializer, the argument's own `y` takes 4, and the
/// argument's holder `x$iv` takes the slot `y` has left. (`it` is not compared: kotlinc's `$i$f$`
/// marker sits below it.)
#[test]
fn a_same_file_inline_argument_is_held_after_its_value_in_an_initializer() {
    same_local_slots_of(
        "slotOrderSameFileInline",
        "inline fun twice(x: Long, f: (Long) -> Long): Long = f(x) + f(x)\n\
fun sameFile(c: Boolean, n: Int): Long {\n\
    val r = twice(if (c) { val y = n * 3L; y + 1 } else 0L) { it + n }\n\
    val k = r + 1\n\
    return k\n\
}\n",
        "SlotOrderSameFileInlineKt",
        &[],
        |local| ["r", "y", "x$iv", "k"].contains(&local),
    );
}

/// A `return` through a `finally` enters its spill at the `return`, above `a`, as kotlinc's
/// `generateFinallyBlocksIfNeeded` does; nothing is reserved when the `try` opens, so `a` is 1.
#[test]
fn a_return_spill_is_entered_at_the_return() {
    same_distinct_local_slots(
        "slotTempReturnSpill",
        "fun tryFinally(x: Int): Int {\n\
    try {\n\
        val a = x + 1\n\
        return a\n\
    } finally {\n\
        println(\"f\")\n\
    }\n\
}\n",
        "SlotTempReturnSpillKt",
    );
}

/// A two-word return spill sits above the two-word `a` (slots 4-5), so the finalizer copy on the
/// `return` path puts `d` at 6, while the copies on the other paths put it at 2 and 3; `after`
/// reuses 2.
#[test]
fn a_wide_return_spill_lies_under_the_finalizer_copy_it_runs() {
    same_distinct_local_slots(
        "slotTempWideReturnSpill",
        "fun wide(x: Long): Long {\n\
    try {\n\
        val a = x + 1\n\
        if (a > 3) return a * 2\n\
    } finally {\n\
        val d = 2.5\n\
        println(d)\n\
    }\n\
    val after = x - 1\n\
    return after\n\
}\n",
        "SlotTempWideReturnSpillKt",
    );
}

/// A statement `try` has no result temporary and its catch-all's throwable is left after the
/// rethrow, so `z` takes slot 1. The whole class matches kotlinc.
#[test]
fn a_local_after_a_try_finally_reuses_the_caught_exception_slot() {
    byte_identical(
        "slotTempStatementTry",
        "fun sink(x: Int) {}\n\
fun statementTryThenLocal(n: Int): Int {\n\
    try {\n\
        sink(n)\n\
    } finally {\n\
        sink(0)\n\
    }\n\
    val z = n + 1\n\
    return z\n\
}\n",
        "SlotTempStatementTryKt",
    );
}

/// The result temporary of a valued `try` is left when its value is read, so `z` reuses it.
#[test]
fn a_local_after_a_valued_try_reuses_its_result_temporary() {
    same_local_slots(
        "slotTempValuedTry",
        "fun valuedThenLocal(n: Int): Int {\n\
    val s = try {\n\
        n / 2\n\
    } catch (e: ArithmeticException) {\n\
        0\n\
    }\n\
    val z = s + 1\n\
    return z\n\
}\n",
        "SlotTempValuedTryKt",
        &[],
    );
}

/// kotlinc types a `try` as the join of its branches even when its value is discarded, and the
/// checker records the same join, so a statement `try` whose body is a `StringBuilder` and whose
/// catch is `Unit` still enters an (`Object`) result temporary after its body: `e` is 2 and `z`
/// reuses 1.
#[test]
fn a_statement_try_whose_branches_disagree_enters_a_result_temporary() {
    same_local_slots(
        "slotTempDiscardedTry",
        "fun discarded(sb: StringBuilder) {\n\
    try {\n\
        sb.append(\"x\")\n\
    } catch (e: Exception) {\n\
        println(e)\n\
    }\n\
    val z = sb.length\n\
    println(z)\n\
}\n",
        "SlotTempDiscardedTryKt",
        &[],
    );
}

/// The catch parameter is entered at its handler and left before that catch's finalizer copy, and
/// the catch-all's throwable is entered at its handler: the normal-path and catch copies of `f` take
/// slot 2, the catch-all's copy 3 above the throwable, and `after` 2.
#[test]
fn catch_parameters_and_the_caught_throwable_are_entered_at_their_handlers() {
    same_local_slots(
        "slotTempFinallyLocal",
        "fun finallyLocal(n: Int): Int {\n\
    var r = 0\n\
    try {\n\
        r = 10 / n\n\
    } catch (e: ArithmeticException) {\n\
        val m = 3\n\
        r = m\n\
    } finally {\n\
        val f = r * 2\n\
        println(f)\n\
    }\n\
    val after = r + 1\n\
    return after\n\
}\n",
        "SlotTempFinallyLocalKt",
        &[],
    );
}

/// Nested `try`s with a `finally` each: every `return` enters its own spill above what is in scope
/// there and both finalizer copies it runs lie above that spill; a `return` in a catch sits above
/// the catch parameter.
#[test]
fn nested_finalizers_run_by_a_return_lie_above_its_spill() {
    same_distinct_local_slots(
        "slotTempNestedTry",
        "fun nestedTry(n: Int): String {\n\
    try {\n\
        try {\n\
            val a = n + 1\n\
            if (a > 3) return \"big$a\"\n\
        } catch (e: IllegalStateException) {\n\
            val q = \"x\"\n\
            return q\n\
        } finally {\n\
            val g = n * 2\n\
            println(g)\n\
        }\n\
    } finally {\n\
        val h = n * 3\n\
        println(h)\n\
    }\n\
    val tail = \"t$n\"\n\
    return tail\n\
}\n\
fun returnInCatch(n: Int): Int {\n\
    try {\n\
        val a = n + 1\n\
        return 10 / a\n\
    } catch (e: ArithmeticException) {\n\
        val b = n - 1\n\
        return b\n\
    } finally {\n\
        println(\"f\")\n\
    }\n\
}\n",
        "SlotTempNestedTryKt",
    );
}

/// Released temporaries hand their slots to what follows, at another type: a return spill, a
/// caught throwable, a `&&` operand, spilled vararg elements and their array are all reused by
/// later locals, including two-word ones, and by branchy finalizers that must still see the
/// parked value.
#[test]
fn reused_temporary_slots_verify_and_run() {
    let src = "fun spill(n: Int): Long {\n\
    try {\n\
        try {\n\
            val a = n.toLong() * 3\n\
            if (n > 2) return a\n\
        } finally {\n\
            val s: String? = if (n > 5) null else \"s$n\"\n\
            println(s?.length ?: -1)\n\
        }\n\
    } catch (e: IllegalStateException) {\n\
        return -1L\n\
    } finally {\n\
        val d = n * 0.5\n\
        if (d > 100.0) println(d)\n\
    }\n\
    val after = n * 7L\n\
    return after\n\
}\n\
fun thrower(n: Int): Int {\n\
    var r = 0\n\
    try {\n\
        try {\n\
            r = 10 / n\n\
        } finally {\n\
            r += 1\n\
        }\n\
    } catch (e: ArithmeticException) {\n\
        val w = 2.0\n\
        r = w.toInt()\n\
    }\n\
    val k = r.toLong() + 1\n\
    return k.toInt()\n\
}\n\
fun mixed(a: Boolean, b: Int): Double {\n\
    val c = a && (if (b > 0) true else b < -5)\n\
    val d = b * 1.5\n\
    val xs = listOf(b, if (c) b + 1 else try { b / 0 } catch (e: ArithmeticException) { 7 })\n\
    val w = xs.sum().toLong()\n\
    return d + w\n\
}\n\
fun box(): String {\n\
    val r = listOf(spill(1), spill(3), spill(8)).joinToString()\n\
    val t = listOf(thrower(0), thrower(5)).joinToString()\n\
    val m = listOf(mixed(true, 2), mixed(false, -1)).joinToString()\n\
    return if (r == \"7, 9, 24\" && t == \"3, 4\" && m == \"8.0, 4.5\") \"OK\" else \"FAIL: $r | $t | $m\"\n\
}\n";
    assert_eq!(run(src), "OK");
}
