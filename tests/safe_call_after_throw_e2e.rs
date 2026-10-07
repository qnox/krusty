//! A safe call whose selector throws keeps the receiver temporary when constant-condition
//! elimination has run, and folds it onto the stack otherwise.
//!
//! kotlinc stores the receiver and folds `aload v; ifnull L; aload v` to `dup` only when nothing
//! falls into `L`. After a loop or an `if`, elimination has deleted the unreachable continuation
//! of `throw` and left a label in front of `L`, so the temporary stays. A method with no `int`
//! jump never runs that elimination, the dead `goto` stays in front of `L`, and the fold runs.

use super::common;

const SRC: &str = "\
fun afterVar(e: Exception?) {\n\
    var lastException: Exception? = e\n\
    lastException?.let { throw it }\n\
}\n\
fun afterIf(flag: Boolean) {\n\
    var lastException: Exception? = null\n\
    if (flag) lastException = Exception()\n\
    lastException?.let { throw it }\n\
}\n\
fun afterLoop(n: Int) {\n\
    var lastException: Exception? = null\n\
    var i = 0\n\
    while (i < n) {\n\
        i++\n\
        lastException = Exception()\n\
    }\n\
    lastException?.let { throw it }\n\
}\n\
fun afterFor(items: List<String>) {\n\
    var lastException: Exception? = null\n\
    for (item in items) {\n\
        try {\n\
            if (item.length < 0) throw Exception()\n\
        } catch (e: Exception) {\n\
            lastException = e\n\
        }\n\
    }\n\
    lastException?.let { throw it }\n\
}\n\
fun note(e: Exception) {}\n\
fun kept(flag: Boolean, e: Exception?) {\n\
    var lastException: Exception? = e\n\
    if (flag) lastException = e\n\
    lastException?.let { note(it) }\n\
}\n\
";

#[test]
fn a_throwing_safe_call_keeps_its_temporary_only_after_an_int_jump() {
    let stored = [
        "public static final void afterIf(",
        "public static final void afterLoop(",
        "public static final void afterFor(",
    ];
    let folded = [
        "public static final void afterVar(",
        "public static final void kept(",
    ];
    let methods = [stored[0], stored[1], stored[2], folded[0], folded[1]];
    let results = common::method_code_diffs_against_kotlinc(
        "SafeCallAfterThrow",
        &[],
        SRC,
        "SafeCallAfterThrowKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    let mut by_method = methods.into_iter().zip(results);
    let mut mismatches = Vec::new();
    for (method, result) in by_method.by_ref().take(stored.len()) {
        if let Err(difference) = result {
            mismatches.push(format!("{method}: {difference}"));
        }
    }
    // `afterVar` folds, and its instructions match. The inlined `it` and its marker end at the
    // null label; the fold moves a local bound standing there to after the `pop`
    // (`a_line_and_a_local_bound_at_the_null_target_move_after_its_pop`), one instruction past
    // where kotlinc leaves them.
    let (after_var, after_var_result) = by_method.next().expect("afterVar");
    match after_var_result {
        Ok(()) => {}
        Err(difference) => {
            let (reference, actual) = split_diff(&difference);
            if instructions(reference) != instructions(actual) {
                mismatches.push(format!("{after_var}: {difference}"));
            }
        }
    }
    // `kept` also folds. kotlinc leaves the `nop` that marked the null continuation; the opcode
    // sequence is otherwise the same, so a selector that does not throw still uses `dup`.
    let (kept, kept_result) = by_method.next().expect("kept");
    match kept_result {
        Ok(()) => {}
        Err(difference) => {
            let (reference, actual) = split_diff(&difference);
            if opcodes(reference) != opcodes(actual) {
                mismatches.push(format!("{kept}: {difference}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// The two disassemblies inside a `method_code_diffs_against_kotlinc` error.
fn split_diff(difference: &str) -> (&str, &str) {
    let rest = difference
        .split_once("--- kotlinc ---\n")
        .expect("kotlinc disassembly")
        .1;
    let (reference, actual) = rest
        .split_once("--- krusty ---\n")
        .expect("krusty disassembly");
    (reference, actual)
}

fn instructions(disassembly: &str) -> &str {
    disassembly
        .split("LocalVariableTable")
        .next()
        .unwrap_or(disassembly)
        .trim()
}

fn opcodes(disassembly: &str) -> Vec<&str> {
    instructions(disassembly)
        .lines()
        .filter_map(|line| {
            let code = line.split("//").next()?.trim();
            let mnemonic = code.split_whitespace().nth(1)?;
            (mnemonic != "nop").then_some(mnemonic)
        })
        .collect()
}

#[test]
fn a_throwing_safe_call_after_a_loop_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SRC}\
             fun box(): String {{\n\
             \x20   afterVar(null)\n\
             \x20   afterIf(false)\n\
             \x20   afterLoop(0)\n\
             \x20   afterFor(emptyList())\n\
             \x20   kept(false, null)\n\
             \x20   kept(true, Exception())\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "throwing safe call after a loop",
    );
}
