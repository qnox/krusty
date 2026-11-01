//! A private suspend function takes kotlinc's coroutine transformer like any other named function.
//! Its continuation class re-enters it through the owner's
//! `access$<name>` bridge. A private `suspend inline` function is not transformed: every call
//! inlines it, and kotlinc leaves its own method as written.
//!
//! A private function used to keep the IR machine. That machine gives each local index one type, so
//! a loop variable that reused the index of an inlined lambda's local was restored with the other
//! local's type, and the class failed verification ("Bad type on operand stack").

use super::common;

const PRIVATE: &str = "class C {\n\
    \x20   private suspend fun inner(s: String): String {\n\
    \x20       val t = id(s)\n\
    \x20       return t + id(s)\n\
    \x20   }\n\
    \x20   suspend fun run() = inner(\"a\")\n\
    }\n\
    private suspend fun twice(x: Int): Int {\n\
    \x20   val y = number(x)\n\
    \x20   return y + number(x)\n\
    }\n\
    suspend fun id(s: String) = s\n\
    suspend fun number(x: Int) = x\n\
    suspend fun both() = twice(2).toString() + C().run()\n";

#[test]
fn a_private_suspend_function_is_the_reference_compilers_class() {
    let classes = common::classes_against_kotlinc_module(&[("Private.kt", PRIVATE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const LOOP: &str = "import kotlin.coroutines.*\n\
    class Img(val m: Map<String, String>) { operator fun get(k: String): String? = m[k] }\n\
    class Api {\n\
    \x20   suspend fun list(): List<Img> = listOf(Img(mapOf(\"id\" to \"1\", \"name\" to \"keep-a\")), Img(mapOf(\"id\" to \"2\", \"name\" to \"x\")))\n\
    \x20   suspend fun delete(id: String) {}\n\
    }\n\
    class Cleaner(val api: Api, val cached: String?) {\n\
    \x20   val log = StringBuilder()\n\
    \x20   suspend fun run(tag: String) = clean(tag)\n\
    \x20   private suspend fun clean(current: String) {\n\
    \x20       val old = api.list().filter { image ->\n\
    \x20           val name = image[\"name\"]\n\
    \x20           val id = image[\"id\"]\n\
    \x20           name != null && id != null && name.startsWith(\"keep-\") && name != current && id != cached\n\
    \x20       }\n\
    \x20       for (image in old) {\n\
    \x20           val id = image[\"id\"]!!\n\
    \x20           runCatching {\n\
    \x20               api.delete(id)\n\
    \x20               log.append(id)\n\
    \x20           }\n\
    \x20       }\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   val cleaner = Cleaner(Api(), null)\n\
    \x20   suspend { cleaner.run(\"t\") }.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    \x20   return if (cleaner.log.toString() == \"1\") \"OK\" else cleaner.log.toString()\n\
    }\n";

#[test]
fn a_private_suspend_loop_after_an_inlined_lambda_runs() {
    common::expect_box_ok_with_stdlib(LOOP, "a private suspend loop after an inlined lambda");
}

/// A lambda the compiler lifts to a private suspend function takes a captured `var` as its `Ref`
/// cell. The continuation re-enters through the `access$` bridge with a placeholder of that cell's
/// type, and names the cell in its enclosing-method descriptor.
const LIFTED_MUTABLE_CAPTURE: &str = "import kotlin.coroutines.*\n\
    interface Source { suspend fun consume(sink: Sink) }\n\
    interface Sink { suspend fun send(item: Int) }\n\
    inline fun source(crossinline action: suspend Sink.() -> Unit): Source = object : Source {\n\
    \x20   override suspend fun consume(sink: Sink) { action(sink) }\n\
    }\n\
    suspend inline fun Source.forEachItem(crossinline action: suspend (Int) -> Unit) {\n\
    \x20   consume(object : Sink { override suspend fun send(item: Int) = action(item) })\n\
    }\n\
    fun range(count: Int): Source = source { for (i in 1..count) send(i) }\n\
    fun builder(c: suspend () -> Unit) {\n\
    \x20   c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    }\n\
    fun box(): String {\n\
    \x20   var res = 0\n\
    \x20   builder { range(4).forEachItem { res += it } }\n\
    \x20   return if (res == 10) \"OK\" else \"fail $res\"\n\
    }\n";

#[test]
fn a_lifted_suspend_function_re_enters_with_its_shared_capture_cell() {
    common::expect_box_ok_with_stdlib(
        LIFTED_MUTABLE_CAPTURE,
        "a lifted suspend function with a mutable capture",
    );
}

/// A private `suspend inline` function keeps kotlinc's own body: its calls inline it, so kotlinc
/// never runs the coroutine transformer over its method, which boxes an `Int` argument with
/// `valueOf` and is called through no `access$` bridge.
const PRIVATE_INLINE: &str = "private suspend inline fun <R> two(s: String, crossinline f: suspend (String, Int) -> R): R = f(s, s.length)\n\
    suspend fun double(s: String): String = two(s) { x, n -> x + n }\n";

#[test]
fn a_private_suspend_inline_function_is_the_reference_compilers_class() {
    let classes = common::classes_against_kotlinc_module(&[("PrivateInline.kt", PRIVATE_INLINE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
