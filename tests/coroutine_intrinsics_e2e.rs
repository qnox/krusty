//! `kotlin.coroutines` compiler intrinsics — `COROUTINE_SUSPENDED`, `suspendCoroutineUninterceptedOrReturn`,
//! `startCoroutine`. These are `@InlineOnly` stdlib declarations whose stub bodies just `throw`; the
//! reference compiler recognizes them by FQ name (an intrinsics table) and emits dedicated codegen rather
//! than calling/inlining. krusty's splice gate refuses the `throw` body, so without the registry
//! (`jvm::coroutine_intrinsics`) they resolved to "unresolved". The checker now types them via the
//! registry (LibrarySet) and lowering emits the intrinsic codegen. These compile-only checks pin the
//! resolution+lowering of the leaf shapes (a full coroutine `box()` round-trip additionally needs the
//! companion-object-as-value completion, a separate piece).

mod common;

use std::path::PathBuf;

fn compiles(src: &str) -> bool {
    let Some(jh) = common::java_home() else {
        return true; // no JDK — skip (treated as pass)
    };
    let Some(sl) = common::stdlib_jar() else {
        return true;
    };
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_in_process(src, "Coro", &[sl], Some(&jdk)).is_some()
}

#[test]
fn leaf_suspend_unintercepted_or_return_and_coroutine_suspended() {
    const SRC: &str = "import kotlin.coroutines.intrinsics.*\n\
suspend fun suspendForever(): Int = suspendCoroutineUninterceptedOrReturn { COROUTINE_SUSPENDED }\n\
fun box(): String = \"OK\"\n";
    assert!(
        compiles(SRC),
        "leaf coroutine intrinsics should resolve + lower"
    );
}

#[test]
fn coroutine_suspended_as_a_plain_value() {
    const SRC: &str = "import kotlin.coroutines.intrinsics.*\n\
suspend fun f(): Any? = suspendCoroutineUninterceptedOrReturn { val s = COROUTINE_SUSPENDED; s }\n\
fun box(): String = \"OK\"\n";
    assert!(
        compiles(SRC),
        "COROUTINE_SUSPENDED bound to a local should resolve + lower"
    );
}
