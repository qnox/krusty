//! Escaping reified lambdas that cross a local delegated-property accessor boundary.
//!
//! The delegate operator itself is deliberately ordinary. Kotlin does not inline a reified
//! `getValue` call made through local-delegate syntax; using one would make the reference fixture
//! throw before it could test Krusty's specialization.

use super::common;

#[test]
fn an_escaping_lambda_specializes_a_cast_after_reading_a_local_delegate() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate(private val value: Any?) {\n\
             operator fun getValue(owner: Any?, property: Any?): Any? = value\n\
         }\n\
         var read: () -> Any? = { null }\n\
         inline fun <reified R : Item> install(value: Any?) {\n\
             val local by Delegate(value)\n\
             read = { local as? R }\n\
         }\n\
         fun box(): String {\n\
             install<Token>(Token())\n\
             val kept = read()\n\
             install<Token>(Root())\n\
             return if (kept is Token && read() == null) \"OK\" else \"Fail\"\n\
         }\n",
        "EscapingReifiedLocalDelegate",
    );
}

#[test]
fn an_omitted_noinline_default_specializes_after_reading_a_local_delegate() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate(private val value: Any?) {\n\
             operator fun getValue(owner: Any?, property: Any?): Any? = value\n\
         }\n\
         inline fun <reified R : Item> read(\n\
             value: Any?,\n\
             noinline action: () -> Any? = {\n\
                 val local by Delegate(value)\n\
                 local as? R\n\
             }\n\
         ): Any? = action()\n\
         fun box(): String {\n\
             val kept = read<Token>(Token())\n\
             val rejected = read<Token>(Root())\n\
             return if (kept is Token && rejected == null) \"OK\" else \"Fail\"\n\
         }\n",
        "ReifiedLocalDelegateDefaultLambda",
    );
}
