//! The alternate `js` backend (krusty-ir → JavaScript) exercised end-to-end on Node: lower Kotlin to
//! backend-agnostic IR, emit JS, run it, and assert the result. Proves the IR is target-neutral — the
//! same lowering that feeds the JVM backend runs correctly on Node — and covers `src/js/mod.rs`.
//!
//! Skips cleanly when the kotlin-stdlib jar / JDK modules (needed for front-end resolution) or `node`
//! are unavailable.

use super::common;

/// Lower `src`, emit JS, append `console.log(box())`, run on Node. Returns Node's stdout, or `None`
/// to skip (toolchain/node missing or a front-end gap).
fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    let ir = common::lower_to_ir(src, &[stdlib], Some(&jdk))?;
    let js = format!("{}\nconsole.log(box());\n", krusty::js::emit_file(&ir));
    common::run_js(&js)
}

/// Assert Node prints `expected`; skip (return) if the toolchain/node is unavailable.
fn check(src: &str, expected: &str) {
    match run(src) {
        Some(out) => assert_eq!(out, expected, "js output mismatch\n--- src ---\n{src}"),
        None => eprintln!("skipping js_backend_e2e: no stdlib/JDK/node"),
    }
}

#[test]
fn arithmetic_and_precedence() {
    check("fun box(): Int { return 2 + 3 * 4 - 10 / 2 }", "9");
}

#[test]
fn if_else_expression() {
    check(
        "fun box(): String { val x = 7; return if (x > 5) \"big\" else \"small\" }",
        "big",
    );
}

#[test]
fn while_loop_accumulate() {
    check(
        "fun box(): Int { var i = 0; var s = 0; while (i < 5) { s = s + i; i = i + 1 }; return s }",
        "10",
    );
}

#[test]
fn do_while_loop() {
    check(
        "fun box(): Int { var i = 0; do { i = i + 1 } while (i < 3); return i }",
        "3",
    );
}

#[test]
fn for_range_loop() {
    check(
        "fun box(): Int { var s = 0; for (i in 1..4) { s = s + i }; return s }",
        "10",
    );
}

#[test]
fn labeled_break() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         loop@ for (i in 0..10) {\n\
           if (i == 4) break@loop\n\
           s = s + i\n\
         }\n\
         return s\n\
         }",
        "6",
    );
}

#[test]
fn continue_in_loop() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         for (i in 0..6) {\n\
           if (i % 2 == 1) continue\n\
           s = s + i\n\
         }\n\
         return s\n\
         }",
        "12",
    );
}

#[test]
fn if_elseif_else_statement() {
    check(
        "fun sign(n: Int): String {\n\
         var r = \"\"\n\
         if (n > 0) { r = \"pos\" } else if (n < 0) { r = \"neg\" } else { r = \"zero\" }\n\
         return r\n\
         }\n\
         fun box(): String { return sign(5) + sign(-2) + sign(0) }",
        "posnegzero",
    );
}

#[test]
fn class_field_and_method() {
    check(
        "class Counter(var n: Int) { fun inc(): Int { n = n + 1; return n } }\n\
         fun box(): Int { val c = Counter(10); c.inc(); return c.inc() }",
        "12",
    );
}

#[test]
fn instanceof_and_cast() {
    check(
        "open class Animal\nclass Dog : Animal()\n\
         fun box(): String { val a: Animal = Dog(); return if (a is Dog) \"dog\" else \"no\" }",
        "dog",
    );
}

#[test]
fn string_concat_and_tostring() {
    check(
        "fun box(): String { val n = 42; val s = \"ab\" + \"cd\"; return s + \"=\" + n.toString() }",
        "abcd=42",
    );
}

#[test]
fn boolean_short_circuit_and_when() {
    check(
        "fun grade(n: Int): String = when {\n\
           n >= 90 -> \"A\"\n\
           n >= 80 -> \"B\"\n\
           else -> \"C\"\n\
         }\n\
         fun box(): String { return grade(85) + grade(95) + grade(10) }",
        "BAC",
    );
}

#[test]
fn int_array_sum() {
    check(
        "fun box(): Int {\n\
         val a = IntArray(4)\n\
         var i = 0\n\
         while (i < 4) { a[i] = i * i; i = i + 1 }\n\
         var s = 0\n\
         for (j in 0..3) { s = s + a[j] }\n\
         return s\n\
         }",
        "14",
    );
}

#[test]
fn recursion() {
    check(
        "fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
         fun box(): Int { return fib(10) }",
        "55",
    );
}

#[test]
fn top_level_property_and_null() {
    check(
        "val base: Int = 100\n\
         fun box(): String { val x: String? = null; return if (x == null) base.toString() else \"no\" }",
        "100",
    );
}
