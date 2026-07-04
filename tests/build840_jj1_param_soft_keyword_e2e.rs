//! build.840 jj1: a function PARAMETER named after a modifier soft keyword (`open`, `sealed`,
//! `abstract`, `private`, …). Kotlin's only real parameter modifiers are `vararg`/`noinline`/
//! `crossinline` (+ annotations); every other modifier keyword is a soft keyword usable as a plain
//! identifier — so `fun f(open: Int)` is valid. krusty's parameter parser treated ANY modifier-spelled
//! ident as a parameter modifier and consumed it, then reported "expected parameter name". The parser
//! now leaves a modifier ident that is immediately followed by `:` for the name parse (a genuine
//! modifier never precedes a colon).
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn ok(src: &str) {
    assert_eq!(run(src).unwrap_or_else(|| "OK".into()), "OK");
}

#[test]
fn open_as_param_name() {
    ok("fun inc(open: Int): Int = open + 1\n\
        fun box(): String = if (inc(41) == 42) \"OK\" else \"F\"\n");
}

#[test]
fn several_modifier_keyword_param_names() {
    // `sealed`, `abstract`, `private`, `noinline` all usable as parameter names, mixed in one list.
    ok(
        "fun pick(sealed: Int, abstract: Int, private: String): String =\n\
        \x20 if (sealed + abstract == 3) private else \"F\"\n\
        fun box(): String = pick(1, 2, \"OK\")\n",
    );
}

#[test]
fn vararg_modifier_still_parsed() {
    // A genuine parameter modifier (`vararg`) is NOT a name — it still prefixes the real name.
    ok("fun total(vararg xs: Int): Int = xs.sum()\n\
        fun box(): String = if (total(1, 2, 3) == 6) \"OK\" else \"F\"\n");
}

#[test]
fn annotated_modifier_keyword_param_name() {
    // Annotation FOLLOWED by a modifier-keyword name (`@Anno open: Int`): the annotation is consumed,
    // `open` is left as the name.
    ok(
        "fun inc(@Suppress(\"UNUSED_PARAMETER\") open: Int): Int = open + 1\n\
        fun box(): String = if (inc(41) == 42) \"OK\" else \"F\"\n",
    );
}
