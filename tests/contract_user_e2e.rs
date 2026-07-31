//! User-declared contracts (`contract { … }` on a same-module function) must drive smart casts
//! at the call site — the effects are decoded from the source DSL block instead of being
//! dropped. Mirrors the conformance corpus shapes `contracts/contractForCast.kt`
//! (`returns() implies actual`) and `contracts/kt45236.kt`
//! (`returns(true) implies (this@f is Err)`). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn returns_implies_actual_smart_casts_the_argument_expression() {
    // `check(s is P)` — the contract makes the argument's facts hold for the rest of the block.
    const SRC: &str = "import kotlin.contracts.*\n\
open class S\n\
class P(val str: String = \"OK\") : S()\n\
@OptIn(kotlin.contracts.ExperimentalContracts::class)\n\
fun check(actual: Boolean) {\n\
    contract { returns() implies actual }\n\
    if (!actual) throw AssertionError()\n\
}\n\
fun box(): String {\n\
    val s: S = P()\n\
    check(s is P)\n\
    return if (s.str == \"OK\") \"OK\" else \"FAIL\"\n\
}\n";
    // `P.str` has a default value in the corpus; give it one here too.
    assert_eq!(
        run(SRC).expect("user contract smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn returns_true_implies_receiver_is_smart_casts_in_then_branch() {
    const SRC: &str = "import kotlin.contracts.ExperimentalContracts\n\
import kotlin.contracts.contract\n\
sealed class Res {\n\
    data class Err(val error: String) : Res()\n\
    object Ok : Res()\n\
}\n\
@OptIn(ExperimentalContracts::class)\n\
fun Res.isErr(): Boolean {\n\
    contract { returns(true) implies (this@isErr is Res.Err) }\n\
    return this is Res.Err\n\
}\n\
fun box(): String {\n\
    val r: Res = Res.Err(\"OK\")\n\
    if (r.isErr()) return r.error\n\
    return \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("receiver is-contract smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn returns_false_implies_param_not_null_smart_casts_in_else_branch() {
    const SRC: &str = "import kotlin.contracts.ExperimentalContracts\n\
import kotlin.contracts.contract\n\
@OptIn(ExperimentalContracts::class)\n\
fun String?.isBlank(): Boolean {\n\
    contract { returns(false) implies (this@isBlank != null) }\n\
    return this == null || this.length == 0\n\
}\n\
fun f(s: String?): Int {\n\
    if (s.isBlank()) return -1\n\
    return s.length\n\
}\n\
fun box(): String = if (f(\"hey\") == 3 && f(null) == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("returns(false) contract smartcast compiles + runs"),
        "OK"
    );
}
