//! Callable-reference adaptation for defaults, varargs, generic expected types, and return coercion.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn adapt_trailing_default_argument() {
    const SRC: &str = "fun foo(x: String, y: String = \"K\"): String = x + y\n\
        fun call(f: (String) -> String, x: String): String = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(run(SRC).expect("adapt trailing default"), "OK");
}

#[test]
fn adapt_trailing_default_through_generic_expected_type() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(
        run(SRC).expect("adapt trailing default through generic expected type"),
        "OK"
    );
}

#[test]
fn adapt_trailing_default_through_repeated_generic_constraint() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun <T> call(f: (T) -> T, x: T): T = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(
        run(SRC).expect("adapt trailing default through repeated generic constraint"),
        "OK"
    );
}

#[test]
fn multiple_adapted_references_share_call_type_bindings() {
    const SRC: &str = "fun text(x: String, suffix: Char = 'K'): String = x + suffix\n\
        fun number(x: Int, suffix: Char = 'K'): Int = x + suffix.code\n\
        fun <T> combine(first: (T) -> T, second: (T) -> T): String = \"unused\"\n\
        fun bad(): String = combine(::text, ::number)\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("inapplicable candidate(s)") && message.contains("number")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn adapted_reference_selects_overloaded_generic_hof() {
    const SRC: &str = "fun target(x: String, suffix: Char = 'K'): String = x + suffix\n\
        fun apply(block: (Int) -> String, value: Int): String = \"Fail\"\n\
        fun <T, U> apply(block: (T) -> U, value: T): U = block(value)\n\
        fun box(): String = apply(::target, \"O\")\n";
    assert_eq!(run(SRC).as_deref(), Some("OK"));
}

#[test]
fn adapt_generic_expected_type_uses_named_sibling_argument_binding() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = call(x = \"O\", f = ::foo)\n";
    assert_eq!(
        run(SRC).expect("adapt generic expected type with named sibling argument"),
        "OK"
    );
}

#[test]
fn adapt_generic_expected_type_preserves_named_argument_return_precision() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = if (call(x = \"O\", f = ::foo).length == 2) \"OK\" else \"Fail\"\n";
    assert_eq!(
        run(SRC).expect("named adapted reference return remains String"),
        "OK"
    );
}

#[test]
fn adapt_generic_expected_type_uses_parameter_variance() {
    const SRC: &str = "fun foo(x: CharSequence, y: Char = 'K'): String = x.toString() + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(
        run(SRC).expect("adapted reference accepts a wider target parameter"),
        "OK"
    );
}

#[test]
fn adapted_overload_is_selected_from_generic_expected_type() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun foo(x: Int, y: Char = 'F'): String = x.toString() + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(run(SRC).expect("select adapted String overload"), "OK");
}

#[test]
fn adapted_parameter_variance_boxes_for_the_target_parameter() {
    const SRC: &str = "fun foo(x: Number, y: Char = 'K'): String = x.toString() + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = if (call(::foo, 1) == \"1K\") \"OK\" else \"Fail\"\n";
    assert_eq!(
        run(SRC).expect("box adapted Int argument for Number target"),
        "OK"
    );
}

#[test]
fn adapted_covariant_return_boxes_for_the_expected_return() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): Int = x.length + y.code\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = if (call<String, Any>(::foo, \"OK\") == 77) \"OK\" else \"Fail\"\n";
    assert_eq!(
        run(SRC).expect("box adapted Int return for Any expectation"),
        "OK"
    );
}

#[test]
fn adapted_covariant_value_class_return_boxes_before_widening() {
    const SRC: &str = "@JvmInline value class Id(val value: String)\n\
        fun foo(x: String, y: Char = 'K'): Id = Id(x + y)\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String {\n\
        \x20 val result: Any = call<String, Any>(::foo, \"O\")\n\
        \x20 val id = result as Id\n\
        \x20 return if (id.value == \"OK\") \"OK\" else \"Fail\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("box adapted value-class return before widening"),
        "OK"
    );
}

