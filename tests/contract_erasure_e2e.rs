//! A `kotlin.contracts.contract { … }` block is erased metadata: it is never executed and emits no
//! bytecode (kotlinc drops it at codegen). The block's body uses the `ContractBuilder` DSL
//! (`callsInPlace`/`returns`/`implies`), which isn't ordinary executable code — before, krusty tried
//! to resolve those DSL members as real calls and failed with "unresolved function 'callsInPlace'".
//! The checker now recognises the `contract { … }` statement (callee `contract` resolving to
//! `kotlin/contracts`), skips type-checking its lambda, and the lowerer drops the statement. Same-file,
//! runnable against the real stdlib.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn contract_calls_in_place_is_erased() {
    // `callsInPlace(action, EXACTLY_ONCE)` — the canonical contract. The block must vanish; the
    // function runs `action()` normally.
    const SRC: &str = "@file:OptIn(ExperimentalContracts::class)\n\
        import kotlin.contracts.*\n\
        fun runOnce(action: () -> Unit) {\n\
        \x20 contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }\n\
        \x20 action()\n\
        }\n\
        fun box(): String {\n\
        \x20 var res = \"FAIL\"\n\
        \x20 runOnce { res = \"OK\" }\n\
        \x20 return res\n\
        }\n";
    assert_eq!(run(SRC).expect("contract callsInPlace erased"), "OK");
}

#[test]
fn contract_returns_implies_is_erased() {
    // A `returns() implies (…)` contract on a boolean-returning function — also pure metadata.
    const SRC: &str = "@file:OptIn(ExperimentalContracts::class)\n\
        import kotlin.contracts.*\n\
        fun isNonEmpty(s: String?): Boolean {\n\
        \x20 contract { returns(true) implies (s != null) }\n\
        \x20 return s != null && s.isNotEmpty()\n\
        }\n\
        fun box(): String = if (isNonEmpty(\"x\") && !isNonEmpty(null)) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("contract returns/implies erased"), "OK");
}

#[test]
fn user_defined_contract_function_is_not_erased() {
    // A user's OWN top-level `fun contract(...)` shadows the stdlib intrinsic — its call is real
    // executable code and must run, not be erased.
    const SRC: &str = "var log = \"\"\n\
        fun contract(block: () -> Unit) { log += \"ran\"; block() }\n\
        fun box(): String {\n\
        \x20 contract { log += \"!\" }\n\
        \x20 return if (log == \"ran!\") \"OK\" else \"fail: \" + log\n\
        }\n";
    assert_eq!(run(SRC).expect("user contract not erased"), "OK");
}
