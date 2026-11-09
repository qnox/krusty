//! A top-level `val` initialized by an array-creation builtin infers its type without an explicit
//! annotation: `val arr = arrayOf("a","b")` → `Array<String>`, `val n = intArrayOf(1,2)` → `IntArray`.
//! The lightweight signature inferer must agree with the full checker's `check_array_builtin`.
//! An empty `arrayOf()` has no element evidence of its own, so only a selected array parameter
//! supplies it; any other parameter leaves it reported.
use super::common;
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Main")
}

#[test]
fn toplevel_reference_array_of_infers() {
    const SRC: &str = "val arr = arrayOf(\"O\", \"K\")\n\
        fun box(): String = arr[0] + arr[1]\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn toplevel_primitive_array_of_infers() {
    const SRC: &str = "val nums = intArrayOf(1, 2, 3)\n\
        fun box(): String = if (nums[0] + nums[2] == 4) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn toplevel_array_size_and_iteration() {
    // The inferred array carries its element type into member access (`.size`) and a for-loop.
    const SRC: &str = "val arr = arrayOf(\"O\", \"K\")\n\
        fun box(): String {\n\
        \x20 var out = \"\"\n\
        \x20 for (e in arr) out += e\n\
        \x20 return if (arr.size == 2) out else \"no\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn empty_array_of_under_a_non_array_parameter_is_reported_like_kotlinc() {
    // Delegation and enum entries postpone the factory until the constructor is selected, then
    // check it under the selected parameter. A `String` parameter supplies no element type and
    // cannot take an array; an `Any` parameter takes the array but still supplies no element type.
    const SRC: &str = "open class Holder(val value: String)\n\
class Two(val count: Int, val text: String)\n\
fun take(text: String) {}\n\
fun takeAny(value: Any) {}\n\
class Header : Holder(arrayOf())\n\
class Secondary : Holder {\n\
    constructor() : super(arrayOf())\n\
}\n\
class Delegating(val value: String) {\n\
    constructor() : this(arrayOf())\n\
}\n\
enum class Kind(val value: String) { A(arrayOf()) }\n\
val plain = Holder(arrayOf())\n\
fun calls() {\n\
    take(arrayOf())\n\
    Two(1, arrayOf())\n\
    takeAny(arrayOf())\n\
}\n";
    const UNINFERRED: &str = "cannot infer type for type parameter 'T'. Specify it explicitly.";
    const MISMATCH: &str = "argument type mismatch: actual type is \
'Array<uninferred T (of fun <T> arrayOf)>', but 'String' was expected.";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    let mut expected = Vec::new();
    for position in ["5:23", "7:23", "10:22", "12:40", "13:20", "15:6", "16:8"] {
        expected.push(format!("Main.kt:{position}: {UNINFERRED}"));
        expected.push(format!("Main.kt:{position}: {MISMATCH}"));
    }
    expected.push(format!("Main.kt:17:9: {UNINFERRED}"));
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        expected
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}
