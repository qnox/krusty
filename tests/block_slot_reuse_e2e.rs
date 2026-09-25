//! Local slots of a block's variables are free again once the block ends, as kotlinc's `FrameMap`
//! makes them: a sibling branch, the next loop, and the statements after a block reuse them.

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
fn same_local_slots(name: &str, src: &str, class: &str) {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip ({name}: no scratch directory)");
        return;
    };
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).unwrap();
    let src_path = dir.join(format!("{name}.kt"));
    std::fs::write(&src_path, src).unwrap();
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip ({name}: reference toolchain unavailable)");
        return;
    };
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(src, name, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{class} was not emitted"));
    let emitted = dir.join(format!("{class}.class"));
    std::fs::write(&emitted, bytes).unwrap();
    let expected = local_variable_rows(&reference.join(format!("{class}.class")));
    assert!(
        !expected.is_empty(),
        "{name}: kotlinc's class has no local variables"
    );
    assert_eq!(local_variable_rows(&emitted), expected, "{name}");
    let _ = std::fs::remove_dir_all(&dir);
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
    );
}
