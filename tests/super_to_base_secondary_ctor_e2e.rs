//! A secondary constructor's `super(...)` may target a base class whose matching constructor is
//! itself a SECONDARY constructor (the base has no primary constructor) — resolved by argument
//! arity, and calling that base `<init>` directly.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn super_targets_a_base_secondary_by_arity() {
    // Base `B` has two secondary constructors (`(Int)` and `(Int, Int)`); `A`'s two secondaries
    // delegate to each via `super(...)`, and a third delegates to a sibling via `this(...)`.
    const SRC: &str = "var log = \"\"\n\
        abstract class B {\n\
        \x20 val p: String\n\
        \x20 constructor(a: Int) { p = a.toString(); log += \"b1;\" }\n\
        \x20 constructor(a: Int, b: Int) { p = (a + b).toString(); log += \"b2;\" }\n\
        }\n\
        class A : B {\n\
        \x20 var q: String = \"\"\n\
        \x20 constructor(x: Int, y: Int): super(x, y) { q = \"two\"; log += \"a2;\" }\n\
        \x20 constructor(x: Int): super(x + 1) { q = \"one\"; log += \"a1;\" }\n\
        \x20 constructor(): this(7) { log += \"a0;\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a = A(5, 10)\n\
        \x20 if (a.p != \"15\" || a.q != \"two\") return \"fail1: ${a.p} ${a.q}\"\n\
        \x20 val b = A(3)\n\
        \x20 if (b.p != \"4\" || b.q != \"one\") return \"fail2: ${b.p} ${b.q}\"\n\
        \x20 val c = A()\n\
        \x20 if (c.p != \"8\" || c.q != \"one\") return \"fail3: ${c.p} ${c.q}\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("super to base secondary"), "OK");
}

#[test]
fn super_targets_a_base_with_single_secondary() {
    // Base `B` has exactly ONE (secondary) constructor and no primary — `super(x)` must call it.
    const SRC: &str = "abstract class B {\n\
        \x20 val p: String\n\
        \x20 constructor(a: Int) { p = \"b\" + a }\n\
        }\n\
        class A : B {\n\
        \x20 constructor(x: Int): super(x)\n\
        }\n\
        fun box(): String {\n\
        \x20 return if (A(5).p == \"b5\") \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("super to single base secondary"), "OK");
}
