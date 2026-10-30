//! `@file:JvmName("Facade") @file:JvmMultifileClass`: each source compiles to a synthetic part
//! `Facade__FooKt`, and the module writes one public facade forwarding to every part. The portable
//! shapes are box regressions (`tests/box_regressions/jvmMultifileClass*.kt`); these cover the
//! JVM-only ones byte for byte against kotlinc: a part's private functions are renamed after the
//! part, which function references, suspend continuations and their accessors all observe.

use super::common;

const FIRST: &str = "@file:JvmName(\"Shared\")\n\
    @file:JvmMultifileClass\n\
    package p\n\
    \n\
    private fun twice(value: Int) = value * 2\n\
    private fun scaled(value: Int = 3) = value * 10\n\
    \n\
    fun first(value: Int) = twice(value) + scaled()\n\
    fun reference(): (Int) -> Int = ::twice\n\
    fun defaulted(): Any = ::scaled\n\
    fun runnable(): Runnable = object : Runnable { override fun run() { twice(1) } }\n\
    \n\
    lateinit var late: String\n";

const SECOND: &str = "@file:JvmName(\"Shared\")\n\
    @file:JvmMultifileClass\n\
    package p\n\
    \n\
    private fun twice(value: Int) = value + value + 1\n\
    \n\
    fun second(value: Int) = twice(value)\n\
    fun thrice(value: Int): Int {\n\
    \x20   val add = { extra: Int -> twice(value) + extra }\n\
    \x20   return add(value - 1)\n\
    }\n\
    \n\
    private suspend fun step(value: Int): Int = value + 1\n\
    suspend fun steps(value: Int): Int = step(step(value))\n";

const CALLER: &str = "package p\n\
    \n\
    import kotlin.coroutines.*\n\
    \n\
    fun box(): String {\n\
    \x20   var stepped = 0\n\
    \x20   suspend { steps(40) }.startCoroutine(Continuation(EmptyCoroutineContext) { stepped = it.getOrThrow() })\n\
    \x20   late = \"late\"\n\
    \x20   runnable().run()\n\
    \x20   val joined = \"${first(1)} ${second(1)} ${thrice(1)} ${reference()(5)} $stepped $late\"\n\
    \x20   return if (joined == \"32 3 3 10 42 late\" && defaulted() == defaulted()) \"OK\" else joined\n\
    }\n";

const SOURCES: [(&str, &str); 3] = [
    ("first.kt", FIRST),
    ("second.kt", SECOND),
    ("Caller.kt", CALLER),
];

#[test]
fn multifile_parts_and_their_facade_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&SOURCES);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        classes.krusty.keys().collect::<Vec<_>>(),
        "exact class inventory"
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn multifile_parts_run_through_their_facade() {
    common::expect_box_ok_files_with_stdlib(&SOURCES, "p.CallerKt");
}
