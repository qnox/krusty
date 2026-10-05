//! An `@InlineOnly` call's arguments are read in place, as kotlinc does.
//!
//! kotlinc's `InplaceArgumentsMethodTransformer` moves each argument of an `@InlineOnly` call to
//! where the inlined body loads its parameter, when the body passes `canInlineArgumentsInPlace`
//! and the argument's code neither stores a local nor jumps out of itself. No parameter slot is
//! written, and the slots close. krusty stored every argument into its parameter slot and loaded it
//! back: `maxOf(a, b)` was `iload_0; iload_1; istore_3; iload_3; invokestatic Math.max`.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "import kotlin.coroutines.*\n\
    class Holder(val text: String)\n\
    fun resumeBuilt(target: Continuation<Holder>) { target.resume(Holder(\"OK\")) }\n\
    fun resumeUnit(target: Continuation<Unit>) { target.resume(Unit) }\n\
    fun larger(first: Int, second: Int): Int = maxOf(first, second)\n\
    fun smaller(first: Long, second: Long): Long = minOf(first, second)\n\
    fun remember(table: MutableMap<String, String>, key: String) { table[key] = key }\n\
    fun checked(ready: Boolean, count: Int): Int { require(ready); return count }\n\
    fun invokeStored(block: () -> Int): Int = run(block)\n\
    private var events = 0\n\
    private fun mark(expected: Int, value: Int): Int {\n\
    \x20   if (events != expected) return -100\n\
    \x20   events += 1\n\
    \x20   return value\n\
    }\n\
    fun orderedOnce(): Int {\n\
    \x20   events = 0\n\
    \x20   return maxOf(mark(0, 1), mark(1, 2)) * 10 + events\n\
    }\n\
    fun held(value: Int): Result<Int> = Result.success(value)\n\
    fun receiverFirst(): Int {\n\
    \x20   events = 0\n\
    \x20   return held(mark(0, 1)).getOrDefault(mark(1, 2)) * 10 + events\n\
    }\n";

#[test]
fn inline_only_arguments_are_read_where_the_body_loads_them_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "InlineArgumentsInPlace",
        SOURCE,
        "InlineArgumentsInPlaceKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "int larger(",
        "long smaller(",
        "void remember(",
        "int invokeStored(",
        "void resumeBuilt(",
        "void resumeUnit(",
        "int receiverFirst(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
        assert_eq!(
            stack_map(&built.krusty, member),
            stack_map(&built.reference, member),
            "{member} frames"
        );
    }
}

#[test]
fn an_inline_only_println_of_a_local_loads_the_local_after_system_out() {
    // `println` is `@InlineOnly` and its body is `getstatic System.out; aload message`. A
    // non-null `String` argument is widened to `Any?` before the call. That widening emits no
    // bytecode, so the parameter is still the caller's local: kotlinc reads it after `System.out`.
    // Storing it first and folding the store leaves `aload; getstatic; swap`.
    let src = "@file:Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")\n\
        @kotlin.internal.InlineOnly\n\
        private inline fun length(value: CharSequence): Int = value.length\n\
        fun printed(message: String) = println(message)\n\
        fun counted(n: Int) = println(n)\n\
        fun viewed(message: String): Int = length(message)\n\
        fun smartViewed(message: Any): Int = if (message is String) length(message) else -1\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "InlinePrintlnLocal",
        src,
        "InlinePrintlnLocalKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "void printed(java.lang.String)",
        "void counted(int)",
        "int viewed(java.lang.String)",
        "int smartViewed(java.lang.Object)",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn inline_only_arguments_read_in_place_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (larger(3, 7) != 7 || smaller(5L, -1L) != -1L) return \"order\"\n\
             \x20   val table = mutableMapOf<String, String>()\n\
             \x20   remember(table, \"k\")\n\
             \x20   if (table[\"k\"] != \"k\") return \"table\"\n\
             \x20   if (invokeStored {{ 9 }} != 9) return \"function parameter\"\n\
             \x20   if (orderedOnce() != 22) return \"evaluation order\"\n\
             \x20   if (receiverFirst() != 12) return \"receiver order\"\n\
             \x20   return if (checked(true, 4) == 4) \"OK\" else \"checked\"\n\
             }}\n"
        ),
        "inline arguments in place",
    );
}
