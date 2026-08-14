//! Unqualified use of a member imported from a SAME-FILE object (`import Host.foo; foo(...)`). krusty
//! handled a classpath object's imported member; a same-file object now dispatches the same way
//! (`getstatic Host.INSTANCE; invoke`). Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn imported_same_file_object_member_call() {
    const SRC: &str = "import Host.foo\n\
        object Host { fun foo(x: String): String = x + \"K\" }\n\
        fun box(): String = foo(\"O\")\n";
    assert_eq!(run(SRC).expect("imported object member call"), "OK");
}

#[test]
fn aliased_same_file_object_member_call() {
    const SRC: &str = "import Host.foo as append\n\
        object Host { fun foo(x: String): String = x + \"K\" }\n\
        fun box(): String = append(\"O\")\n";
    assert_eq!(run(SRC).expect("aliased object member call"), "OK");
}

#[test]
fn imported_same_file_object_property_read() {
    const SRC: &str = "import Host.answer\n\
        object Host { val answer = \"OK\" }\n\
        fun box(): String = answer\n";
    assert_eq!(run(SRC).expect("imported object property read"), "OK");
}
