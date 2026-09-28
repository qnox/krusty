//! An enum entry referenced from a class nested in that entry is the entry instance.
//!
//! The static field is still null while the entry constructor runs, so a superclass argument, a
//! property initializer, or a delegation inside the nested class must use the enclosing instance.
use super::common::expect_box_same_as_kotlinc;

#[test]
fn an_inner_class_super_argument_reads_the_enclosing_enum_entry() {
    expect_box_same_as_kotlinc(
        "interface IFoo {\n\
             fun foo(): String\n\
         }\n\
         \n\
         interface IBar {\n\
             fun bar(): String\n\
         }\n\
         \n\
         abstract class Base(val x: IFoo)\n\
         \n\
         enum class Test : IFoo, IBar {\n\
             FOO {\n\
                 inner class Inner : Base(FOO)\n\
         \n\
                 val z = Inner()\n\
         \n\
                 override fun foo() = \"OK\"\n\
         \n\
                 override fun bar() = z.x.foo()\n\
             }\n\
         }\n\
         \n\
         fun box() = Test.FOO.bar()\n",
        "EnumEntrySuperArg",
    );
}

#[test]
fn an_inner_class_initializer_calls_the_enclosing_enum_entry() {
    expect_box_same_as_kotlinc(
        "interface IFoo {\n\
             fun foo(): String\n\
         }\n\
         \n\
         interface IBar {\n\
             fun bar(): String\n\
         }\n\
         \n\
         enum class Test : IFoo, IBar {\n\
             FOO {\n\
                 inner class Inner {\n\
                     val fooFoo = FOO.foo()\n\
                 }\n\
         \n\
                 val z = Inner()\n\
         \n\
                 override fun foo() = \"OK\"\n\
         \n\
                 override fun bar() = z.fooFoo\n\
             }\n\
         }\n\
         \n\
         fun box() = Test.FOO.bar()\n",
        "EnumEntryInitializerCall",
    );
}

#[test]
fn an_inner_class_delegates_to_the_enclosing_enum_entry() {
    expect_box_same_as_kotlinc(
        "interface IFoo {\n\
             fun foo(): String\n\
         }\n\
         \n\
         interface IBar {\n\
             fun bar(): String\n\
         }\n\
         \n\
         enum class Test : IFoo, IBar {\n\
             FOO {\n\
                 inner class Inner : IFoo by FOO\n\
         \n\
                 val z = Inner()\n\
         \n\
                 override fun foo() = \"OK\"\n\
         \n\
                 override fun bar() = z.foo()\n\
             }\n\
         }\n\
         \n\
         fun box() = Test.FOO.bar()\n",
        "EnumEntryDelegation",
    );
}

#[test]
fn an_entry_constructor_argument_reads_the_entry_after_initialization() {
    expect_box_same_as_kotlinc(
        "enum class Choice(val text: String, val callback: () -> String) {\n\
             RETAIN(\"OK\", { RETAIN.text })\n\
         }\n\
         \n\
         fun box() = Choice.RETAIN.callback()\n",
        "EnumEntryArgumentLambda",
    );
}

#[test]
fn an_entry_method_and_its_lambda_read_that_entry() {
    expect_box_same_as_kotlinc(
        "interface Probe {\n\
             fun check(): String\n\
         }\n\
         \n\
         enum class Test : Probe {\n\
             FOO {\n\
                 override fun check(): String {\n\
                     val direct = FOO\n\
                     val fromLambda = { FOO }()\n\
                     val qualified = Test.FOO\n\
                     if (direct !== Test.FOO) return \"direct\"\n\
                     if (fromLambda !== Test.FOO) return \"lambda\"\n\
                     if (qualified !== Test.FOO) return \"qualified\"\n\
                     return \"OK\"\n\
                 }\n\
             }\n\
         }\n\
         \n\
         fun box() = Test.FOO.check()\n",
        "EnumEntryMethodLambda",
    );
}

#[test]
fn a_nested_class_reads_a_sibling_enum_entry_from_its_static_field() {
    expect_box_same_as_kotlinc(
        "interface Probe {\n\
             fun check(): String\n\
         }\n\
         \n\
         enum class Test : Probe {\n\
             FOO {\n\
                 inner class Inner {\n\
                     fun self() = FOO\n\
                     fun sibling() = BAR\n\
                     fun qualified() = Test.FOO\n\
                 }\n\
                 override fun check(): String {\n\
                     val i = Inner()\n\
                     if (i.self() !== Test.FOO) return \"self\"\n\
                     if (i.sibling() !== Test.BAR) return \"sibling\"\n\
                     if (i.qualified() !== Test.FOO) return \"qualified\"\n\
                     return \"OK\"\n\
                 }\n\
             },\n\
             BAR {\n\
                 override fun check() = \"BAR\"\n\
             }\n\
         }\n\
         \n\
         fun box() = Test.FOO.check()\n",
        "EnumEntrySibling",
    );
}
