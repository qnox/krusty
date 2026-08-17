//! Binding a callable's type variable from a LAMBDA's result during signature inference.
//!
//! A member property's type comes from the signature pre-pass, which asked the resolver only for a
//! call's already-substituted return. A type variable reachable only through a lambda's result
//! (`fun <T, R> Iterable<T>.map(transform: (T) -> R): List<R>`) is by then erased to its bound, so
//! `val xs = listOf(dto).map { Item(it.id) }` typed as `List<Any>` and every member read on an
//! element was "unresolved reference". A lambda argument is contextual: its parameter types come
//! from the callable's own symbolic parameter and its body's type binds what nothing else can.
use super::common;

#[test]
fn a_member_property_binds_a_type_variable_from_a_lambda_result() {
    const MAIN: &str = "package repro\n\
        class ItemDto(val id: String)\n\
        class Item(val id: String, val tag: String)\n\
        class Repo {\n\
        \x20   private val items = listOf(ItemDto(\"a\"), ItemDto(\"b\")).map { Item(it.id, \"t\") }\n\
        \x20   fun find(key: String): Item? = items.find { it.id == key }\n\
        \x20   fun tags(): String = items.joinToString(\",\") { it.tag }\n\
        }\n\
        fun box(): String {\n\
        \x20   val repo = Repo()\n\
        \x20   if (repo.find(\"b\")?.id != \"b\") return \"fail find\"\n\
        \x20   if (repo.find(\"z\") != null) return \"fail miss\"\n\
        \x20   if (repo.tags() != \"t,t\") return \"fail tags: \" + repo.tags()\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a lambda-result type variable");
}

#[test]
fn the_receiver_type_arguments_reach_the_lambda_parameter() {
    // The lambda's PARAMETER comes from the receiver's own type arguments (`Iterable<T>` answered by
    // a `List<ItemDto>`), so binding the result is only possible once the receiver has been applied
    // through the hierarchy. Reading `it.id` inside the lambda is what pins it.
    const MAIN: &str = "package repro\n\
        class ItemDto(val id: String)\n\
        class Holder {\n\
        \x20   private val ids = listOf(ItemDto(\"a\")).map { it.id }\n\
        \x20   fun first(): String? = ids.firstOrNull()\n\
        }\n\
        fun box(): String = if (Holder().first() == \"a\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "receiver type arguments");
}

#[test]
fn a_chain_of_lambda_bound_calls_keeps_its_element() {
    // Each link's inferred result is the next link's receiver, so a single unbound variable erases
    // everything downstream of it.
    const MAIN: &str = "package repro\n\
        class ItemDto(val id: String, val keep: Boolean)\n\
        class Item(val id: String)\n\
        class Repo {\n\
        \x20   private val items =\n\
        \x20       listOf(ItemDto(\"a\", true), ItemDto(\"b\", false))\n\
        \x20           .filter { it.keep }\n\
        \x20           .map { Item(it.id) }\n\
        \x20           .sortedBy { it.id }\n\
        \x20   fun ids(): String = items.joinToString(\",\") { it.id }\n\
        }\n\
        fun box(): String = if (Repo().ids() == \"a\") \"OK\" else \"fail: \" + Repo().ids()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a chain of lambda-bound calls");
}

#[test]
fn a_non_generic_callable_and_explicit_arguments_are_untouched() {
    // The must-not-touch side. A non-generic parameter of function type binds nothing, and explicit
    // type arguments already fix the variables — neither may be rewritten by a binding taken from
    // the lambda's body.
    const MAIN: &str = "package repro\n\
        class Runner {\n\
        \x20   fun run(action: (String) -> String): String = action(\"x\")\n\
        }\n\
        class Repo {\n\
        \x20   private val out = Runner().run { it + \"y\" }\n\
        \x20   private val named = listOf(1, 2).map<Int, String> { \"n\" + it }\n\
        \x20   fun report(): String = out + \"/\" + named.joinToString(\",\") { it.uppercase() }\n\
        }\n\
        fun box(): String {\n\
        \x20   val report = Repo().report()\n\
        \x20   return if (report == \"xy/N1,N2\") \"OK\" else \"fail: \" + report\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "shapes the binding must not touch",
    );
}

#[test]
fn a_labelled_argument_keeps_the_ordinary_inference() {
    // Labels reorder arguments against the declaration, and the binding has no parameter names to
    // map them with — it must decline rather than bind the variables from the wrong arguments.
    const MAIN: &str = "package repro\n\
        class Repo {\n\
        \x20   private val joined = listOf(\"b\", \"a\").sortedBy { it }.joinToString(separator = \"-\")\n\
        \x20   fun value(): String = joined\n\
        }\n\
        fun box(): String = if (Repo().value() == \"a-b\") \"OK\" else \"fail: \" + Repo().value()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a labelled argument");
}