#[test]
fn cross_file_adapted_value_class_return_boxes_before_widening() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun foo(x: String, y: Char = 'K'): Id = Id(x + y)\n",
        ),
        (
            "Use",
            "package sample\n\
             fun call(f: (String) -> Any): Any = f(\"O\")\n\
             fun box(): String {\n\
             \x20 val id = call(::foo) as Id\n\
             \x20 return id.value\n\
             }\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_nullable_value_class_return_boxes_null_safely_before_widening() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun foo(x: String, y: Char = 'K'): Id? = if (x.isEmpty()) null else Id(x + y)\n",
        ),
        (
            "Use",
            "package sample\n\
             fun call(f: (String) -> Any?): Any? = f(\"O\")\n\
             fun box(): String = if (call(::foo) is Id) \"OK\" else \"Fail\"\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_adapted_value_class_parameter_materializes_simple_default() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun foo(x: Id, y: Char = 'K'): String = x.value + y\n",
        ),
        (
            "Use",
            "package sample\n\
             fun call(f: (Id) -> String): String = f(Id(\"O\"))\n\
             fun box(): String = call(::foo)\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_value_class_constructor_default_reports_materialization_error() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun foo(x: String, y: Id = Id(\"K\")): String = x + y.value\n",
            "package sample\n\
             fun call(f: (String) -> String): String = f(\"O\")\n\
             fun bad(): String = call(::foo)\n",
        ],
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        vec![
            "cannot adapt cross-file reference 'foo': default value for parameter 'y' is not \
             available at this call site"
                .to_string()
        ]
    );
}

#[test]
fn cross_file_value_class_reference_coerces_return_to_unit() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun consume(id: Id): String = if (id.value == \"OK\") id.value else error(\"bad\")\n",
        ),
        (
            "Use",
            "package sample\n\
             fun call(block: (Id) -> Unit) { block(Id(\"OK\")) }\n\
             fun box(): String { call(::consume); return \"OK\" }\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_value_class_reference_drops_vararg_with_ordinary_call() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\n\
             @JvmInline value class Id(val value: String)\n\
             fun render(id: Id, vararg suffix: String): String = id.value + suffix.size\n",
        ),
        (
            "Use",
            "package sample\n\
             fun call(block: (Id) -> String): String = block(Id(\"OK\"))\n\
             fun box(): String = if (call(::render) == \"OK0\") \"OK\" else \"Fail\"\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn same_file_adapted_value_class_parameter_uses_boxed_lambda_boundary() {
    const SRC: &str = "@JvmInline value class Id(val value: String)\n\
        fun foo(x: Id, y: Char = 'K'): String = x.value + y\n\
        fun call(f: (Id) -> String): String = f(Id(\"O\"))\n\
        fun box(): String = call(::foo)\n";
    assert_eq!(run(SRC).as_deref(), Some("OK"));
}

#[test]
fn adapted_overload_specificity_uses_target_parameter_types() {
    const SRC: &str = "fun foo(x: Any, y: Char = 'F'): Any = \"Fail\"\n\
        fun foo(x: CharSequence, y: Char = 'K'): Any = x.toString() + y\n\
        fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
        fun box(): String = call(::foo, \"O\") as String\n";
    assert_eq!(
        run(SRC).expect("select more-specific CharSequence adapted overload"),
        "OK"
    );
}

#[test]
fn public_cross_file_adapted_reference_materializes_source_default() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Target",
            "package sample\nfun foo(x: String, y: Char = 'K'): String = x + y\n",
        ),
        (
            "Use",
            "package sample\n\
             import sample.*\n\
             fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
             fun box(): String = call(::foo, \"O\")\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn explicit_type_arguments_constrain_adapted_reference() {
    const SRC: &str = "fun foo(x: String, y: Char = 'K'): String = x + y\n\
        fun <T, U> hold(f: (T) -> U): U = hold(f)\n\
        fun bad(): String = hold<Int, String>(::foo)\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("inapplicable candidate(s)") && message.contains("foo")),
        "{diagnostics:?}"
    );
}

#[test]
fn generic_bound_constrains_adapted_reference() {
    const SRC: &str = "fun foo(x: Int, y: Char = 'K'): String = x.toString() + y\n\
        fun <T : CharSequence, U> hold(f: (T) -> U): U = hold(f)\n\
        fun bad(): String = hold(::foo)\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(!diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn private_cross_file_adapted_reference_is_inaccessible() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "package sample\nprivate fun foo(x: String, y: Char = 'K'): String = x + y\n",
            "package sample\n\
             fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
             fun bad(): String = call(::foo, \"O\")\n",
        ],
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("cannot access 'foo'") && message.contains("private")),
        "{diagnostics:?}"
    );
}

