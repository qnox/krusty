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
    let actual =
        common::compile_and_run_box(src, "string_concatenation", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "sq7nullc17!null");
}
