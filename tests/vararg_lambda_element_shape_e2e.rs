//! Lambdas PACKED into a `vararg` parameter of function type.
//!
//! A vararg parameter is declared as the array (`vararg selectors: (T) -> R` is
//! `Array<out (T) -> R>`), but each argument packed into it has the ELEMENT type. Shaping a lambda
//! argument from the declared array asks whether an array is a function type, which it is not — so
//! the lambda is left unshaped, `it` has no type, and every member read on it is "unresolved
//! reference". The first argument happened to survive only because its position coincided with the
//! parameter's, so the defect showed up as "the second lambda onwards".
use super::common;

#[test]
fn every_lambda_packed_into_a_vararg_is_shaped_by_its_element() {
    const MAIN: &str = "package repro\n\
        class Endpoint(val path: String, val method: String)\n\
        fun describeAll(vararg selectors: (Endpoint) -> String): String =\n\
            selectors.joinToString(\",\") { it(Endpoint(\"/a\", \"GET\")) }\n\
        fun box(): String {\n\
            val none = describeAll()\n\
            val one = describeAll({ it.path })\n\
            val two = describeAll({ it.path }, { it.method })\n\
            val three = describeAll({ it.path }, { it.method }, { it.path + it.method })\n\
            if (none != \"\") return \"fail none: \" + none\n\
            if (one != \"/a\") return \"fail one: \" + one\n\
            if (two != \"/a,GET\") return \"fail two: \" + two\n\
            if (three != \"/a,GET,/aGET\") return \"fail three: \" + three\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "lambdas packed into a vararg");
}

#[test]
fn a_generic_vararg_of_lambdas_is_shaped_by_its_element() {
    // The element carries the callable's own type variable, so the shape has to substitute it.
    const MAIN: &str = "package repro\n\
        class Endpoint(val path: String, val method: String)\n\
        fun <T> pickAll(subject: T, vararg selectors: (T) -> String): String =\n\
            selectors.joinToString(\",\") { it(subject) }\n\
        fun box(): String {\n\
            val endpoint = Endpoint(\"/a\", \"GET\")\n\
            val two = pickAll(endpoint, { it.path }, { it.method })\n\
            val explicit = pickAll<Endpoint>(endpoint, { it.method }, { it.path })\n\
            if (two != \"/a,GET\") return \"fail two: \" + two\n\
            if (explicit != \"GET,/a\") return \"fail explicit: \" + explicit\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "generic vararg of lambdas");
}

#[test]
fn a_spread_argument_still_passes_the_whole_array() {
    // The mirror case: `*arrayOf(...)` IS the declared array, so it must keep the declared type and
    // not be shaped as an element.
    const MAIN: &str = "package repro\n\
        class Endpoint(val path: String, val method: String)\n\
        fun describeAll(vararg selectors: (Endpoint) -> String): String =\n\
            selectors.joinToString(\",\") { it(Endpoint(\"/a\", \"GET\")) }\n\
        fun box(): String {\n\
            val selectors: Array<(Endpoint) -> String> = arrayOf({ e -> e.path }, { e -> e.method })\n\
            val spread = describeAll(*selectors)\n\
            return if (spread == \"/a,GET\") \"OK\" else \"fail: \" + spread\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "spread into a vararg");
}

#[test]
fn a_lambda_inside_a_branch_still_reaches_the_vararg() {
    // The element type is needed only where a lambda is shaped. Taking it on the shared parameter
    // instead double-unwraps it for every OTHER argument, because the ordinary argument path takes
    // the element of a final vararg itself — the expectation then collapsed to an error type and
    // this program, which master and kotlinc both accept, was rejected outright.
    const MAIN: &str = "package repro\n\
        class Endpoint(val path: String, val method: String)\n\
        fun describeAll(vararg selectors: (Endpoint) -> String): String =\n\
            selectors.joinToString(\",\") { it(Endpoint(\"/a\", \"GET\")) }\n\
        fun box(): String {\n\
            val byPath = true\n\
            val chosen = describeAll(if (byPath) { { it.path } } else { { it.method } })\n\
            val whenChosen = describeAll(when (byPath) { true -> { { it.method } } else -> { { it.path } } })\n\
            if (chosen != \"/a\") return \"fail if: \" + chosen\n\
            if (whenChosen != \"GET\") return \"fail when: \" + whenChosen\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a lambda inside a branch");
}

#[test]
fn ordinary_arguments_to_a_final_vararg_are_untouched() {
    // The must-not-touch families: a vararg of a primitive and a vararg whose element is ITSELF an
    // array. `nested(arrayOf(), arrayOf("x"))` is the sharp one — an argument that is an array
    // reaching a parameter declared as an array of arrays.
    const MAIN: &str = "package repro\n\
        fun sumAll(vararg xs: Int): Int = xs.sum()\n\
        fun nested(vararg rows: Array<String>): String =\n\
            rows.size.toString() + \":\" + rows.joinToString(\";\") { it.joinToString(\",\") }\n\
        fun box(): String {\n\
            if (sumAll(3, 4) != 7) return \"fail sum: \" + sumAll(3, 4)\n\
            val rows = nested(arrayOf(), arrayOf(\"x\", \"y\"))\n\
            if (rows != \"2:;x,y\") return \"fail rows: \" + rows\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "ordinary vararg arguments");
}

#[test]
fn a_long_vararg_argument_keeps_its_declared_width() {
    // A box run cannot see this one: after the double unwrap the expectation was an error type, so
    // `longs(1, 2)` lost its `Long` expectation and each constant was loaded as an `int` and
    // widened (`iconst_1; i2l`) instead of `lconst_1`. The program still summed to 3 — only the
    // bytes drifted from kotlinc's. This pins the load, so the drift fails the test.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "package repro\n\
        fun longs(vararg xs: Long): Long = xs.sum()\n\
        fun box(): String = if (longs(1, 2) == 3L) \"OK\" else \"fail\"\n";
    let Some(classes) =
        common::compile_in_process(SRC, "Main", std::slice::from_ref(&stdlib), Some(&jdk))
    else {
        panic!(
            "{:?}",
            common::front_end_diagnostics(SRC, std::slice::from_ref(&stdlib), Some(&jdk))
        );
    };
    let work = std::env::temp_dir().join(format!("krusty_vararg_long_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch dir");
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&["-c", "-cp", &work.to_string_lossy(), "repro.MainKt"])
        .expect("javap unavailable");
    let box_body = dumped
        .split("public static final java.lang.String box();")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !box_body.contains("i2l"),
        "a constant packed into a `vararg xs: Long` loads as a long, not a widened int: {box_body}"
    );
}
