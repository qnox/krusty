//! A multi-argument index operator (`m[i, j]` → `m.get(i, j)`, `m[i, j] = v` → `m.set(i, j, v)`) on a
//! user class with member `operator fun get`/`set`. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn user_class_member_get_and_set() {
    const SRC: &str = "class Matrix {\n\
        \x20 val data: IntArray = IntArray(9)\n\
        \x20 operator fun get(i: Int, j: Int): Int = data[i * 3 + j]\n\
        \x20 operator fun set(i: Int, j: Int, v: Int) { data[i * 3 + j] = v }\n\
        }\n\
        fun box(): String {\n\
        \x20 val m = Matrix()\n\
        \x20 m[1, 2] = 42\n\
        \x20 m[2, 0] = 7\n\
        \x20 return if (m[1, 2] == 42 && m[2, 0] == 7 && m[0, 0] == 0) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("multi-index member get/set"), "OK");
}
