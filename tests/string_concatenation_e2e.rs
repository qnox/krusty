//! A `String.plus` chain, a `String` addition and a string template are one concatenation, as
//! kotlinc's `FlattenStringConcatenationLowering` collects them: nested concatenations contribute
//! their own arguments, and each run of constant arguments merges into one `String` constant.
use super::common;

fn assert_matches_kotlinc(name: &str, src: &str) {
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn plus_chains_flatten_into_one_concatenation() {
    assert_matches_kotlinc(
        "FlattenedPlus",
        "fun sink(value: Any?) {}\n\
         fun f(x: Int, s: String, n: String?, c: Char, l: Long) {\n\
         \x20   sink(\"a\" + x)\n\
         \x20   sink(s + x + \"c\" + 'd')\n\
         \x20   sink(s + (\"q\" + x))\n\
         \x20   sink(n + \"x\")\n\
         \x20   sink(n + null)\n\
         \x20   sink(c + \"s\")\n\
         \x20   sink(\"l\" + l + 1.5f + true + null)\n\
         \x20   sink(\"\" + x)\n\
         }\n",
    );
}

#[test]
fn templates_inside_a_concatenation_contribute_their_parts() {
    assert_matches_kotlinc(
        "FlattenedTemplates",
        "fun sink(value: Any?) {}\n\
         fun f(x: Int, s: String) {\n\
         \x20   sink(\"${s}a\" + \"b${x}\" + (1 + 2) + 'c')\n\
         \x20   sink(\"<${\"[$x]\"}>\")\n\
         }\n",
    );
}

#[test]
fn a_nullable_to_string_operand_appends_its_receiver() {
    assert_matches_kotlinc(
        "FlattenedNullableToString",
        "fun sink(value: Any?) {}\n\
         fun f(a: Any?) {\n\
         \x20   sink(\"a\" + a.toString())\n\
         \x20   sink(a.toString())\n\
         }\n",
    );
}

#[test]
fn a_flattened_concatenation_keeps_kotlin_values() {
    let src = "fun box(): String {\n\
               \x20   val x = 7\n\
               \x20   val n: String? = null\n\
               \x20   val a: Any? = null\n\
               \x20   val s = \"s\" + (\"q\" + x) + n + 'c' + 1L + \"${x}!\" + a.toString()\n\
               \x20   return s\n\
               }\n";
    let actual = common::compile_and_run_box(
        src,
        "string_concatenation",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "sq7nullc17!null");
}

/// Kotlin source for `fun <name>(): String = "<'a' * count><tail>"`, with `tail` in source spelling.
fn long_constant(name: &str, count: usize, tail: &str) -> String {
    format!("fun {name}(): String = \"{}{tail}\"\n", "a".repeat(count))
}

/// The sources of constants around the 65,535-byte modified-UTF-8 limit of one constant-pool entry:
/// exactly at it, one byte over, a three-byte character crossing it, and a surrogate pair whose
/// low half starts the second piece.
fn boundary_constants() -> String {
    [
        long_constant("exact", 65_535, ""),
        long_constant("over", 65_536, ""),
        long_constant("wide", 65_534, "\\u0800"),
        long_constant("pair", 65_532, "\\uD83D\\uDE00"),
        "fun concatenated(x: Int): String = \"a\".plus(exact()) + x + over()\n".to_string(),
    ]
    .concat()
}

#[test]
fn constants_beyond_one_pool_entry_split_like_kotlinc() {
    assert_matches_kotlinc("LongConstants", &boundary_constants());
}

#[test]
fn split_constants_rebuild_their_text() {
    let src = format!(
        "{}fun box(): String {{\n\
         \x20   if (exact().length != 65535) return \"exact ${{exact().length}}\"\n\
         \x20   if (over().length != 65536) return \"over ${{over().length}}\"\n\
         \x20   val w = wide()\n\
         \x20   if (w.length != 65535 || w[65534] != '\\u0800' || w[65533] != 'a') return \"wide\"\n\
         \x20   val p = pair()\n\
         \x20   if (p.length != 65534 || p[65532] != '\\uD83D' || p[65533] != '\\uDE00') return \"pair\"\n\
         \x20   return \"OK\"\n\
         }}\n",
        boundary_constants()
    );
    let actual = common::compile_and_run_box(
        &src,
        "long_constants",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
