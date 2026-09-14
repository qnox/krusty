//! A suspension inside a `throw` operand hoists like any other.
//!
//! `throw classify(status, body())` — where `body()` suspends — left the suspension buried in the
//! thrown expression. The state-machine flattener cannot split a suspension there, so it declined the
//! whole function and the backend reported:
//!
//! ```text
//! error: krusty: this suspend-function shape is not yet supported by the IR backend
//! ```
//!
//! Because that verdict is per-FILE, one such `throw` cost a module every class in the file.
//!
//! `throw` evaluates its operand unconditionally and then leaves, exactly like the single-operand
//! statements beside it (`!!`, a `lateinit` check, arithmetic negation), each of which already hoisted
//! its operand. Writing the same expression into a local first, or returning it instead of throwing
//! it, always worked — which is what isolated the `throw` operand itself.

use super::common;
use std::path::PathBuf;

fn run_box(tag: &str, main: &str) -> String {
    let classpath: Vec<PathBuf> = vec![
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ];
    let jdk = common::jdk_modules();
    let _ = tag;
    common::expect_box_run(main, "Main", &classpath, Some(jdk.as_path()))
}

const DECLARATIONS: &str = "import kotlinx.coroutines.runBlocking\n\
\n\
class ApiError(val detail: String) : RuntimeException(detail)\n\
\n\
suspend fun body(): String = \"late\"\n\
\n\
fun classify(status: Int, text: String): Exception = ApiError(status.toString() + \"-\" + text)\n\
\n";

/// The failing shape, reduced: the thrown expression consumes a suspension.
#[test]
fn a_suspension_inside_a_thrown_expression_is_hoisted() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun fail(status: Int): String {{\n\
\x20   throw classify(status, body())\n\
}}\n\
suspend fun run(): String =\n\
\x20   try {{\n\
\x20       fail(7)\n\
\x20   }} catch (e: ApiError) {{\n\
\x20       e.detail\n\
\x20   }}\n\
fun box(): String {{\n\
\x20   val detail = runBlocking {{ run() }}\n\
\x20   return if (detail == \"7-late\") \"OK\" else \"FAIL: \" + detail\n\
}}\n"
    );
    assert_eq!(run_box("suspend_throw_operand", &main), "OK");
}

/// The corpus spelling: the same `throw` guarded by a status check, so the suspension sits inside a
/// conditional branch as well.
#[test]
fn a_conditional_throw_hoists_its_suspension_too() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun check(status: Int): String {{\n\
\x20   if (status > 299) {{\n\
\x20       throw classify(status, body())\n\
\x20   }}\n\
\x20   return \"ok-\" + status\n\
}}\n\
suspend fun run(): String =\n\
\x20   try {{\n\
\x20       check(500)\n\
\x20   }} catch (e: ApiError) {{\n\
\x20       e.detail\n\
\x20   }}\n\
fun box(): String {{\n\
\x20   val detail = runBlocking {{ run() }}\n\
\x20   val fine = runBlocking {{ check(200) }}\n\
\x20   if (detail != \"500-late\") return \"FAIL: \" + detail\n\
\x20   return if (fine == \"ok-200\") \"OK\" else \"FAIL: \" + fine\n\
}}\n"
    );
    assert_eq!(run_box("suspend_conditional_throw", &main), "OK");
}

/// The control that isolates the `throw` operand: binding the suspension to a local first always
/// worked.
#[test]
fn the_same_suspension_bound_to_a_local_still_works() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun fail(status: Int): String {{\n\
\x20   val text = body()\n\
\x20   throw classify(status, text)\n\
}}\n\
suspend fun run(): String =\n\
\x20   try {{\n\
\x20       fail(7)\n\
\x20   }} catch (e: ApiError) {{\n\
\x20       e.detail\n\
\x20   }}\n\
fun box(): String {{\n\
\x20   val detail = runBlocking {{ run() }}\n\
\x20   return if (detail == \"7-late\") \"OK\" else \"FAIL: \" + detail\n\
}}\n"
    );
    assert_eq!(run_box("suspend_throw_local", &main), "OK");
}

/// The other control: the identical expression RETURNED rather than thrown. Same operands, same
/// suspension, and this shape already hoisted.
#[test]
fn the_same_expression_returned_instead_of_thrown_still_works() {
    let main = format!(
        "{DECLARATIONS}\
suspend fun describe(status: Int): String {{\n\
\x20   return (classify(status, body()) as ApiError).detail\n\
}}\n\
fun box(): String {{\n\
\x20   val detail = runBlocking {{ describe(7) }}\n\
\x20   return if (detail == \"7-late\") \"OK\" else \"FAIL: \" + detail\n\
}}\n"
    );
    assert_eq!(run_box("suspend_return_operand", &main), "OK");
}
