//! A property initializer's store sits on its initializer expression's line. For an array factory
//! call (`arrayOf(`, `intArrayOf(`) whose elements start on a later line, that is the call's line:
//! kotlinc marks it before the array is built and again before the `putstatic`/`putfield`, while
//! the array's own instructions take the first element's line.

use super::common;

#[test]
fn an_array_factory_initializer_stores_on_the_call_line_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Factories.kt",
        "package factories\n\
         \n\
         val names = arrayOf(\n\
         \x20   \"a\", \"b\")\n\
         \n\
         val numbers = intArrayOf(\n\
         \x20   1, 2)\n\
         \n\
         val list = listOf(\n\
         \x20   \"c\")\n\
         \n\
         val sameLine = arrayOf(\"d\",\n\
         \x20   \"e\")\n\
         \n\
         class Holder {\n\
         \x20   val names = arrayOf(\n\
         \x20       \"a\")\n\
         \x20   var numbers = longArrayOf(\n\
         \x20       1L)\n\
         \n\
         \x20   companion object {\n\
         \x20       val shared = arrayOf(\n\
         \x20           \"s\")\n\
         \x20   }\n\
         }\n\
         \n\
         object Single {\n\
         \x20   val names = arrayOf(\n\
         \x20       \"o\")\n\
         }\n\
         \n\
         fun local(): Array<String> {\n\
         \x20   val names = arrayOf(\n\
         \x20       \"l\")\n\
         \x20   return names\n\
         }\n",
    )]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
