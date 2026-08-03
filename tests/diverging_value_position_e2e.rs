//! A DIVERGING expression used in VALUE position (`boom(): Nothing` as a receiver, an argument, an
//! initializer, an elvis lhs, a block tail). A `Nothing`-returning real call physically returns, so
//! the emitter discards the `Void` and throws `KotlinNothingValueException` — after which the code
//! that consumes the "value" (the store, the outer `invokevirtual`, the method's implicit `return`)
//! is unreachable straight-line bytecode with no stack-map frame. The verifier rejects those classes
//! ("Expecting a stack map frame" / "Operand stack overflow") even though kotlinc accepts and runs
//! the same sources.
//!
//! The fix is generic: dead bytecode between an unconditional terminator and the next bound label is
//! never emitted, so no construct-specific divergence check is needed at each consuming site. Every
//! case below is the SAME defect through a different consuming construct — they are separate tests so
//! a partial fix (e.g. only the safe-call arm) can't pass the file.
use super::common;

/// The reported shape: a statically-`null` safe-call receiver whose evaluation DIVERGES. The
/// lowerer folds a `Ty::Null` receiver to `{ evaluate receiver; null }`, so the null constant and
/// the enclosing method's implicit `return` sat after the receiver's `athrow`.
#[test]
fn safe_call_on_diverging_null_receiver() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1() { (if (true) { boom(); null } else null)?.hashCode() }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_safe_call_null_recv");
}

/// A safe call whose receiver is itself `Nothing` (diverging): the same always-null fold applies —
/// `Nothing` has no non-null value — and the receiver terminates the path.
#[test]
fn safe_call_on_diverging_nothing_receiver() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): String? = boom()?.toString()\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_safe_call_nothing_recv");
}

/// One operator over: a diverging elvis lhs in VALUE position. `expr_inner_elvis` already returns
/// the lhs alone for a diverging `Nothing` lhs — correct IR — but the `istore` for the initializer
/// still followed the `athrow`.
#[test]
fn elvis_with_diverging_lhs_in_value_position() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): Int { val y: Int = boom() ?: 1; return y }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_elvis_value_position");
}

/// Statement position for the same elvis — already accepted before the fix; locks it against a
/// regression from the dead-code suppression.
#[test]
fn elvis_with_diverging_lhs_in_statement_position() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1() { boom() ?: 1 }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_elvis_statement_position");
}

/// Argument position: the outer `invokevirtual println` followed the terminator, and its operands
/// were counted into `max_stack` from an already-emptied stack ("Operand stack overflow").
#[test]
fn diverging_call_in_argument_position() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1() { println(boom()) }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_argument_position");
}

/// Receiver position: `boom().toString()`.
#[test]
fn diverging_call_in_receiver_position() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): String { val s: String = boom().toString(); return s }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_receiver_position");
}

/// A block whose NON-LAST statement diverges, used as a value (`if (true) { boom(); 1 } else 2`).
/// The constant condition is folded away in lowering, so the `istore` for the initializer trails
/// the terminator with no label between.
#[test]
fn diverging_statement_inside_value_block() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): Int { val x: Int = (if (true) { boom(); 1 } else 2); return x }\n\
        fun box(): String {\n\
            return try { run1(); \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") \"OK\" else \"F:\" + e.message }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_value_block");
}

/// A BRANCHY sibling still inside the dead region: the diverging argument is evaluated first, so the
/// second argument's whole control-flow construct lands in dropped bytecode. Binding its labels must
/// not resurrect the construct's tail around the hole where its condition used to be — a bind revives
/// only when some ALREADY-EMITTED branch targets the label, and a branch emitted while dead was itself
/// dropped. Covers the `if`/`when`/`&&`/`try` spellings plus inline-spliced bodies, whose relocated
/// frames sit inside the spliced bytes and so cannot serve as an entry frame.
#[test]
fn diverging_sibling_of_a_branchy_argument() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun g(a: Any?, b: Int): Int = b\n\
        fun f1(b: Boolean): Int = g(boom(), if (b) 1 else 2)\n\
        fun f2(b: Boolean): Int = g(boom(), when { b -> 1; else -> 2 })\n\
        fun f3(b: Boolean, s: String?): Int = g(boom(), if (b && s != null) 1 else 2)\n\
        fun f4(): Int = g(boom(), try { 1 } catch (e: Exception) { 2 })\n\
        fun f5(): Int = g(boom(), 5.let { it + 1 })\n\
        fun f6(): Int = g(boom(), listOf(1, 2).fold(0) { a, x -> a + x })\n\
        fun f7(): Int = g(boom(), listOf(1, 2).map { it + 1 }.size)\n\
        fun box(): String {\n\
            var n = 0\n\
            val fs = listOf<() -> Int>({ f1(true) }, { f2(true) }, { f3(true, \"x\") }, { f4() }, { f5() }, { f6() }, { f7() })\n\
            for (f in fs) {\n\
                try { f(); return \"F:no-throw\" } catch (e: RuntimeException) { if (e.message == \"boom\") n++ }\n\
            }\n\
            return if (n == 7) \"OK\" else \"F:\" + n\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_branchy_sibling");
}

/// The inverse of the case above: a LIVE `try` whose body diverges leaves the stream dead exactly at
/// the handler, which is reached over the exception edge and has no incoming branch to revive on. The
/// handler must still be emitted — it is the whole point of the `try`.
#[test]
fn diverging_try_body_handler_is_still_emitted() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): String = try { boom() } catch (e: RuntimeException) { \"caught:\" + e.message }\n\
        fun run2(): String {\n\
            val sb = StringBuilder()\n\
            try { sb.append(\"a\"); boom() } finally { sb.append(\"b\") }\n\
            return sb.toString()\n\
        }\n\
        fun box(): String {\n\
            if (run1() != \"caught:boom\") return \"F:\" + run1()\n\
            val fin = try { run2(); \"no-throw\" } catch (e: RuntimeException) { e.message ?: \"?\" }\n\
            return if (fin == \"boom\") \"OK\" else \"F:\" + fin\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_try_handler_emitted");
}

/// Regression lock for the surrounding machinery: a `when` whose branches diverge on one side only
/// still merges — the non-diverging branch's `goto` and the `end` label must survive suppression.
#[test]
fn one_diverging_branch_still_merges() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun pick(b: Boolean): Int = if (b) boom() else 7\n\
        fun box(): String {\n\
            val ok = pick(false) == 7\n\
            val threw = try { pick(true); false } catch (e: RuntimeException) { true }\n\
            return if (ok && threw) \"OK\" else \"F:\" + ok + \"/\" + threw\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_one_branch_merge");
}

/// A diverging `try` body with a live `catch`: the handler is a bound label, so suppression must
/// stop there and the protected range must stay non-degenerate.
#[test]
fn diverging_try_body_with_live_catch() {
    const SRC: &str = "fun boom(): Nothing = throw RuntimeException(\"boom\")\n\
        fun run1(): Int { val v: Int = try { boom() } catch (e: RuntimeException) { 5 }; return v }\n\
        fun box(): String = if (run1() == 5) \"OK\" else \"F:\" + run1()\n";
    common::expect_box_ok_with_stdlib(SRC, "diverging_try_live_catch");
}
