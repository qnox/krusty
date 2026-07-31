//! Contract- and flow-based smart casts kotlinc applies but krusty dropped:
//!
//!  * `!x.isNullOrEmpty()` / `!x.isNullOrBlank()` — the stdlib `kotlin.text` contract
//!    `returns(false) implies (receiver != null)` narrows a stable nullable receiver.
//!  * `!cond` negation inverts branch facts (also De Morgan through `&&`/`||`).
//!  * `b?.prop != null` — a non-null safe-call result narrows the safe-call RECEIVER.
//!  * `requireNotNull(x)` / `checkNotNull(x)` — `returns() implies (x != null)` narrows a stable
//!    nullable first argument for the rest of the block (like the `require`/`check` guards).
//!  * `while (x != null)` narrows the loop body like an `if` then-branch.
//!
//! Every form is round-tripped on the JVM; shadowing user declarations must NOT narrow.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_rejected(src: &str) {
    assert!(
        common::compile_and_run_with_stdlib(src, "Main").is_none(),
        "source should be rejected, but compiled successfully:\n{src}"
    );
}

#[test]
fn not_is_null_or_empty_narrows_both_receivers() {
    const SRC: &str = "fun extract(a: String?, b: String?): Pair<String, String>? {\n\
    return if (!a.isNullOrEmpty() && !b.isNullOrEmpty()) a to b else null\n\
}\n\
fun box(): String {\n\
    val p = extract(\"iss\", \"sub\") ?: return \"FAIL\"\n\
    return if (p.first == \"iss\" && p.second == \"sub\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("contract smartcast compiles + runs"), "OK");
}

