//! End-to-end "box" coverage for control-flow and expression features: try/catch/finally,
//! `when` as an expression, ranges/progressions, labeled loops, elvis/safe-cast, and nested
//! if-as-expression. Each test compiles a `box()` in-process and round-trips it on the JVM.

mod common;

/// Compile `src`'s `box()` under `stem` and assert it returns "OK". Skips (returns) when the
/// JDK / stdlib toolchain isn't provisioned.
fn check(stem: &str, src: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

#[test]
fn try_catch_finally_value_from_try() {
    let src = "fun box(): String {\n\
var log = StringBuilder()\n\
val r = try {\n\
log.append(\"t\")\n\
42\n\
} catch (e: Exception) {\n\
log.append(\"c\")\n\
0\n\
} finally {\n\
log.append(\"f\")\n\
}\n\
if (r != 42) return \"bad-r\"\n\
if (log.toString() != \"tf\") return \"bad-log:\" + log.toString()\n\
return \"OK\"\n\
}\n";
    check("TryFinallyTry", src);
}

#[test]
fn try_catch_recovers_and_finally_runs() {
    let src = "fun box(): String {\n\
var log = StringBuilder()\n\
val r = try {\n\
log.append(\"t\")\n\
throw RuntimeException(\"boom\")\n\
} catch (e: RuntimeException) {\n\
log.append(\"c\")\n\
7\n\
} finally {\n\
log.append(\"f\")\n\
}\n\
if (r != 7) return \"bad-r\"\n\
if (log.toString() != \"tcf\") return \"bad-log:\" + log.toString()\n\
return \"OK\"\n\
}\n";
    check("TryCatchRecover", src);
}

#[test]
fn multi_catch_selects_right_arm() {
    let src = "fun classify(n: Int): String {\n\
return try {\n\
when (n) {\n\
0 -> throw IllegalStateException(\"s\")\n\
1 -> throw IllegalArgumentException(\"a\")\n\
else -> \"none\"\n\
}\n\
} catch (e: IllegalStateException) {\n\
\"state\"\n\
} catch (e: IllegalArgumentException) {\n\
\"arg\"\n\
}\n\
}\n\
fun box(): String {\n\
if (classify(0) != \"state\") return \"f0\"\n\
if (classify(1) != \"arg\") return \"f1\"\n\
if (classify(2) != \"none\") return \"f2\"\n\
return \"OK\"\n\
}\n";
    check("MultiCatch", src);
}

#[test]
fn nested_try_inner_catch() {
    let src = "fun box(): String {\n\
var sum = 0\n\
try {\n\
try {\n\
throw RuntimeException(\"inner\")\n\
} catch (e: RuntimeException) {\n\
sum += 1\n\
throw IllegalStateException(\"rethrow\")\n\
} finally {\n\
sum += 10\n\
}\n\
} catch (e: IllegalStateException) {\n\
sum += 100\n\
}\n\
if (sum != 111) return \"bad:\" + sum.toString()\n\
return \"OK\"\n\
}\n";
    check("NestedTry", src);
}

#[test]
fn when_expression_assigned_to_val() {
    let src = "fun name(n: Int): String {\n\
val s = when (n) {\n\
1 -> \"one\"\n\
2 -> \"two\"\n\
else -> \"many\"\n\
}\n\
return s\n\
}\n\
fun box(): String {\n\
if (name(1) != \"one\") return \"f1\"\n\
if (name(2) != \"two\") return \"f2\"\n\
if (name(9) != \"many\") return \"f9\"\n\
return \"OK\"\n\
}\n";
    check("WhenValExpr", src);
}

#[test]
fn when_multiple_values_per_arm() {
    let src = "fun kind(n: Int): String {\n\
return when (n) {\n\
1, 3, 5, 7, 9 -> \"odd\"\n\
0, 2, 4, 6, 8 -> \"even\"\n\
else -> \"big\"\n\
}\n\
}\n\
fun box(): String {\n\
if (kind(3) != \"odd\") return \"f3\"\n\
if (kind(4) != \"even\") return \"f4\"\n\
if (kind(11) != \"big\") return \"f11\"\n\
return \"OK\"\n\
}\n";
    check("WhenMultiVal", src);
}

#[test]
fn when_no_subject_boolean_arms() {
    let src = "fun grade(n: Int): String {\n\
return when {\n\
n >= 90 -> \"A\"\n\
n >= 80 -> \"B\"\n\
n >= 70 -> \"C\"\n\
else -> \"F\"\n\
}\n\
}\n\
fun box(): String {\n\
if (grade(95) != \"A\") return \"f95\"\n\
if (grade(85) != \"B\") return \"f85\"\n\
if (grade(72) != \"C\") return \"f72\"\n\
if (grade(10) != \"F\") return \"f10\"\n\
return \"OK\"\n\
}\n";
    check("WhenNoSubject", src);
}

#[test]
fn range_step_and_downto_and_until() {
    let src = "fun box(): String {\n\
var a = 0\n\
for (i in 1..10 step 2) a += i\n\
if (a != 25) return \"step:\" + a.toString()\n\
var b = 0\n\
for (i in 10 downTo 1) b += i\n\
if (b != 55) return \"down:\" + b.toString()\n\
val n = 5\n\
var c = 0\n\
for (i in 0 until n) c += i\n\
if (c != 10) return \"until:\" + c.toString()\n\
return \"OK\"\n\
}\n";
    check("RangeProgressions", src);
}

#[test]
fn range_membership_in_and_not_in() {
    let src = "fun box(): String {\n\
val r = 1..10\n\
if (5 !in r) return \"in5\"\n\
if (11 in r) return \"in11\"\n\
if (!(0 !in r)) return \"notin0\"\n\
var cnt = 0\n\
for (i in 1..100) if (i in 40..60) cnt += 1\n\
if (cnt != 21) return \"cnt:\" + cnt.toString()\n\
return \"OK\"\n\
}\n";
    check("RangeMembership", src);
}

#[test]
fn nested_loops_labeled_break_continue() {
    let src = "fun box(): String {\n\
var hits = 0\n\
outer@ for (i in 1..5) {\n\
for (j in 1..5) {\n\
if (j == 3) continue@outer\n\
if (i == 4) break@outer\n\
hits += 1\n\
}\n\
}\n\
if (hits != 6) return \"bad:\" + hits.toString()\n\
return \"OK\"\n\
}\n";
    check("LabeledLoops", src);
}

#[test]
fn while_and_do_while_values() {
    let src = "fun box(): String {\n\
var i = 0\n\
var s = 0\n\
while (i < 5) { s += i; i += 1 }\n\
if (s != 10) return \"while:\" + s.toString()\n\
var k = 0\n\
var t = 0\n\
do { t += k; k += 1 } while (k < 4)\n\
if (t != 6) return \"do:\" + t.toString()\n\
return \"OK\"\n\
}\n";
    check("WhileDoWhile", src);
}

#[test]
fn elvis_chain_and_bang_bang() {
    let src = "fun pick(a: String?, b: String?, c: String): String {\n\
return a ?: b ?: c\n\
}\n\
fun box(): String {\n\
if (pick(\"x\", \"y\", \"z\") != \"x\") return \"f1\"\n\
if (pick(null, \"y\", \"z\") != \"y\") return \"f2\"\n\
if (pick(null, null, \"z\") != \"z\") return \"f3\"\n\
val nn: String? = \"hi\"\n\
if (nn!!.length != 2) return \"f4\"\n\
return \"OK\"\n\
}\n";
    check("ElvisBangBang", src);
}

#[test]
fn safe_cast_with_fallback() {
    let src = "fun asLen(x: Any?): Int {\n\
val s = x as? String\n\
return s?.length ?: -1\n\
}\n\
fun box(): String {\n\
if (asLen(\"abcd\") != 4) return \"f1\"\n\
if (asLen(42) != -1) return \"f2\"\n\
if (asLen(null) != -1) return \"f3\"\n\
return \"OK\"\n\
}\n";
    check("SafeCastFallback", src);
}

#[test]
fn nested_if_as_expression() {
    let src = "fun sign(n: Int): String {\n\
val s = if (n > 0) \"pos\" else if (n < 0) \"neg\" else \"zero\"\n\
return s\n\
}\n\
fun box(): String {\n\
if (sign(5) != \"pos\") return \"f1\"\n\
if (sign(-3) != \"neg\") return \"f2\"\n\
if (sign(0) != \"zero\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    check("NestedIfExpr", src);
}
