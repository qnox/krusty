//! A reified type argument survives the value-class representation wrapper.
//!
//! A reified `inline fun` is spliced by specializing its `reifiedOperationMarker` against the call's
//! type arguments; with none to specialize the splice declines, and a `MustInline` callee then bails
//! the whole FILE.
//!
//! Value-class lowering moves a call BELOW a representation wrapper, cloning it to a new expression.
//! That clone carried the call's physical/logical types and its suspension identity, but not its
//! reified type arguments — so the splicer saw a call with none and declined. Only a value class
//! whose underlying is NULLABLE is wrapped this way, which is why the same call spliced for every
//! other result type, a non-nullable value class included.

use super::common;

/// The failing shape: the reified argument is a value class with a nullable underlying.
#[test]
fn a_reified_nullable_value_class_result_still_splices() {
    const LIB: &str = "package dep\n\
class Resp(val payload: Any?)\n\
inline fun <reified T> Resp.body(): T = payload as T\n\
fun respond(value: Any?): Resp = Resp(value)\n";
    const MAIN: &str = "import dep.body\n\
import dep.respond\n\
@JvmInline\n\
value class Wrapped(val placeholder: String?)\n\
fun wrapped(): Wrapped = respond(Wrapped(\"ok\")).body()\n\
fun box(): String = if (wrapped().placeholder == \"ok\") \"OK\" else \"F\"\n";
    let Some(out) = common::expect_box_run_against_ref("reified_nullable_value_class", LIB, MAIN)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}

/// Control: a value class with a NON-nullable underlying takes no wrapper and still splices, so the
/// fix must carry the arguments through the wrapper rather than change how they are recorded.
#[test]
fn a_reified_non_null_value_class_result_still_splices() {
    const LIB: &str = "package dep\n\
class Resp(val payload: Any?)\n\
inline fun <reified T> Resp.body(): T = payload as T\n\
fun respond(value: Any?): Resp = Resp(value)\n";
    const MAIN: &str = "import dep.body\n\
import dep.respond\n\
@JvmInline\n\
value class Plain(val raw: String)\n\
fun plain(): Plain = respond(Plain(\"ok\")).body()\n\
fun box(): String = if (plain().raw == \"ok\") \"OK\" else \"F\"\n";
    let Some(out) = common::expect_box_run_against_ref("reified_non_null_value_class", LIB, MAIN)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