#[test]
fn is_null_or_blank_false_branch_narrows() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    if (s.isNullOrBlank()) return -1\n\
    return s.length\n\
}\n\
fun box(): String = if (f(\"hi\") == 2 && f(null) == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("isNullOrBlank smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn or_condition_false_branch_narrows() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    if (s == null || s.isNullOrEmpty()) return -1\n\
    return s.length\n\
}\n\
fun box(): String = if (f(\"hey\") == 3 && f(null) == -1 && f(\"\") == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("|| else-branch smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn negated_is_narrows_else_branch() {
    const SRC: &str = "fun f(x: Any): Int {\n\
    if (!(x is String)) return 0\n\
    return x.length\n\
}\n\
fun box(): String = if (f(\"abcd\") == 4 && f(1) == 0) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("!(x is T) smartcast compiles + runs"), "OK");
}

#[test]
fn safe_call_non_null_result_narrows_receiver() {
    const SRC: &str = "class B(val prop: String?)\n\
fun f(b: B?): String {\n\
    if (b?.prop != null) return b.prop!!\n\
    return \"none\"\n\
}\n\
fun box(): String = if (f(B(\"x\")) == \"x\" && f(null) == \"none\") \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("?. != null smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn safe_call_null_result_else_narrows_receiver() {
    const SRC: &str = "class B(val prop: String?)\n\
fun f(b: B?): Int {\n\
    if (b?.prop == null) return -1\n\
    return b.prop!!.length\n\
}\n\
fun box(): String = if (f(B(\"xy\")) == 2 && f(B(null)) == -1 && f(null) == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("?. == null else smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn require_not_null_narrows_the_argument() {
    const SRC: &str = "fun f(a: String?): Int {\n\
    requireNotNull(a)\n\
    return a.length\n\
}\n\
fun box(): String = if (f(\"hello\") == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("requireNotNull smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn check_not_null_narrows_the_argument() {
    const SRC: &str = "fun f(a: String?): Int {\n\
    checkNotNull(a)\n\
    return a.length\n\
}\n\
fun box(): String = if (f(\"hey\") == 3) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("checkNotNull smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn require_not_null_result_is_non_null() {
    const SRC: &str = "fun f(a: String?): String = requireNotNull(a)\n\
fun g(a: String?): String = checkNotNull(a) { \"missing\" }\n\
fun box(): String = if (f(\"ab\").length == 2 && g(\"abc\").length == 3) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("requireNotNull T: Any binding compiles + runs"),
        "OK"
    );
}

#[test]
fn require_with_contract_condition_narrows() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    require(!s.isNullOrEmpty())\n\
    return s.length\n\
}\n\
fun box(): String = if (f(\"hi\") == 2) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("require(!isNullOrEmpty) smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn while_condition_narrows_the_body() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    var out = 0\n\
    while (s != null) {\n\
        out = s.length\n\
        break\n\
    }\n\
    return out\n\
}\n\
fun box(): String = if (f(\"abcd\") == 4 && f(null) == 0) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("while-condition smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn user_is_null_or_empty_does_not_apply_stdlib_contract() {
    const SRC: &str = "fun String?.isNullOrEmpty(): Boolean = false\n\
fun nullable(): String? = \"x\"\n\
fun box(): String {\n\
    val s: String? = nullable()\n\
    if (!s.isNullOrEmpty()) return if (s.length == 1) \"OK\" else \"FAIL\"\n\
    return \"FAIL\"\n\
}\n";
    assert_rejected(SRC);
}

#[test]
fn local_require_not_null_does_not_apply_stdlib_contract() {
    const SRC: &str = "fun nullable(): String? = \"x\"\n\
fun box(): String {\n\
    fun requireNotNull(x: Any?) {}\n\
    val a: String? = nullable()\n\
    requireNotNull(a)\n\
    return if (a.length == 1) \"OK\" else \"FAIL\"\n\
}\n";
    assert_rejected(SRC);
}

#[test]
fn var_receiver_does_not_narrow() {
    const SRC: &str = "fun box(): String {\n\
    var s: String? = \"x\"\n\
    if (!s.isNullOrEmpty()) return if (s.length == 1) \"OK\" else \"FAIL\"\n\
    return \"FAIL\"\n\
}\n";
    assert_rejected(SRC);
}

#[test]
fn nested_safe_call_narrows_root_receiver() {
    const SRC: &str = "class C(val v: Int)\nclass B(val c: C?)\n\
fun f(b: B?): Int {\n\
    if (b?.c?.v != null) return b.c!!.v\n\
    return 0\n\
}\n\
fun box(): String = if (f(B(C(7))) == 7 && f(B(null)) == 0 && f(null) == 0) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("a?.b?.c != null smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn var_safe_call_receiver_does_not_narrow() {
    const SRC: &str = "class B(val prop: String?)\n\
fun box(): String {\n\
    var b: B? = B(\"x\")\n\
    if (b?.prop != null) return if (b.prop == \"x\") \"OK\" else \"FAIL\"\n\
    return \"FAIL\"\n\
}\n";
    assert_rejected(SRC);
}

#[test]
fn de_morgan_then_branch_narrows() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    if (!(s == null || s.isNullOrEmpty())) return s.length\n\
    return -1\n\
}\n\
fun box(): String = if (f(\"hey\") == 3 && f(null) == -1 && f(\"\") == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("De Morgan smartcast compiles + runs"), "OK");
}

#[test]
fn while_narrowing_does_not_leak_past_the_loop() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    while (s != null) {\n        return s.length\n    }\n\
    return s.length\n\
}\n";
    assert_rejected(SRC);
}

#[test]
fn check_not_null_lambda_overload_narrows_the_argument() {
    const SRC: &str = "fun f(a: String?): Int {\n\
    checkNotNull(a) { \"missing\" }\n\
    return a.length\n\
}\n\
fun box(): String = if (f(\"hey\") == 3) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("checkNotNull(lambda) smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn while_contract_condition_narrows_the_body() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    while (!s.isNullOrEmpty()) {\n        return s.length\n    }\n\
    return -1\n\
}\n\
fun box(): String = if (f(\"abcd\") == 4 && f(null) == -1 && f(\"\") == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("while-contract smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn alias_imported_contract_still_narrows() {
    // The contract follows the RESOLVED stdlib callable, not the syntax name.
    const SRC: &str = "import kotlin.text.isNullOrEmpty as ise\n\
fun f(s: String?): Int {\n\
    if (!s.ise()) return s.length\n\
    return -1\n\
}\n\
fun box(): String = if (f(\"hi\") == 2 && f(null) == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("alias contract smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn is_empty_on_nullable_receiver_stays_rejected() {
    // kotlinc parity: there is NO `CharSequence?.isEmpty()` stdlib extension — `s.isEmpty()` on a
    // nullable receiver is an error (only `isNullOrEmpty`/`isNullOrBlank` accept null).
    const SRC: &str = "fun nullable(): String? = \"x\"\n\
fun box(): String {\n\
    val s: String? = nullable()\n\
    return if (s.isEmpty()) \"OK\" else \"FAIL\"\n\
}\n";
    assert_rejected(SRC);
}
