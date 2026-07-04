//! A property with a visibility-only setter (`var x = 0; private set`) is a plain backing-field
//! property whose setter is emitted `private`. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn private_setter() {
    const SRC: &str = "class P {\n\
    var x: Int = 0\n\
        private set\n\
    fun bump() { x = x + 1 }\n\
}\n\
fun box(): String { val p = P(); p.bump(); p.bump(); return if (p.x == 2) \"OK\" else \"fail\" }\n";
    assert_eq!(run(SRC).expect("private set compiles + runs"), "OK");
}
