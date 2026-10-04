//! `receiver.f(args)` where `f` is a VALUE of extension function type is `f.invoke(receiver, args)`.
//!
//! kotlinc places that invoke at the scope-tower level of the value, not of the receiver:
//! - with an expression receiver, members of the receiver win, then the local level (local
//!   extension functions and extension-function-typed locals/parameters, nearest first), then
//!   implicit receivers (member extensions, then their extension-function-typed properties), then
//!   top-level extension functions, then top-level properties;
//! - with an object or companion named by its classifier (`TheScope.f()`), the singleton's own
//!   members and extensions form the last tower group, so any applicable function value beats
//!   even a member of that object.
//!
//! Every fixture runs under both compilers and must produce kotlinc's `OK`.

use super::common;

#[test]
fn suspend_extension_function_parameter_on_object_qualifier_is_accepted_like_kotlinc() {
    common::assert_accepted_like_kotlinc(
        "interface Scope\n\
         object TheScope : Scope\n\
         suspend fun <R> scope(block: suspend Scope.() -> R): R = TheScope.block()\n",
    );
}

#[test]
fn suspend_extension_function_parameter_on_object_qualifier_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "import kotlin.coroutines.*\n\
         interface Scope { val tag: String }\n\
         object TheScope : Scope { override val tag = \"scope\" }\n\
         suspend fun <R> scope(block: suspend Scope.() -> R): R = TheScope.block()\n\
         fun box(): String {\n\
         \x20   var result = \"none\"\n\
         \x20   val body: suspend () -> Unit = { result = scope { tag + \"!\" } }\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
         \x20   return if (result == \"scope!\") \"OK\" else \"result $result\"\n\
         }\n",
        "ext_fn_value_suspend_object_qualifier",
    );
}

#[test]
fn extension_function_values_on_object_and_companion_qualifiers_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "interface Scope { val tag: String\n\
         \x20   companion object : Scope { override val tag = \"companion\" } }\n\
         object TheScope : Scope { override val tag = \"object\" }\n\
         fun <R> scope(block: Scope.() -> R): R = TheScope.block()\n\
         fun local(): String { val f: Scope.() -> String = { tag + \"-local\" }; return TheScope.f() }\n\
         fun nullable(x: Scope?, f: Scope.() -> String): String? = x?.f()\n\
         fun withLambda(f: Scope.(() -> String) -> String): String = TheScope.f { \"lam\" }\n\
         fun companion(f: Scope.() -> String): String = Scope.f() + \"/\" + Scope.Companion.f()\n\
         fun box(): String {\n\
         \x20   val all = listOf(\n\
         \x20       scope { tag }, local(), nullable(TheScope) { tag } ?: \"null\",\n\
         \x20       nullable(null) { tag } ?: \"null\", withLambda { it() + tag },\n\
         \x20       companion { tag },\n\
         \x20   ).joinToString()\n\
         \x20   return if (all == \"object, object-local, object, null, lamobject, companion/companion\") \"OK\" else all\n\
         }\n",
        "ext_fn_value_object_qualifiers",
    );
}

#[test]
fn function_value_outranks_members_of_a_qualified_singleton_but_not_of_an_expression() {
    common::expect_box_same_as_kotlinc(
        "interface Scope\n\
         object TheScope : Scope { fun f(): String = \"member\" }\n\
         class Plain : Scope { fun f(): String = \"member\" }\n\
         interface Holder { companion object : Scope { fun f(): String = \"member\" } }\n\
         fun qualified(f: Scope.() -> String): String = TheScope.f()\n\
         fun expression(f: Scope.() -> String): String { val s = TheScope; return s.f() }\n\
         fun instance(f: Scope.() -> String): String = Plain().f()\n\
         fun companion(f: Scope.() -> String): String = Holder.f() + \"/\" + Holder.Companion.f()\n\
         fun box(): String {\n\
         \x20   val all = listOf(qualified { \"var\" }, expression { \"var\" }, instance { \"var\" },\n\
         \x20       companion { \"var\" }).joinToString()\n\
         \x20   return if (all == \"var, member, member, var/var\") \"OK\" else all\n\
         }\n",
        "ext_fn_value_vs_singleton_member",
    );
}

#[test]
fn qualified_singleton_prefers_any_function_value_over_its_extensions() {
    common::expect_box_same_as_kotlinc(
        "interface Scope\n\
         object TheScope : Scope { val g: Scope.() -> String = { \"member-property\" } }\n\
         fun Scope.f(): String = \"top-level-extension\"\n\
         val f: Scope.() -> String = { \"top-level-property\" }\n\
         fun qualified(): String = TheScope.f()\n\
         fun expression(): String { val s: Scope = TheScope; return s.f() }\n\
         fun memberProperty(g: Scope.() -> String): String = TheScope.g()\n\
         fun localExtension(f: Scope.() -> String): String {\n\
         \x20   fun Scope.f(): String = \"local-extension\"\n\
         \x20   val s: Scope = TheScope\n\
         \x20   return s.f() + \"/\" + TheScope.f()\n\
         }\n\
         fun box(): String {\n\
         \x20   val all = listOf(qualified(), expression(), memberProperty { \"var\" },\n\
         \x20       localExtension { \"var\" }).joinToString()\n\
         \x20   val want = \"top-level-property, top-level-extension, var, local-extension/var\"\n\
         \x20   return if (all == want) \"OK\" else all\n\
         }\n",
        "ext_fn_value_vs_singleton_extensions",
    );
}

#[test]
fn local_and_receiver_function_values_outrank_outer_extensions_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "interface Scope\n\
         object TheScope : Scope\n\
         fun Scope.f(): String = \"top-level-extension\"\n\
         fun parameter(f: Scope.() -> String): String { val s: Scope = TheScope; return s.f() }\n\
         fun local(): String {\n\
         \x20   val f: Scope.() -> String = { \"local\" }\n\
         \x20   val s: Scope = TheScope\n\
         \x20   return s.f()\n\
         }\n\
         fun safe(s: Scope?, f: Scope.() -> String): String? = s?.f()\n\
         class Host(val f: Scope.() -> String) {\n\
         \x20   fun go(): String { val s: Scope = TheScope; return s.f() }\n\
         }\n\
         class MemberExtensions {\n\
         \x20   fun Scope.f(): String = \"member-extension\"\n\
         \x20   fun parameter(f: Scope.() -> String): String { val s: Scope = TheScope; return s.f() }\n\
         \x20   fun localExtension(): String {\n\
         \x20       fun Scope.f(): String = \"local-extension\"\n\
         \x20       val s: Scope = TheScope\n\
         \x20       return s.f()\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val all = listOf(parameter { \"parameter\" }, local(), safe(TheScope) { \"safe\" },\n\
         \x20       Host { \"property\" }.go(), MemberExtensions().parameter { \"parameter\" },\n\
         \x20       MemberExtensions().localExtension()).joinToString()\n\
         \x20   val want = \"parameter, local, safe, property, parameter, local-extension\"\n\
         \x20   return if (all == want) \"OK\" else all\n\
         }\n",
        "ext_fn_value_vs_outer_extensions",
    );
}
