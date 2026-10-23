//! A class whose header passes arguments to its superclass constructor can declare an inline member
//! that the same module calls. Preparing that inline body checks the class header again, so the
//! `super(…)` arguments are part of what the compiler keeps for it.

use super::common;

fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn an_inline_member_of_a_class_with_superclass_arguments_compiles_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Counter.kt",
        "open class Base(val start: Int)\n\
         \n\
         class Counter(start: Int) : Base(start + 1) {\n\
         \x20   inline fun next(step: () -> Int): Int = start + step()\n\
         }\n\
         \n\
         fun box(): String = if (Counter(1).next { 2 } == 4) \"OK\" else \"fail\"\n",
    )]);
}

#[test]
fn an_inline_member_of_a_class_with_interface_delegation_runs() {
    common::expect_box_ok_with_stdlib(
        "interface Named { val name: String }\n\
         \n\
         class Fixed(override val name: String) : Named\n\
         \n\
         class Greeter(name: String) : Named by Fixed(name.uppercase()) {\n\
         \x20   inline fun greet(prefix: () -> String): String = prefix() + name\n\
         }\n\
         \n\
         fun box(): String = Greeter(\"k\").greet { \"O\" }\n",
        "Named",
    );
}
