//! A `suspend` function's first numbered slot belongs to its continuation class.
//!
//! The continuation class and the anonymous objects declared in a function are numbered from the
//! same per-function sequence, and krusty drew them from two independent counters. Both then
//! claimed `<facade>$<fn>$1`, so the anonymous object's class file was silently OVERWRITTEN by the
//! continuation's — the emitted jar simply lost a class — and when their constructors disagreed on
//! arity the backend panicked instead:
//!
//! ```text
//! checked constructor supplied-argument count: owner=…$build$1 supplied=1 physical=2
//! ```
//!
//! Measured against kotlinc 2.4.10: the slot is reserved for ANY suspend function, whether or not a
//! continuation class is ultimately emitted — a suspend function with no suspension point still
//! names its first anonymous object `…$build$2` and emits no `$build$1` at all.

use super::common;

fn classes(src: &str, stem: &str) -> Vec<String> {
    let mut names: Vec<String> = common::expect_classes_with_stdlib(src, stem)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.contains("$build$"))
        .collect();
    names.sort();
    names
}

/// One suspension and one anonymous object: the two classes must both survive.
#[test]
fn a_suspend_function_reserves_its_first_slot_for_the_continuation() {
    const SRC: &str = "interface One { val a: String }\n\
        class Api { suspend fun one(x: String): String = x }\n\
        suspend fun build(api: Api, p: String): One {\n\
        \x20   val ra = api.one(p)\n\
        \x20   return object : One { override val a = ra }\n\
        }\n";
    assert_eq!(
        classes(SRC, "SuspendAnonNaming"),
        vec!["SuspendAnonNamingKt$build$1", "SuspendAnonNamingKt$build$2"],
        "the continuation takes $1 and the anonymous object $2"
    );
}

/// No suspension point, so no continuation class — and the slot stays reserved anyway.
#[test]
fn a_suspend_function_without_a_suspension_still_reserves_the_slot() {
    const SRC: &str = "interface One { val a: String }\n\
        suspend fun build(p: String): One = object : One { override val a = p }\n";
    assert_eq!(
        classes(SRC, "SuspendAnonNoSuspension"),
        vec!["SuspendAnonNoSuspensionKt$build$2"],
        "the reservation does not depend on a continuation actually being emitted"
    );
}

/// A non-suspend function reserves nothing.
#[test]
fn an_ordinary_function_numbers_its_objects_from_one() {
    const SRC: &str = "interface One { val a: String }\n\
        fun build(p: String): One = object : One { override val a = p }\n";
    assert_eq!(
        classes(SRC, "OrdinaryAnonNaming"),
        vec!["OrdinaryAnonNamingKt$build$1"]
    );
}

/// Two captures made the constructors disagree on arity, which panicked rather than losing a class
/// quietly. Running it proves both the naming and the capture forwarding.
#[test]
fn a_suspend_anonymous_object_carries_every_capture() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        interface Dep { val a: String; val b: String }\n\
        class Api { suspend fun one(x: String): String = x }\n\
        suspend fun build(api: Api, p: String): Dep {\n\
        \x20   val ra = api.one(p)\n\
        \x20   val rb = \"K\"\n\
        \x20   return object : Dep { override val a = ra; override val b = rb }\n\
        }\n\
        fun box(): String = runBlocking {\n\
        \x20   val dep = build(Api(), \"O\")\n\
        \x20   dep.a + dep.b\n\
        }\n";
    let jdk = common::jdk_modules();
    let run = common::compile_and_run_box(
        SRC,
        "Main",
        &[common::stdlib_jar(), common::coroutines_jar(), jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("a suspend function's anonymous object must compile and run");
    assert_eq!(run, "OK");
}
