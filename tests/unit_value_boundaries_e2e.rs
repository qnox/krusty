//! A `Unit` operation that leaves nothing on the JVM stack still has the `kotlin.Unit` value.
//!
//! A function-value invocation (`f()` for `f: () -> Unit`) discards the `Object` its `invoke`
//! returns, and a `Unit` `when` or `try` runs its branches for effect. Wherever such a value is
//! stored, passed, returned or used as a receiver, the consumer must receive `Unit.INSTANCE`, as
//! kotlinc's `StackValue.coerce` from `void` pushes it. krusty left the stack empty and failed
//! frame computation. A property initializer is its body's value, so an initializer `when` must
//! not be discarded as a statement either.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "var sink: Any? = null\n\
var log = \"\"\n\
fun effect() { log += \"e\" }\n\
fun invokedValue(f: () -> Unit) { sink = f() }\n\
fun invokedBranch(c: Boolean, f: () -> Unit) { sink = if (c) f() else \"no\" }\n\
fun whenValue(n: Int) { sink = when (n) { 0 -> effect() else -> effect() } }\n\
fun tryValue() { sink = try { effect() } catch (e: Throwable) { } }\n\
fun tryFinallyValue() { sink = try { effect() } finally { log += \"f\" } }\n\
class Holder(val p: Int) {\n\
    val kind: String = when { p == 0 -> \"zero\" else -> \"other\" }\n\
}\n\
val topKind: String = when { log == \"\" -> \"empty\" else -> \"full\" }\n\
val Unit.marker: Int get() = 7\n\
fun invokedReceiver(f: () -> Unit): Int = f().marker\n";

#[test]
fn unit_values_reach_their_consumers() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (topKind != \"empty\") return \"fail topKind \" + topKind\n\
             \x20   invokedValue {{ effect() }}\n\
             \x20   if (sink !== Unit) return \"fail invokedValue\"\n\
             \x20   sink = null\n\
             \x20   invokedBranch(true) {{ effect() }}\n\
             \x20   if (sink !== Unit) return \"fail invokedBranch\"\n\
             \x20   invokedBranch(false) {{ effect() }}\n\
             \x20   if (sink != \"no\") return \"fail invokedBranch else\"\n\
             \x20   sink = null\n\
             \x20   whenValue(1)\n\
             \x20   if (sink !== Unit) return \"fail whenValue\"\n\
             \x20   sink = null\n\
             \x20   tryValue()\n\
             \x20   if (sink !== Unit) return \"fail tryValue\"\n\
             \x20   sink = null\n\
             \x20   tryFinallyValue()\n\
             \x20   if (sink !== Unit) return \"fail tryFinallyValue\"\n\
             \x20   if (Holder(0).kind != \"zero\" || Holder(1).kind != \"other\") return \"fail Holder\"\n\
             \x20   if (invokedReceiver {{ effect() }} != 7) return \"fail invokedReceiver\"\n\
             \x20   if (log != \"eeeeefe\") return \"fail log \" + log\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "UnitValues",
    );
}

#[test]
fn unit_values_are_materialized_where_kotlinc_materializes_them() {
    let built = compare_with_kotlinc_plugin(
        "UnitValues",
        SOURCE,
        "UnitValuesKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for member in [
        "void invokedValue(",
        "void invokedBranch(",
        "void whenValue(",
        "int invokedReceiver(",
        "static {}",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc: {member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}
