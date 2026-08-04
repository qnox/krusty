//! Vararg call shapes kotlinc accepts but krusty rejected — all false positives:
//!   s1  spread BEFORE a positional element (`top(*s, "b")`): the tail expansion assumed
//!       element-form only, so the spread's `Array<String>` failed the element fit.
//!   s2  EXTENSION vararg with MIXED element + spread (`b.seg("a", *s)`): reported an arity error
//!       ("expects 1 args, got 2") — the extension path never expanded the vararg slot for a mix.
//!   s3  the same mix when the vararg is followed by a defaulted parameter
//!       (`fun B.segd(vararg s: String, flag: Boolean = false)`), the ktor
//!       `URLBuilder.appendPathSegments("admin", "realms", *segments)` shape.
//!   s4  a CLASSPATH extension with those shapes, plus `flag = true` NAMED after positional
//!       vararg elements — the classpath candidate reported "none of the following candidates
//!       is applicable".
//! Each box() checks the RUNTIME packing (joined elements + flag), not just acceptance.
use super::common;

#[test]
fn source_top_level_named_after_vararg_elements() {
    // `topd("O", "K", flag = true)`: positional elements fill the vararg, then a NAMED argument
    // sets the trailing defaulted parameter. The top-level source path mis-mapped the named
    // argument onto the vararg slot and reported an argument type mismatch.
    const SRC: &str = "fun topd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun box(): String {\n\
        \x20 val got = topd(\"O\", \"K\", flag = true)\n\
        \x20 return if (got == \"OKtrue\") \"OK\" else \"F:\" + got\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "named_after_vararg")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

#[test]
fn source_extension_mixed_element_and_spread() {
    const SRC: &str = "class B\n\
        fun B.seg(vararg s: String): String = s.joinToString(\"\")\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"K\")\n\
        \x20 return if (B().seg(\"O\", *xs) == \"OK\") \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "ext_mixed_spread")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

#[test]
fn source_extension_mixed_spread_with_trailing_default() {
    const SRC: &str = "class B\n\
        fun B.segd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"K\")\n\
        \x20 val got = B().segd(\"O\", *xs)\n\
        \x20 return if (got == \"OKfalse\") \"OK\" else \"F:\" + got\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "mixed_spread_default")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

#[test]
fn source_extension_named_after_vararg_elements() {
    // Fully-mapped named vararg calls (one element, several, and with a spread): the slot map is
    // complete, and the slot lowering must still PACK the vararg — passing the stored element
    // where the descriptor spells the array was a VerifyError, not a diagnostic.
    const SRC: &str = "class B\n\
        fun B.segd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"c\")\n\
        \x20 if (B().segd(\"a\", flag = true) != \"atrue\") return \"F1\"\n\
        \x20 if (B().segd(\"a\", \"b\", flag = true) != \"abtrue\") return \"F2\"\n\
        \x20 if (B().segd(\"a\", *xs, flag = true) != \"actrue\") return \"F3\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "ext_named_after_vararg")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

#[test]
fn source_cross_file_extension_mixed_spread() {
    // The same-module SIBLING-FILE form: the vararg position must come from the checker's
    // record, not from a declaration this file does not contain.
    let sources = [
        (
            "Lib.kt",
            "class B\n\
             fun B.seg(vararg s: String): String = s.joinToString(\"\")\n",
        ),
        (
            "Main.kt",
            "fun box(): String {\n\
             \x20 val xs = arrayOf(\"b\", \"c\")\n\
             \x20 val got = B().seg(\"a\", *xs)\n\
             \x20 return if (got == \"abc\") \"OK\" else \"F:\" + got\n\
             }\n",
        ),
    ];
    if let Some(out) = common::compile_and_run_files_with_stdlib(&sources) {
        assert_eq!(out, "OK", "cross-file mixed spread");
    } else {
        // A lowering decline (file skip) is tolerated; a FRONTEND diagnostic is the false
        // positive this suite exists to prevent.
        let stdlib = common::stdlib_jar();
        let jdk = common::jdk_modules();
        let texts: Vec<&str> = sources.iter().map(|(_, text)| *text).collect();
        let diagnostics =
            common::front_end_diagnostics_files(&texts, &[stdlib], Some(jdk.as_path()));
        assert!(
            diagnostics.is_empty(),
            "cross-file mixed spread must not produce diagnostics: {diagnostics:?}"
        );
    }
}

#[test]
fn source_named_single_element_vararg_prohibited() {
    // kotlinc's exact diagnostic pair for `s = "a"` on a vararg: the array-type mismatch AND the
    // named-form prohibition. The array form (`s = arr`) and spread (`s = *arr`) stay clean.
    const SRC: &str = "fun topd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun bad(): String = topd(s = \"a\")\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("argument type mismatch")),
        "missing type mismatch: {diagnostics:?}"
    );
    assert!(
        diagnostics.iter().any(|message| message
            .contains("assigning single elements to varargs in named form is prohibited.")),
        "missing named-form prohibition: {diagnostics:?}"
    );

    const OK_SRC: &str = "fun topd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun good(xs: Array<String>): String = topd(s = xs) + topd(s = *xs, flag = true)\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(OK_SRC) else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "array/spread named forms are legal"
    );
}

#[test]
fn source_named_in_position_before_positional_vararg() {
    // A named argument in its OWN position followed by positionals is legal Kotlin: the
    // positional counter must skip name-bound parameters or `"x"` lands on `a` and reports a
    // false type mismatch.
    const SRC: &str =
        "fun ok2(a: Int, vararg s: String): String = a.toString() + s.joinToString(\"\")\n\
        fun ok3(a: Int, b: Int = 0, vararg s: String): String =\n\
        \x20 a.toString() + b + s.joinToString(\"\")\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"z\")\n\
        \x20 if (ok2(a = 1, \"x\", \"y\") != \"1xy\") return \"F1:\" + ok2(a = 1, \"x\", \"y\")\n\
        \x20 if (ok3(1, b = 2, *xs) != \"12z\") return \"F2:\" + ok3(1, b = 2, *xs)\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "named_in_position")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

#[test]
fn source_named_spread_wrong_element_type_rejected() {
    // `s = *ints` on a `String` vararg is kotlinc's compile-time mismatch — accepting it
    // produces an `ArrayStoreException` inside the spread builder at runtime.
    const SRC: &str = "fun topd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun bad(ints: Array<Int>): String = topd(s = *ints)\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("argument type mismatch")),
        "ill-typed named spread must be a compile-time mismatch: {diagnostics:?}"
    );
}

#[test]
fn source_top_level_named_after_spread() {
    // A plain-name call mixing a spread with a trailing named argument: previously diverted to
    // the name-only spread lowering, which bailed the file.
    const SRC: &str = "fun topd(vararg s: String, flag: Boolean = false): String =\n\
        \x20 s.joinToString(\"\") + flag\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"O\", \"K\")\n\
        \x20 val got = topd(*xs, flag = true)\n\
        \x20 return if (got == \"OKtrue\") \"OK\" else \"F:\" + got\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "named_after_spread_top")
            .expect("strict helper always returns Some"),
        "OK"
    );
}

const LIB: &str = "package lib\n\
    class B\n\
    fun B.segd(vararg s: String, flag: Boolean = false): String =\n\
    \x20 s.joinToString(\"\") + flag\n";

// The ktor `URLBuilder.appendPathSegments` shape: the SAME name declares a vararg overload and a
// `List` overload, both with a trailing defaulted parameter.
const OVERLOAD_LIB: &str = "package lib\n\
    class U\n\
    fun U.seg(vararg components: String, encodeSlash: Boolean = false): String =\n\
    \x20 components.joinToString(\"/\") + encodeSlash\n\
    fun U.seg(segments: List<String>, encodeSlash: Boolean = false): String =\n\
    \x20 \"L:\" + segments.joinToString(\"/\") + encodeSlash\n";

#[test]
fn classpath_overloaded_vararg_mixed_element_and_spread() {
    const MAIN: &str = "import lib.U\n\
        import lib.seg\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"b\", \"c\")\n\
        \x20 val got = U().seg(\"a\", *xs)\n\
        \x20 return if (got == \"a/b/cfalse\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_overload_vararg_mix", OVERLOAD_LIB, MAIN)
    {
        assert_eq!(out, "OK", "overloaded classpath mixed element+spread");
    }
}

