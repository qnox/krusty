//! A secondary constructor's `this(...)` delegation, in a class WITH a primary constructor, may
//! target ANOTHER secondary constructor (a sibling) — not only the primary. The target is resolved
//! by argument arity/types, like the no-primary-constructor sibling path.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn this_delegation_targets_sibling_secondary() {
    // `constructor(x: Int): this(x.toString())` must reach the `constructor(x: String)` sibling,
    // which itself delegates to the primary `A()` — running the init blocks exactly once.
    const SRC: &str = "var log: String = \"\"\n\
        class A() {\n\
        \x20 var prop: String = \"\"\n\
        \x20 init { log += \"i\" }\n\
        \x20 constructor(x: String): this() { prop = x; log += \"s\" }\n\
        \x20 constructor(x: Int): this(x.toString()) { prop += \"#int\"; log += \"n\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a1 = A(\"abc\")\n\
        \x20 if (a1.prop != \"abc\" || log != \"is\") return \"fail1: ${a1.prop} $log\"\n\
        \x20 log = \"\"\n\
        \x20 val a2 = A(7)\n\
        \x20 if (a2.prop != \"7#int\" || log != \"isn\") return \"fail2: ${a2.prop} $log\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("this-delegation to sibling"), "OK");
}
