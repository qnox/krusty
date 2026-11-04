//! A top-level property read or written from a non-private inline function goes through its
//! accessors, as kotlinc compiles it: the body may be spliced into another class, where the
//! facade's field is private. A private inline function and an ordinary function keep the field,
//! a private property has no accessors, and a `lateinit` property calls its setter. A function
//! value stored there is cast to its function type first, as an instance field store does.
use super::common;

const PROPS: &str = "package props\n\
\n\
interface Item\n\
class Token : Item\n\
\n\
var counter: Int = 0\n\
internal var hidden: Int = 0\n\
lateinit var name: String\n\
val fixed: Int = 1\n\
var check: (Item) -> Boolean = { false }\n\
private var secret: (Item) -> Boolean = { false }\n\
\n\
inline fun publicWrite() { counter = 1 }\n\
inline fun publicRead(): Int = counter + fixed\n\
inline fun publicIncrement() { counter++ }\n\
inline fun publicName(value: String) { name = value }\n\
inline fun publicNameRead(): String = name\n\
inline fun publicCheck() { check = { it is Token } }\n\
internal inline fun internalWrite() { counter = 2; hidden = 3 }\n\
internal inline fun internalRead(): Int = hidden\n\
private inline fun privateWrite() { counter = 4; secret = { it is Token } }\n\
fun plainWrite() { counter = 5; privateWrite() }\n\
fun plainRead(): Int = counter\n\
fun secretAccepts(item: Item): Boolean = secret(item)\n";

/// Each inline function from another file of the module: every one is spliced into `MainKt`.
const MAIN: &str = "package props\n\
\n\
fun box(): String {\n\
    publicWrite()\n\
    if (publicRead() != 2) return \"public write ${publicRead()}\"\n\
    internalWrite()\n\
    if (internalRead() != 3 || counter != 2) return \"internal write\"\n\
    publicName(\"set\")\n\
    if (publicNameRead() != \"set\") return \"lateinit\"\n\
    publicCheck()\n\
    if (!check(Token())) return \"lambda\"\n\
    plainWrite()\n\
    if (plainRead() != 4 || !secretAccepts(Token())) return \"private write\"\n\
    return \"OK\"\n\
}\n";

/// `bump` reads and writes `total` through `getTotal`/`setTotal` in its own body, and the caller in
/// the same facade splices that body.
const SAME_FILE: &str = "package props\n\
\n\
var total: Int = 0\n\
inline fun bump() { total = total + 1 }\n\
fun box(): String {\n\
    bump()\n\
    bump()\n\
    return if (total == 2) \"OK\" else \"total $total\"\n\
}\n";

fn assert_module_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "the module differs from kotlinc:\n{}",
        differences.join("\n\n")
    );
}

fn assert_runs_like_kotlinc(sources: &[(&str, &str)], main_class: &str, stem: &str) {
    assert_eq!(common::kotlinc_box_files_result(sources, main_class), "OK");
    common::expect_box_ok_files_with_stdlib(sources, stem);
}

#[test]
fn inline_functions_reach_top_level_properties_through_accessors_like_kotlinc() {
    assert_module_like_kotlinc(&[("Props.kt", PROPS), ("Main.kt", MAIN)]);
}

#[test]
fn inline_functions_reaching_top_level_properties_run_like_kotlinc() {
    assert_runs_like_kotlinc(
        &[("Props.kt", PROPS), ("Main.kt", MAIN)],
        "props.MainKt",
        "inline_top_level_property_accessors",
    );
}

#[test]
fn a_same_file_inline_property_write_is_spliced_like_kotlinc() {
    assert_module_like_kotlinc(&[("Total.kt", SAME_FILE)]);
}

#[test]
fn a_same_file_inline_property_write_runs_like_kotlinc() {
    assert_runs_like_kotlinc(
        &[("Total.kt", SAME_FILE)],
        "props.TotalKt",
        "inline_top_level_property_same_file",
    );
}
