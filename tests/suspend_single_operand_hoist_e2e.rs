//! The suspend hoister descends into every single-operand node, not a subset of them.
//!
//! `hoist_expr` recurses through one IR node kind per arm, and two nodes had no arm at all:
//!
//! * `enumValueOf<Level>(pickName())` — the enum lookup's operand;
//! * `var total = count()` where a lambda captures `total`, which boxes the local into a
//!   `Ref` holder whose INITIALIZER then held the suspension.
//!
//! Both left the suspension buried where the state-machine flattener cannot split it, so the backend
//! declined the whole function:
//!
//! ```text
//! error: krusty: this suspend-function shape is not yet supported by the IR backend
//! ```
//!
//! That verdict is per-FILE, so either shape cost a module every class in the file. Each operand
//! evaluates unconditionally before the node it feeds, so each hoists to a preceding temp exactly as
//! the neighbouring arms already do.

use super::common;
use std::path::PathBuf;

fn classpath() -> Vec<PathBuf> {
    vec![
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ]
}

/// Compile and run the fixture with the REFERENCE compiler, on the same classpath.
///
/// `kotlinc_box_result` cannot serve here: it builds a stdlib-only classpath and these fixtures need
/// the coroutines runtime for `runBlocking`.
fn reference_box(tag: &str, main: &str) -> String {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{tag}: cannot allocate a scratch directory"))
        .join(tag);
    std::fs::create_dir_all(&work).expect("create reference fixture directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, main).expect("write reference fixture");
    let out = work.join("reference-classes");
    let joined = std::env::join_paths(classpath()).expect("join reference classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{tag}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{tag}: kotlinc rejected the fixture: {diagnostics}"
    );
    let mut run_cp = vec![out];
    run_cp.extend(classpath());
    common::run_box(&[], "MainKt", &run_cp)
        .unwrap_or_else(|| panic!("{tag}: the reference-built box() failed to run"))
}

/// Run one fixture under BOTH compilers and require the same `box()` value. Asserting only that
/// krusty returns `OK` proves krusty agrees with itself; these shapes are about matching the
/// reference compiler's hoisting of a suspension in an operand position.
fn run_box_with(tag: &str, main: &str) -> String {
    let reference = reference_box(tag, main);
    assert_eq!(
        reference, "OK",
        "{tag}: the reference compiler disagrees: {reference}"
    );
    let jdk = common::jdk_modules();
    common::expect_box_run(main, "Main", &classpath(), Some(jdk.as_path()))
}

/// Every suspending helper here calls `yield()`, so it genuinely SUSPENDS and resumes rather than
/// completing synchronously. That is the difference that matters: a helper which returns without
/// suspending never forces the flattener to split the state machine at that operand, so it cannot
/// show whether the operand was hoisted. Each helper also bumps `calls`, which the fixtures assert,
/// so a hoist that evaluates its operand twice — or not at all — is observable rather than hidden
/// behind a result that happens to be the same either way.
const DECLARATIONS: &str = "import kotlinx.coroutines.runBlocking\n\
import kotlinx.coroutines.yield\n\
\n\
enum class Level { LOW, HIGH }\n\
\n\
var calls = 0\n\
\n\
suspend fun pickName(): String { calls++; yield(); return \"HIGH\" }\n\
\n\
suspend fun count(): Int { calls++; yield(); return 2 }\n\
\n\
suspend fun size(): Int { calls++; yield(); return 3 }\n\
\n\
suspend fun make(): Any { calls++; yield(); return \"text\" }\n\
\n";

/// An enum lookup whose name comes from a suspension.
#[test]
fn an_enum_lookup_hoists_a_suspending_name() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun pick(): Level = enumValueOf<Level>(pickName())\n\
fun box(): String {{\n\
\x20   val level = runBlocking {{ pick() }}\n\
\x20   if (calls != 1) return \"FAIL: evaluated \" + calls + \" times\"\n\
\x20   return if (level == Level.HIGH) \"OK\" else \"FAIL: \" + level\n\
}}\n"
    );
    assert_eq!(
        run_box_with("an_enum_lookup_hoists_a_suspending_name", &main),
        "OK"
    );
}

/// A captured mutable local whose INITIALIZER suspends. The capture is what turns the local into a
/// `Ref` holder, so the suspension ends up inside the holder's construction.
#[test]
fn a_captured_local_hoists_a_suspending_initializer() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun tally(): Int {{\n\
\x20   var total = count()\n\
\x20   val bump = {{ total += 3 }}\n\
\x20   bump()\n\
\x20   return total\n\
}}\n\
fun box(): String {{\n\
\x20   val total = runBlocking {{ tally() }}\n\
\x20   if (calls != 1) return \"FAIL: evaluated \" + calls + \" times\"\n\
\x20   return if (total == 5) \"OK\" else \"FAIL: \" + total\n\
}}\n"
    );
    assert_eq!(
        run_box_with("a_captured_local_hoists_a_suspending_initializer", &main),
        "OK"
    );
}

/// The control that isolates the initializer: the same captured local assigned from a suspension
/// LATER always worked, because the assignment is an ordinary holder write rather than the holder's
/// construction.
#[test]
fn a_captured_local_assigned_from_a_suspension_later_still_works() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun tally(): Int {{\n\
\x20   var total = 0\n\
\x20   val bump = {{ total += 3 }}\n\
\x20   bump()\n\
\x20   total += count()\n\
\x20   return total\n\
}}\n\
fun box(): String {{\n\
\x20   val total = runBlocking {{ tally() }}\n\
\x20   return if (total == 5) \"OK\" else \"FAIL: \" + total\n\
}}\n"
    );
    assert_eq!(
        run_box_with(
            "a_captured_local_assigned_from_a_suspension_later_still_works",
            &main
        ),
        "OK"
    );
}

/// The control for the enum shape: an UNcaptured local and a non-suspending lookup name, so neither
/// node carries a suspension.
#[test]
fn an_enum_lookup_with_no_suspension_still_works() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun pick(): Level {{\n\
\x20   val bump = count()\n\
\x20   val chosen = enumValueOf<Level>(\"LOW\")\n\
\x20   return if (bump > 0) chosen else Level.HIGH\n\
}}\n\
fun box(): String {{\n\
\x20   val level = runBlocking {{ pick() }}\n\
\x20   return if (level == Level.LOW) \"OK\" else \"FAIL: \" + level\n\
}}\n"
    );
    assert_eq!(
        run_box_with("an_enum_lookup_with_no_suspension_still_works", &main),
        "OK"
    );
}

/// An array whose SIZE suspends. The size is evaluated before the array exists, so the suspension
/// cannot stay where it is: `NewArray` had no arm at all.
#[test]
fn an_array_size_hoists_a_suspension() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun build(): Array<String?> = arrayOfNulls<String>(size())\n\
fun box(): String {{\n\
\x20   val made = runBlocking {{ build() }}\n\
\x20   if (calls != 1) return \"FAIL: evaluated \" + calls + \" times\"\n\
\x20   if (made.size != 3) return \"FAIL: size \" + made.size\n\
\x20   return if (made[0] == null) \"OK\" else \"FAIL: element\"\n\
}}\n"
    );
    assert_eq!(run_box_with("an_array_size_hoists_a_suspension", &main), "OK");
}

/// A BOUND class literal whose receiver suspends. `make()::class` evaluates the receiver for its
/// runtime class, so the suspension is a single operand like any other; `KClassLiteral` had no arm.
/// An unbound `String::class` carries no operand and cannot suspend, which is why the arm matches
/// only the bound form.
#[test]
fn a_bound_class_literal_hoists_a_suspending_receiver() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun runtimeClass(): String = make()::class.simpleName ?: \"?\"\n\
fun box(): String {{\n\
\x20   val name = runBlocking {{ runtimeClass() }}\n\
\x20   if (calls != 1) return \"FAIL: evaluated \" + calls + \" times\"\n\
\x20   return if (name == \"String\") \"OK\" else \"FAIL: \" + name\n\
}}\n"
    );
    assert_eq!(
        run_box_with("a_bound_class_literal_hoists_a_suspending_receiver", &main),
        "OK"
    );
}

/// Two single-operand nodes fed by SEPARATE suspensions in one expression. Each operand must be
/// hoisted to its own temp, in evaluation order, and each must run exactly once — the shape that a
/// hoist rewriting a shared arena node in place would get wrong.
#[test]
fn two_operands_in_one_expression_each_hoist_once() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun both(): Int = arrayOfNulls<String>(size()).size + count()\n\
fun box(): String {{\n\
\x20   val total = runBlocking {{ both() }}\n\
\x20   if (calls != 2) return \"FAIL: evaluated \" + calls + \" times\"\n\
\x20   return if (total == 5) \"OK\" else \"FAIL: \" + total\n\
}}\n"
    );
    assert_eq!(
        run_box_with("two_operands_in_one_expression_each_hoist_once", &main),
        "OK"
    );
}