// Coercion to `Unit`: a reference to a value-returning function passed where `() -> Unit` /
// `(T) -> Unit` is expected — the adapter calls the target and discards its result.
#[test]
fn adapt_coercion_to_unit() {
    const SRC: &str = "var log = \"\"\n\
        fun foo(x: String): String { log += x; return x }\n\
        fun call(f: (String) -> Unit, x: String) { f(x) }\n\
        fun box(): String {\n\
        \x20 call(::foo, \"OK\")\n\
        \x20 return log\n\
        }\n";
    assert_eq!(run(SRC).expect("adapt coercion to Unit"), "OK");
}

// A trailing `vararg` is dropped: the adapter passes an empty array.
#[test]
fn adapt_trailing_empty_vararg() {
    const SRC: &str = "fun foo(x: String, vararg y: String): String =\n\
        \x20 if (y.isEmpty()) x + \"K\" else \"Fail\"\n\
        fun call(f: (String) -> String, x: String): String = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(run(SRC).expect("adapt trailing vararg"), "OK");
}

// Discarding a WIDE (2-slot) result (Long) in the coercion adapter's statement position.
#[test]
fn adapt_coercion_wide_discard() {
    const SRC: &str = "var n = 0L\n\
        fun foo(x: Long): Long { n = x; return x }\n\
        fun call(f: (Long) -> Unit) { f(9L) }\n\
        fun box(): String { call(::foo); return if (n == 9L) \"OK\" else \"Fail\" }\n";
    assert_eq!(run(SRC).expect("wide discard"), "OK");
}

#[test]
fn adapt_coercion_primitive_discard() {
    const SRC: &str = "var n = 0\n\
        fun foo(x: Int): Boolean { n = x; return true }\n\
        fun call(f: (Int) -> Unit) { f(7) }\n\
        fun box(): String { call(::foo); return if (n == 7) \"OK\" else \"Fail\" }\n";
    assert_eq!(run(SRC).expect("primitive discard"), "OK");
}

// Base support: a plain call to a function with a trailing vararg AND a defaulted fixed parameter,
// omitting the vararg (empty). Previously rejected ("expects at least 1 arg") / not lowered.
#[test]
fn default_and_empty_vararg_call() {
    const SRC: &str =
        "fun foo(s: String = \"K\", vararg t: String): String = s + t.size.toString()\n\
        fun box(): String = if (foo() == \"K0\" && foo(\"A\") == \"A0\") \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("default + empty vararg"), "OK");
}

// Combined: drop a trailing default AND a trailing vararg, coercing to Unit. Now supported because the
// base $default stub for a vararg function is emitted.
#[test]
fn adapt_default_and_vararg_to_unit() {
    const SRC: &str = "var log = \"\"\n\
        fun foo(s: String = \"K\", vararg t: String): Boolean {\n\
        \x20 log += s; log += t.size.toString(); return true\n\
        }\n\
        fun bar(f: () -> Unit) { f() }\n\
        fun box(): String { bar(::foo); return if (log == \"K0\") \"OK\" else \"Fail: $log\" }\n";
    assert_eq!(run(SRC).expect("adapt default+vararg to Unit"), "OK");
}

// Vararg COLLECTION: a reference to `of(vararg args)` adapted to a fixed-arity function type — the
// extra parameters are collected into the vararg array.
#[test]
fn adapt_vararg_collection() {
    const SRC: &str =
        "fun of(vararg args: Any): String = args[0].toString() + args[1].toString()\n\
        fun foo(b: (Any, Any) -> String): String = b(\"O\", \"K\")\n\
        fun box(): String = foo(::of)\n";
    assert_eq!(run(SRC).expect("vararg collection"), "OK");
}

// Vararg collection with PRIMITIVE collected arguments: each is boxed into the Object[] vararg.
#[test]
fn adapt_vararg_collection_primitive() {
    const SRC: &str = "fun of(vararg args: Any): Int = (args[0] as Int) + (args[1] as Int)\n\
        fun foo(b: (Int, Int) -> Int): Int = b(3, 4)\n\
        fun box(): String = if (foo(::of) == 7) \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("vararg collection primitive"), "OK");
}
