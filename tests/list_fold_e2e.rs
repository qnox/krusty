//! Collection fold/reduce family over a `List<Int>`: `fold`, `sum`, `sumOf { … }`, `count`. The
//! interesting case is `sumOf`, which resolves by lambda RETURN type to the `@JvmName`-mangled
//! `@InlineOnly` `sumOfInt` (a private static fold loop with no legal `invokestatic`), so the lowerer
//! must splice its body and inline the `{ it * 2 }` lambda argument. Same-file, runs `box()`.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn list_fold_sum_sumof_count() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val xs = listOf(1, 2, 3, 4, 5)\n\
        \x20 if (xs.fold(0) { a, b -> a + b } != 15) return \"fold\"\n\
        \x20 if (xs.sum() != 15) return \"sum\"\n\
        \x20 if (xs.sumOf { it * 2 } != 30) return \"sumOf\"\n\
        \x20 if (xs.count() != 5) return \"count\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).as_deref(), Some("OK"));
}
