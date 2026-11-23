//! A callable reference's adapter takes the reference's line for its whole body. For an adapted
//! reference to an array factory (`::intArrayOf`, `::arrayOf`) the body is the array itself, so
//! kotlinc enters the line at the array's first instruction rather than at its first element read.

use super::common;

#[test]
fn an_adapted_array_factory_reference_enters_its_line_at_the_array() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Factories.kt",
        "package factories\n\
         \n\
         fun pair(): (Int, Int) -> IntArray = ::intArrayOf\n\
         \n\
         fun chars(): (Char) -> CharArray = ::charArrayOf\n\
         \n\
         fun strings(): (String, String) -> Array<String> = ::arrayOf\n\
         \n\
         fun single(): (Long) -> LongArray = ::longArrayOf\n",
    )]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
