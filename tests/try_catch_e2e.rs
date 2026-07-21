//! `try { … } catch (e: E) { … }` as both expression and statement, including a throwing body caught
//! by the handler. Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn try_catch_runs() {
    let src =
        "fun mightThrow(b: Boolean): Int { if (b) throw RuntimeException(\"x\"); return 1 }\n\
fun box(): String {\n\
val r = try { mightThrow(true) } catch (e: RuntimeException) { 42 }\n\
if (r != 42) return \"f1\"\n\
val s = try { mightThrow(false) } catch (e: RuntimeException) { 0 }\n\
if (s != 1) return \"f2\"\n\
val t = \"O\" + try { throw Exception(\"boom\") } catch (e: Exception) { \"K\" }\n\
if (t != \"OK\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "T");
}

/// A `try/catch` as the RHS of arithmetic whose LHS is already on the stack: the exception handler
/// CLEARS the operand stack, so the LHS must be spilled to a temp across the try — it cannot be held
/// on the stack (the keep-LHS-on-stack parity path for branchy RHS must not apply). Both the throwing
/// (handler taken, LHS restored from the temp) and non-throwing paths round-trip under `-Xverify:all`.
#[test]
fn arithmetic_lhs_across_try_catch_rhs() {
    let src =
        "fun mightThrow(b: Boolean): Int { if (b) throw RuntimeException(\"x\"); return 2 }\n\
fun f(x: Int, b: Boolean): Int = x * 31 + try { mightThrow(b) } catch (e: RuntimeException) { 7 }\n\
fun box(): String {\n\
if (f(1, false) != 33) return \"f1\"\n\
if (f(1, true) != 38) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "T");
}