#[test]
fn classpath_overloaded_vararg_named_after_elements() {
    const MAIN: &str = "import lib.U\n\
        import lib.seg\n\
        fun box(): String {\n\
        \x20 val got = U().seg(\"a\", \"b\", encodeSlash = true)\n\
        \x20 return if (got == \"a/btrue\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) =
        common::expect_box_run_against("cp_overload_vararg_named", OVERLOAD_LIB, MAIN)
    {
        assert_eq!(
            out, "OK",
            "overloaded classpath named after vararg elements"
        );
    }
}

#[test]
fn classpath_overloaded_list_overload_still_selected() {
    const MAIN: &str = "import lib.U\n\
        import lib.seg\n\
        fun box(): String {\n\
        \x20 val got = U().seg(listOf(\"a\", \"b\"))\n\
        \x20 return if (got == \"L:a/bfalse\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_overload_list", OVERLOAD_LIB, MAIN) {
        assert_eq!(out, "OK", "List overload still selected");
    }
}

#[test]
fn classpath_extension_mixed_element_and_spread() {
    const MAIN: &str = "import lib.B\n\
        import lib.segd\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"K\")\n\
        \x20 val got = B().segd(\"O\", *xs)\n\
        \x20 return if (got == \"OKfalse\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_vararg_mix", LIB, MAIN) {
        assert_eq!(out, "OK", "classpath mixed element+spread");
    }
}

#[test]
fn classpath_extension_named_after_vararg_elements() {
    const MAIN: &str = "import lib.B\n\
        import lib.segd\n\
        fun box(): String {\n\
        \x20 val got = B().segd(\"O\", \"K\", flag = true)\n\
        \x20 return if (got == \"OKtrue\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_vararg_named", LIB, MAIN) {
        assert_eq!(out, "OK", "classpath named after vararg elements");
    }
}

#[test]
fn classpath_extension_named_after_spread() {
    const MAIN: &str = "import lib.B\n\
        import lib.segd\n\
        fun box(): String {\n\
        \x20 val xs = arrayOf(\"O\", \"K\")\n\
        \x20 val got = B().segd(*xs, flag = true)\n\
        \x20 return if (got == \"OKtrue\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_vararg_spread_named", LIB, MAIN) {
        assert_eq!(out, "OK", "classpath named after spread");
    }
}
