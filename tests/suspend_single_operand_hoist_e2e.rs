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

fn run_box(main: &str) -> String {
    let classpath: Vec<PathBuf> = vec![
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ];
    let jdk = common::jdk_modules();
    common::expect_box_run(main, "Main", &classpath, Some(jdk.as_path()))
}

const DECLARATIONS: &str = "import kotlinx.coroutines.runBlocking\n\
\n\
enum class Level { LOW, HIGH }\n\
\n\
suspend fun pickName(): String = \"HIGH\"\n\
\n\
suspend fun count(): Int = 2\n\
\n";

/// An enum lookup whose name comes from a suspension.
#[test]
fn an_enum_lookup_hoists_a_suspending_name() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun pick(): Level = enumValueOf<Level>(pickName())\n\
fun box(): String {{\n\
\x20   val level = runBlocking {{ pick() }}\n\
\x20   return if (level == Level.HIGH) \"OK\" else \"FAIL: \" + level\n\
}}\n"
    );
    assert_eq!(run_box(&main), "OK");
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
\x20   return if (total == 5) \"OK\" else \"FAIL: \" + total\n\
}}\n"
    );
    assert_eq!(run_box(&main), "OK");
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
    assert_eq!(run_box(&main), "OK");
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
    assert_eq!(run_box(&main), "OK");
}
