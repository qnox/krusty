//! A reified type argument of `Unit` must reach the splicer.
//!
//! A reified `inline fun` is spliced by specializing its `reifiedOperationMarker` against the call's
//! type arguments; with no arguments to specialize, the splice declines and a `MustInline` callee
//! then bails the whole FILE. The map of those arguments was built by asking each for its class
//! name, and `Unit` — unlike `Int` or `String` — has no `Obj` spelling, so it was silently dropped.
//! The map went EMPTY, which reads as "this call supplied no reified arguments", and
//! `suspend fun f(): Unit = response.body()` failed while the identical call at any other result
//! type spliced fine.

use super::common;

/// The failing shape: the reified argument is `Unit`, inferred from the caller's declared result.
#[test]
fn a_reified_unit_result_still_splices() {
    const LIB: &str = "package dep\n\
class Resp(val payload: Any)\n\
inline fun <reified T> Resp.body(): T = payload as T\n\
fun respond(): Resp = Resp(Unit)\n";
    const MAIN: &str = "import dep.body\n\
import dep.respond\n\
class Item\n\
fun unitResult(): Unit = respond().body()\n\
fun box(): String {\n\
\x20   unitResult()\n\
\x20   return \"OK\"\n\
}\n";
    let Some(out) = common::expect_box_run_against_ref("reified_unit_splice", LIB, MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}

/// Control: a reference result still splices, and the spliced body really runs — the `Unit` case
/// must be ADDED to the map, not replace what was already there.
#[test]
fn a_reified_reference_result_still_splices() {
    const LIB: &str = "package dep\n\
class Resp(val payload: Any)\n\
inline fun <reified T> Resp.body(): T = payload as T\n\
fun respondWith(value: Any): Resp = Resp(value)\n";
    const MAIN: &str = "import dep.body\n\
import dep.respondWith\n\
fun stringResult(): String = respondWith(\"ok\").body()\n\
fun box(): String = if (stringResult() == \"ok\") \"OK\" else \"F\"\n";
    let Some(out) = common::expect_box_run_against_ref("reified_ref_splice", LIB, MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
