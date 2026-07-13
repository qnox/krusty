//! A top-level `val` initialized by an array-creation builtin infers its type without an explicit
//! annotation: `val arr = arrayOf("a","b")` → `Array<String>`, `val n = intArrayOf(1,2)` → `IntArray`.
//! The lightweight signature inferer must agree with the full checker's `check_array_builtin`.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn toplevel_reference_array_of_infers() {
    const SRC: &str = "val arr = arrayOf(\"O\", \"K\")\n\
        fun box(): String = arr[0] + arr[1]\n";
    assert_eq!(run(SRC).expect("arrayOf infer"), "OK");
}

#[test]
fn toplevel_primitive_array_of_infers() {
    const SRC: &str = "val nums = intArrayOf(1, 2, 3)\n\
        fun box(): String = if (nums[0] + nums[2] == 4) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("intArrayOf infer"), "OK");
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
    assert_eq!(run(SRC).expect("array size + iterate"), "OK");
}
