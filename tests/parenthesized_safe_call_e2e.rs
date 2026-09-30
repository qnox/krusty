//! An unparenthesized safe-call selector includes the following index, assignment, and update.
//!
//! `arg?.alias[i]` indexes the non-null member. `(arg?.alias)[i]` and `(arg?.alias[i])` index or
//! update the nullable result, so an extension on `Foo?` wins over `Foo`'s member operator.
//! Parentheses inside an index, a safe selector nested in an index, and a second safe index on
//! the right-hand side stay inside the outer null check.

use super::common;

fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

#[test]
fn a_parenthesized_safe_call_selects_the_nullable_operator() {
    agrees_with_kotlinc(
        "ParenthesizedSafeCallOperators",
        "var result = \"none\"\n\
         var marks = 0\n\
         fun member(name: String) { result = \"$name: member\" }\n\
         fun extension(name: String) { result = \"$name: extension\" }\n\
         fun mark(n: Int): Int { marks += 1; return n }\n\
         fun checkResult(s: String) {\n\
         \x20   if (s != result) throw RuntimeException(\"fail: $s, but $result\")\n\
         }\n\
         class Payload(val n: Int)\n\
         class Foo {\n\
         \x20   var alias: Foo = this\n\
         \x20   operator fun get(index: Int): Foo { member(\"get\"); return this }\n\
         \x20   operator fun set(index: Int, arg: Foo) { member(\"set\") }\n\
         \x20   operator fun plusAssign(arg: Payload) { member(\"plusAssign\") }\n\
         \x20   operator fun inc(): Foo { member(\"inc\"); return this }\n\
         \x20   operator fun invoke(arg: String) { member(\"invoke\") }\n\
         }\n\
         operator fun Foo?.get(index: Int): Foo? { extension(\"get\"); return this }\n\
         operator fun Foo?.set(index: Int, arg: Foo?) { extension(\"set\") }\n\
         operator fun Foo?.plusAssign(arg: Payload) { extension(\"plusAssign\") }\n\
         operator fun Foo?.inc(): Foo { extension(\"inc\"); return this!! }\n\
         operator fun Foo?.invoke(arg: String) { extension(\"invoke\") }\n\
         class Bar {\n\
         \x20   var slot: Bar = this\n\
         \x20   operator fun get(index: Int): Bar { member(\"get\"); return this }\n\
         \x20   operator fun set(index: Int, value: Bar?) { member(\"set\") }\n\
         }\n\
         fun huh(arg: Foo?) {\n\
         \x20   arg?.alias[42]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   arg?.alias[42] = arg\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   arg?.alias += Payload(0)\n\
         \x20   checkResult(\"plusAssign: member\")\n\
         \x20   arg?.alias++\n\
         \x20   checkResult(\"inc: member\")\n\
         \x20   ++arg?.alias\n\
         \x20   checkResult(\"inc: member\")\n\
         \x20   arg?.alias(\"\")\n\
         \x20   checkResult(\"invoke: member\")\n\
         \x20   arg?.alias[42] += Payload(0)\n\
         \x20   checkResult(\"plusAssign: member\")\n\
         \x20   arg?.alias[42]++\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   ++arg?.alias[42]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   (arg?.alias)[42]\n\
         \x20   checkResult(\"get: extension\")\n\
         \x20   (arg?.alias)[42] = arg\n\
         \x20   checkResult(\"set: extension\")\n\
         \x20   (arg?.alias) += Payload(0)\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias)(\"\")\n\
         \x20   checkResult(\"invoke: extension\")\n\
         \x20   (arg?.alias)[42] += Payload(0)\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias[42]) += Payload(0)\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias[42])++\n\
         \x20   checkResult(\"set: extension\")\n\
         \x20   ++(arg?.alias[42])\n\
         \x20   checkResult(\"get: extension\")\n\
         }\n\
         fun nested(left: Bar?, right: Bar?) {\n\
         \x20   left?.slot[(0)]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   marks = 0\n\
         \x20   left?.slot[mark(1)] = right?.slot[mark(2)]\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   if (marks != 2) throw RuntimeException(\"fail marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   val absent: Bar? = null\n\
         \x20   absent?.slot[mark(1)] = right?.slot[mark(2)]\n\
         \x20   if (marks != 0) throw RuntimeException(\"fail null marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   left?.slot[right?.slot[mark(3)]?.let { 4 } ?: 5]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   if (marks != 1) throw RuntimeException(\"fail index marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   absent?.slot[right?.slot[mark(3)]?.let { 4 } ?: 5]\n\
         \x20   if (marks != 0) throw RuntimeException(\"fail null index marks $marks\")\n\
         }\n\
         fun box(): String {\n\
         \x20   huh(Foo())\n\
         \x20   nested(Bar(), Bar())\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn a_safe_index_update_logs_each_call_in_order() {
    agrees_with_kotlinc(
        "SafeIndexEvaluationOrder",
        r#"
var log = ""
fun note(event: String) { log += event + ";" }
fun recv(value: Holder?): Holder? { note("recv"); return value }
fun idx(): Int { note("idx"); return 0 }
fun payload(): Payload { note("payload"); return Payload(0) }
fun rhs(): Snap { note("rhs"); return Snap(7) }
class Payload(val n: Int)
class Snap(val n: Int) {
    operator fun inc(): Snap { note("inc"); return Snap(n + 1) }
    operator fun plusAssign(arg: Payload) { note("plusAssign") }
}
class Cell(var n: Int) {
    operator fun get(index: Int): Snap { note("get"); return Snap(n) }
    operator fun set(index: Int, value: Snap) { note("set"); n = value.n }
    operator fun plusAssign(arg: Payload) { note("cellPlus") }
}
class Holder {
    var stored = Cell(0)
    var cell: Cell
        get() { note("getter"); return stored }
        set(value) { stored = value }
}
fun expect(label: String, expected: String) {
    if (log != expected) throw RuntimeException("$label [$log]")
    log = ""
}
fun box(): String {
    val present = Holder()
    val post = recv(present)?.cell[idx()]++
    expect("postfix", "recv;getter;idx;get;inc;set;")
    if (post?.n != 0) return "postfix-value ${post?.n}"
    present.stored.n = 0
    val pre = ++recv(present)?.cell[idx()]
    expect("prefix", "recv;getter;idx;get;inc;set;get;")
    if (pre?.n != 1) return "prefix-value ${pre?.n}"
    present.stored.n = 0
    recv(null)?.cell[idx()]++
    expect("null-postfix", "recv;")
    ++recv(null)?.cell[idx()]
    expect("null-prefix", "recv;")
    recv(present)?.cell[idx()] += payload()
    expect("index-plus", "recv;getter;idx;get;payload;plusAssign;")
    recv(null)?.cell[idx()] += payload()
    expect("null-index-plus", "recv;")
    recv(present)?.cell += payload()
    expect("member-plus", "recv;getter;payload;cellPlus;")
    recv(null)?.cell += payload()
    expect("null-member-plus", "recv;")
    recv(present)?.cell[idx()] = rhs()
    expect("assign", "recv;getter;idx;rhs;set;")
    if (present.stored.n != 7) return "assign-value ${present.stored.n}"
    present.stored.n = 0
    recv(null)?.cell[idx()] = rhs()
    expect("null-assign", "recv;")
    return "OK"
}
"#,
    );
}

#[test]
fn a_safe_member_plus_updates_the_non_null_property() {
    agrees_with_kotlinc(
        "SafeMemberPlus",
        "class A(var value: Int)\n\
         operator fun A?.plus(other: A): A = A((this?.value ?: 0) + other.value)\n\
         class B(var a: A)\n\
         fun box(): String {\n\
         \x20   var present: B? = B(A(11))\n\
         \x20   present?.a += A(31)\n\
         \x20   if (present?.a?.value != 42) return \"FAIL ${present?.a?.value}\"\n\
         \x20   var absent: B? = null\n\
         \x20   absent?.a += A(1)\n\
         \x20   if (absent != null) return \"FAIL null\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}
