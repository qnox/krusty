//! A private suspend function takes kotlinc's coroutine transformer like any other named function
//! (`docs/JVM_INLINE_BEFORE_CPS.md` §1a). Its continuation class re-enters it through the owner's
//! `access$<name>` bridge.
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
