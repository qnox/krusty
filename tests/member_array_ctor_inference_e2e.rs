//! A member property initialized by a primitive-array size constructor (`val data = IntArray(9)`) has
//! its type INFERRED — previously the member-property inference probe didn't recognize the stdlib
//! array pseudo-constructors and demanded an explicit type. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_property_inferred_from_primitive_array_ctor() {
    const SRC: &str = "class Buf {\n\
        \x20 val data = IntArray(4)\n\
        \x20 val chars = CharArray(2)\n\
        }\n\
        fun box(): String {\n\
        \x20 val b = Buf()\n\
        \x20 b.data[0] = 5\n\
        \x20 return if (b.data.size == 4 && b.data[0] == 5 && b.chars.size == 2) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("member array-ctor inference"), "OK");
}
