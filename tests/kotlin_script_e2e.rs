//! Kotlin scripts (`.kts`) compile to the same class files as kotlinc's: a class named after the
//! file that extends the script template, runs the script's statements in its constructor, keeps the
//! value of a final expression in `$$result`, and gets a `main` that runs it.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

fn assert_script_matches_kotlin(name: &str, source: &str) {
    assert_module_matches_kotlinc(&[(name, source)]);
}

#[test]
fn statements_run_in_the_script_constructor_like_kotlinc() {
    assert_script_matches_kotlin(
        "hello.kts",
        "// greeting\n\
         val greeting = \"hello\"\n\
         println(greeting)\n\
         if (greeting.length > 3) println(greeting.length)\n",
    );
}

#[test]
fn script_members_and_nested_classes_compile_like_kotlinc() {
    assert_script_matches_kotlin(
        "members.kts",
        "class First(val s: String)\n\
         var count = 0\n\
         fun bump(by: Int): Int {\n\
         \x20   count += by\n\
         \x20   return count\n\
         }\n\
         class Second(val first: First)\n\
         val total = bump(2) + Second(First(\"x\")).first.s.length\n\
         val text get() = \"$count\"\n",
    );
}

#[test]
fn template_members_are_read_through_the_template_like_kotlinc() {
    assert_script_matches_kotlin(
        "params.kts",
        "val x = 1\n\
         fun g() = x + 1\n\
         println(args.size)\n\
         fun f() = args.size + g() + hashCode()\n\
         println(f())\n",
    );
}

#[test]
fn a_final_expression_is_the_script_result_like_kotlinc() {
    assert_script_matches_kotlin("result.kts", "val a = 2\na * 21\n");
    assert_script_matches_kotlin("counter.kts", "var x = 0\nx++\nx++\n");
    assert_script_matches_kotlin("nullable.kts", "val s: String? = null\ns?.length\n");
}

#[test]
fn a_final_statement_without_a_value_keeps_no_result_like_kotlinc() {
    assert_script_matches_kotlin("unit.kts", "val a = 2\nprintln(a)\n");
    assert_script_matches_kotlin("branch.kts", "val c = args.isEmpty()\nif (c) 1\n");
    assert_script_matches_kotlin("assignment.kts", "var a = 2\nif (true) {\n    a = 3\n}\n");
    assert_script_matches_kotlin(
        "throws.kts",
        "val a = 2\nthrow IllegalStateException(\"$a\")\n",
    );
}

#[test]
fn local_functions_follow_the_script_main_like_kotlinc() {
    assert_script_matches_kotlin(
        "local.kts",
        "var x: Int = 0\n\
         \n\
         if (true) {\n\
         \x20   fun foo(y: Int) = y + 20\n\
         \x20   x = foo(9)\n\
         }\n\
         \n\
         val rv = x\n",
    );
}

#[test]
fn a_declared_main_replaces_the_generated_one_like_kotlinc() {
    assert_script_matches_kotlin(
        "declared.kts",
        "fun main(): Int = 2\n\
         val rv = main()\n",
    );
}

#[test]
fn an_inline_script_function_compiles_like_kotlinc() {
    assert_script_matches_kotlin(
        "inlined.kts",
        "inline fun foo(l: (Int) -> Int): Int = l(1)\n\
         fun bar(p: Int): Int = p + 1\n\
         val rv = foo { x -> bar(x) }\n",
    );
}

#[test]
fn delegated_script_properties_have_no_dispatch_receiver_like_kotlinc() {
    assert_script_matches_kotlin(
        "delegated.kts",
        "import kotlin.reflect.KProperty\n\
         \n\
         class Delegate {\n\
         \x20   operator fun getValue(t: Any?, p: KProperty<*>): Int = 3\n\
         }\n\
         \n\
         val prop: Int by Delegate()\n\
         val reference = ::prop\n\
         val x = prop + reference.get()\n",
    );
}

#[test]
fn the_script_starts_at_its_file_header_like_kotlinc() {
    assert_script_matches_kotlin(
        "packaged.kts",
        "// a package\n\npackage p\n\nimport kotlin.math.max\n\nval m = max(1, 2)\n",
    );
    assert_script_matches_kotlin(
        "annotated.kts",
        "// annotated\n@file:Suppress(\"UNUSED\")\n\nval a = 1\n",
    );
    assert_script_matches_kotlin("shebang.kts", "#!/usr/bin/env kotlin\n\nval a = 1\n");
    assert_script_matches_kotlin("commented.kts", "\n// leading\nval a = 1\n");
}

#[test]
fn a_script_class_name_comes_from_its_file_like_kotlinc() {
    assert_script_matches_kotlin("build.gradle.kts", "val name = \"build\"\n");
    assert_script_matches_kotlin("my-script.kts", "val name = \"dashed\"\n");
}

#[test]
fn a_script_without_its_template_on_the_classpath_extends_nothing_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module_without_script_runtime(&[(
        "bare.kts",
        "fun f() = 1\nprintln(f())\n",
    )]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

/// The standard definition's `args: Array<String>` names `kotlin.Array` and `kotlin.String` by
/// identity: a script that imports a same-named `String` or `Array` keeps the template's
/// constructor and reads `args` as kotlinc does.
#[test]
fn imports_do_not_rebind_the_template_parameter_type_like_kotlinc() {
    assert_module_matches_kotlinc(&[
        (
            "shadow/Shadow.kt",
            "package shadow\n\
             class String\n\
             class Array<T>\n",
        ),
        (
            "shadowed.kts",
            "import shadow.String\n\
             import shadow.Array\n\
             println(args.size)\n\
             val mine: Array<String> = Array()\n",
        ),
    ]);
}
