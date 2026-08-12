//! krusty's diagnostics should match kotlinc's message and source location. For a set of erroneous
//! snippets, compile with both and assert the first error's file, line, column, and text match exactly.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedError {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

/// Extract the first `path:line:column: error: message` record. Compare the basename because both
/// compilers receive the same path, while their renderers may independently canonicalize its prefix.
fn first_error(output: &str) -> Option<ObservedError> {
    output.lines().find_map(|rendered| {
        let (location, message) = rendered.split_once("error:")?;
        let location = location.trim().trim_end_matches(':');
        let mut fields = location.rsplitn(3, ':');
        let column = fields.next()?.trim().parse().ok()?;
        let line = fields.next()?.trim().parse().ok()?;
        let path = fields.next()?.trim();
        let file = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_string();
        Some(ObservedError {
            file,
            line,
            column,
            message: message.trim().to_string(),
        })
    })
}

#[test]
fn generic_cast_is_accepted_by_both_frontends() {
    let source = "fun <T> materialize(): T = 42 as T";
    let root =
        std::env::temp_dir().join(format!("krusty_generic_cast_parity_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let kt = root.join("GenericCast.kt");
    fs::write(&kt, source).unwrap();
    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("kotlinc-out").to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skipping generic cast parity: kotlinc server unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc rejected generic cast: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "generic cast parity");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn constructor_infers_nested_function_type() {
    let diagnostics = common::front_end_diagnostics(
        "class C<T>(val consume: ((T) -> Unit)?)\n\
         fun bad() { val f: (String) -> Unit = { }; val c = C(f); c.consume!!(1) }",
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        ["argument type mismatch: actual type is 'Int', but 'String' was expected."]
    );
}

#[test]
fn callable_reference_bound_failure_is_inference_error() {
    let diagnostics = common::front_end_diagnostics(
        "interface Bound\n\
         fun foo(x: Int, y: Char = 'K'): String = \"\"\n\
         fun <T : Bound, U> hold(f: (T) -> U): U = hold(f)\n\
         fun bad(): String = hold(::foo)",
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        ["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
}

#[test]
fn constructor_header_lambda_this_matches_kotlinc() {
    let source = r#"
enum class Choice(val callback: () -> Enum.Companion) {
    RETAIN({ this })
}

open class Base(val callback: () -> Base.Companion) {
    companion object
}

class Derived : Base({ this })
"#;
    let root = std::env::temp_dir().join(format!(
        "krusty_enum_entry_this_parity_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let kt = root.join("EnumEntryThis.kt");
    fs::write(&kt, source).unwrap();
    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("kotlinc-out").to_string_lossy().into_owned(),
    ];
    let Some((kotlinc_code, kotlinc_stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skipping enum-entry this parity: kotlinc server unavailable");
        return;
    };
    let diagnostics = common::front_end_diagnostics(
        source,
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    );
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        kotlinc_code, 0,
        "kotlinc rejected enum this: {kotlinc_stderr}"
    );
    assert!(diagnostics.is_empty(), "krusty: {diagnostics:?}");
}

#[test]
fn errors_match_kotlinc_in_text_and_location() {
    let krusty = common::krusty_binary();

    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
        // A qualified expression commits to its lexical root before resolving later segments.
        // `java` is the local Int here, not the lower-priority JDK package, so both compilers must
        // diagnose `io` and must not backtrack to `java.io.File`.
        "fun f() { val java = 1; val file = java.io.File(\"A\") }",
        // The same rule applies to a top-level property root: package lookup is considered only when
        // the expression/value scope has no winning declaration named `java`.
        "val java = 1\nfun f() { val file = java.io.File(\"A\") }",
        "fun f(a: Int): String = a",
        "fun f(): String = null",
        "val x: String = null",
        "fun f(x: String): String = x\nfun g(): String = f(null)",
        "fun f() { var x: String = \"\"; x = null }",
        "fun f(): String { return 1 }",
        "fun f(): Int { val x = 1; x = 2; return x }",
        "fun f(x: Int): String { val y: String = x; return y }",
        "val x: String = 1",
        "fun f() { var x: String = \"\"; x = 1 }",
        "fun f(x: Int): Int = x\nfun g(): Int = f()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f<Int>()",
        "fun f(x: Int): Int = x\nfun g(): Int = f ()",
        "fun f(x: Int): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(x: Int): Int = x\nfun g(): Int = f(\"no\")",
        "fun f(): Array<String> = arrayOf()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "suspend fun <T> f(x: T): T = x\nsuspend fun g(): Int = f(1, 2)",
        "inline fun <reified T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T : Any> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T> f(x: T & Any): T & Any = x\nfun g() { f(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(x = null) }",
        "class C<T : CharSequence?>(val x: T)\nfun g() { C(null).x.length }",
        "val String.first: Char get() = 'x'\nfun bad(s: String?): Char = s.first",
        "class PairBox<T>(val a: T, val b: T)\nclass Host { val String.pair get() = PairBox(\"x\", null); fun bad(): Int = \"\".pair.b.length }",
        "class C<T : Number>(val a: T, val b: T)\nfun f() = C(1, \"bad\")",
        "class C<T>(val consume: (T) -> Unit)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume(1) }",
        "class C<T>(val consume: ((T) -> Unit)?)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume!!(1) }",
        "fun <T> inferred(x: T) = x\nfun g(): Int = inferred(1, 2)",
        "fun <T> f(x: T, y: Int = 1): T = x\nfun g(): Int = f(1, 2, 3)",
        "fun f(x: Int): Int = x\nfun f(x: String): Int = 0\nfun g(): Int = f(1, 2)",
        "fun f(x: Int = 1): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(a: Int = 0, b: String): String = b\nfun g(): String = f(a = 1)",
        "fun f(`a`: Int = 0, b: String): String = b\nfun g(): String = f(`a` = 1)",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, c = 2, b = \"ok\")",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, a = 2, b = \"ok\")",
        "fun f(a: Int = 0, b: String, vararg x: Int): Int = 0\nfun g(): Int = f()",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f()\n}",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f(1, 2)\n}",
        "fun g(): Int {\nfun choose(a: Int): Int = a\nfun choose(a: String, b: String, c: String): Int = 0\nreturn choose(1, 2)\n}",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f()",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun f(x: Int = 1): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun <T> choose(a: T): T = a; fun <T> choose(a: T, b: T): T = a }\nfun g(c: C): Int = c.choose(1, 2, 3)",
        "class C { fun choose(a: Int): Int = a; fun choose(a: String, b: String, c: String): Int = 0 }\nfun g(c: C): Int = c.choose(1, 2)",
        "open class Base { fun <T> choose(a: T): T = a }\nclass Child : Base()\nfun g(c: Child): Int = c.choose(1, 2)",
        "class C(val x: Int)\nfun g(): C = C()",
        "class C(val x: Int)\nfun g(): C = C(1, 2)",
        "class C(val x: Int = 1)\nfun g(): C = C(1, 2)",
        "class C(val a: Int) { constructor(a: String, b: String, c: String) : this(0) }\nfun g(): C = C(1, 2)",
        "fun f(vararg x: Int): Int = x.size\nfun g(): Int = f(\"no\")",
        "fun f(x: Int = \"no\"): Int = x",
        "fun f(x: String): Int = x.missing",
        "fun f(x: String): Int = x.`missing name`",
        "fun f(x: String?): Int? = x?.`missing name`",
        "fun f(x: String): Int = x.missing()",
        "class C { fun member(value: Int): Int = value }\nfun f(value: C?): Int = value.member(1)",
        "fun f(value: String?): String = value.substring(1)",
        "fun f(value: String?): String = value. /* gap */ substring(1)",
        "fun String.nonNullExtension(): Int = length\nfun f(value: String?): Int = value.nonNullExtension()",
        "class C(val block: () -> Int)\nfun f(value: C?): Int = value.block()",
        "fun <T : Any> T.nonNullGeneric(): Int = 1\nfun f(value: String?): Int = value.nonNullGeneric()",
        "class GenericHolder<T> { fun read(): Int = 1 }\nfun f(value: GenericHolder<String>?): Int = value.read()",
        "fun f(block: (() -> Int)?): Int = block.invoke()",
        "fun f(value: Any?): Boolean = value.equals(null)",
        "fun f(x: String): String = x.substring(\"no\")",
        "fun f(x: Int): Int = x.substring(1)",
        "fun f(): Int { if (1) return 1; return 0 }",
        "fun f(): Int = when { 1 -> 1; else -> 0 }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f() }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f(1, 2) }",
        "class C { fun unaryMinus(): C = this }\nfun g(): C = -C()",
        "class C { fun inc(): C = this }\nfun g(c: C) { var value = c; value++ }",
        "class C { fun plus(other: C): C = this }\nfun g(left: C, right: C): C = left + right",
        "class Parser\nfun Parser.decode(source: String): String = source\nfun Parser.decode(value: Int): String = value.toString()\nfun bad() { val parser = Parser(); val reference = parser::decode }",
        "fun cross(x: String, y: Any): String = \"A\"\nfun cross(x: CharSequence, y: CharSequence): String = \"B\"\nfun bad() { val reference: (String, String) -> String = ::cross }",
        "fun <T> applySame(block: (T) -> T, value: T): T = block(value)\nfun mismatched(x: String, suffix: Char = 'K'): Int = x.length + suffix.code\nfun bad(): Any = applySame(::mismatched, \"O\")",
        "fun foo(x: String, y: Char = 'K'): String = x + y\nfun <T, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold<Int, String>(::foo)",
        "fun foo(x: Int, y: Char = 'K'): String = x.toString() + y\nfun <T : CharSequence, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold(::foo)",
        // A deliberately unique type present on NO supplied classpath. Keep the spelling synthetic:
        // diagnostic regressions must not depend on or disclose a class from the scanned project.
        "fun f(p: DefinitelyAbsentClassifier): Int = 0",
        // An `is` whose TARGET type is unresolved reports the unresolved reference at the type's
        // span — never a compiler-specific "not supported" rejection.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier",
        // … and a failing type ARGUMENT is named at its own span, not the outer generic's.
        "fun f(p: Any) = p is Array<DefinitelyAbsentClassifier>",
        // Ordinary generic arguments retain an outer `Ty::Obj`; nested Error detection must inspect
        // that semantic shape instead of relying on `outer == Ty::Error`.
        "fun f(p: Any) = p is List<DefinitelyAbsentClassifier>",
        // When the OUTER name is the unresolvable one it is named first, not its type argument.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier<String>",
        // A nullable unresolved target resolves to the same reference diagnostic.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier?",
        // The `as` sibling reports identically.
        "fun f(p: Any) = p as DefinitelyAbsentClassifier",
        "fun f(p: Any) = p as List<DefinitelyAbsentClassifier>",
        "fun f(p: Any) = p is (DefinitelyAbsentClassifier) -> String",
        "fun f(p: Any) = p is Function1<DefinitelyAbsentClassifier, String>",
        "fun f(p: Any) = p is Function1<Any?, Any?>",
        // Casts permit an erased function shape, so an unresolved parameter remains the primary
        // diagnostic. (`is` is different and is pinned by the unsupported-shape test below.)
        "fun f(p: Any) = p as (DefinitelyAbsentClassifier) -> String",
    ];

    let root = std::env::temp_dir().join(format!("krusty_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let mut mismatches = Vec::new();
    for (i, src) in cases.iter().enumerate() {
        let kt = root.join(format!("t{i}.kt"));
        fs::write(&kt, src).unwrap();

        let kr = Command::new(&krusty)
            .args(["-d", root.join("o").to_str().unwrap()])
            .arg(&kt)
            .output()
            .unwrap();
        let kr_error = first_error(String::from_utf8_lossy(&kr.stderr).as_ref())
            .or_else(|| first_error(&String::from_utf8_lossy(&kr.stdout)));

        // Reference compile via the persistent kotlinc server (one reused JVM, not a CLI spawn/case).
        let args = vec![
            kt.to_string_lossy().into_owned(),
            "-d".to_string(),
            root.join("ko").to_string_lossy().into_owned(),
        ];
        let Some((_, kc_err)) = common::kotlinc_compile(&args) else {
            eprintln!("skipping diagnostics_match_kotlinc: kotlinc server unavailable");
            return;
        };
        let kc_error = first_error(&kc_err);

        if kr_error != kc_error {
            mismatches.push(format!(
                "diagnostic mismatch for {src:?}\n krusty: {kr_error:?}\n kotlinc: {kc_error:?}"
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn jvm_builtin_errors_match_kotlinc() {
    let krusty = common::krusty_binary();
    let root = std::env::temp_dir().join(format!("krusty_jvm_builtin_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let source = "fun f(a: Array<String>): Array<String> = a.clone(1)";
    let kt = root.join("CloneError.kt");
    fs::write(&kt, source).unwrap();
    let stdlib = common::stdlib_jar();

    let kr = Command::new(&krusty)
        .args(["-d", root.join("o").to_str().unwrap(), "-cp"])
        .arg(&stdlib)
        .arg(&kt)
        .output()
        .unwrap();
    let kr_error = first_error(String::from_utf8_lossy(&kr.stderr).as_ref())
        .or_else(|| first_error(&String::from_utf8_lossy(&kr.stdout)));

    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("ko").to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.to_string_lossy().into_owned(),
    ];
    let Some((_, kc_err)) = common::kotlinc_compile(&args) else {
        eprintln!("skipping jvm_builtin_errors_match_kotlinc: kotlinc server unavailable");
        return;
    };
    let kc_error = first_error(&kc_err);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(kr_error, kc_error, "diagnostic mismatch for {source:?}");
}

#[test]
fn kotlin_internal_exact_requires_an_exact_argument_type() {
    let root = std::env::temp_dir().join(format!(
        "krusty_kotlin_internal_exact_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let ordinary = root.join("Ordinary.kt");
    fs::write(
        &ordinary,
        "fun <T> ordinary(value: T) {}\nfun use() = ordinary<CharSequence>(\"x\")",
    )
    .unwrap();
    let ordinary_args = vec![
        ordinary.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("ordinary-out").to_string_lossy().into_owned(),
    ];
    let Some((ordinary_code, ordinary_stderr)) = common::kotlinc_compile(&ordinary_args) else {
        eprintln!("skipping kotlin_internal_exact contract: kotlinc server unavailable");
        return;
    };
    assert_eq!(ordinary_code, 0, "{ordinary_stderr}");

    let exact = root.join("Exact.kt");
    fs::write(
        &exact,
        concat!(
            "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n",
            "fun <T> exact(value: @kotlin.internal.Exact T) {}\n",
            "fun use() = exact<CharSequence>(\"x\")",
        ),
    )
    .unwrap();
    let exact_args = vec![
        exact.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("exact-out").to_string_lossy().into_owned(),
    ];
    let Some((exact_code, exact_stderr)) = common::kotlinc_compile(&exact_args) else {
        eprintln!("skipping kotlin_internal_exact contract: kotlinc server unavailable");
        return;
    };
    assert_ne!(
        exact_code, 0,
        "@Exact unexpectedly accepted the widened type"
    );
    assert!(
        exact_stderr.contains("argument type mismatch"),
        "unexpected kotlinc diagnostic: {exact_stderr}"
    );

    let krusty = common::krusty_binary();
    let krusty_output = Command::new(krusty)
        .args(["-d", root.join("krusty-out").to_str().unwrap()])
        .arg(&exact)
        .output()
        .unwrap();
    let krusty_error = first_error(String::from_utf8_lossy(&krusty_output.stderr).as_ref())
        .or_else(|| first_error(&String::from_utf8_lossy(&krusty_output.stdout)));
    let _ = fs::remove_dir_all(&root);
    assert_eq!(krusty_error, first_error(&exact_stderr));
}

#[test]
fn kotlin_internal_exact_can_require_two_arguments_to_have_the_same_type() {
    let root = std::env::temp_dir().join(format!(
        "krusty_kotlin_internal_exact_pair_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let source = root.join("ExactPair.kt");
    fs::write(
        &source,
        concat!(
            "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n",
            "fun <T> same(first: @kotlin.internal.Exact T, second: @kotlin.internal.Exact T) {}\n",
            "fun use(first: String, second: CharSequence) = same(first, second)",
        ),
    )
    .unwrap();
    let args = vec![
        source.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("out").to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skipping kotlin_internal_exact pair contract: kotlinc server unavailable");
        return;
    };
    let _ = fs::remove_dir_all(&root);
    assert_ne!(code, 0, "two @Exact parameters accepted different types");
    assert!(
        stderr.contains("argument type mismatch"),
        "unexpected kotlinc diagnostic: {stderr}"
    );
}

#[test]
fn resolved_but_unsupported_is_as_shapes_are_not_called_unresolved() {
    // `resolve_ty` also returns `Ty::Error` for a KNOWN classifier whose shape the backend cannot
    // implement. That is distinct from an absent classifier: the unresolved-reference helper must
    // decline these so the existing supported-shape diagnostic remains authoritative.
    for source in [
        "fun f(p: Any) = p is Array",
        "fun f(p: Any) = p is Array<Nothing>",
        "fun f(p: Any) = p as Array",
        "fun f(p: Any) = p as Array<Nothing>",
    ] {
        let diagnostics = common::front_end_diagnostics(source, &[], None);
        assert!(
            diagnostics
                .iter()
                .all(|message| !message.contains("unresolved reference")),
            "a resolved or precedence-suppressed shape was mislabeled unresolved for \
             {source:?}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("not supported")),
            "the resolved unsupported-shape diagnostic disappeared for {source:?}: {diagnostics:?}"
        );
    }
}

#[test]
fn cross_file_generic_diagnostic_matches_kotlinc() {
    let krusty = common::krusty_binary();
    let root = std::env::temp_dir().join(format!("krusty_cross_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let declaration = root.join("declaration.kt");
    let use_site = root.join("use.kt");
    fs::write(&declaration, "fun <T> id(x: T): T = x").unwrap();
    fs::write(&use_site, "fun use(): Int = id(1, 2)").unwrap();

    let krusty_output = Command::new(&krusty)
        .args(["-d", root.join("out").to_str().unwrap()])
        .arg(&declaration)
        .arg(&use_site)
        .output()
        .unwrap();
    let krusty_error = first_error(String::from_utf8_lossy(&krusty_output.stderr).as_ref())
        .or_else(|| first_error(&String::from_utf8_lossy(&krusty_output.stdout)));

    let kotlinc_args = vec![
        declaration.to_string_lossy().into_owned(),
        use_site.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("kotlinc-out").to_string_lossy().into_owned(),
    ];
    let Some((_, kotlinc_stderr)) = common::kotlinc_compile(&kotlinc_args) else {
        eprintln!("skipping cross-file diagnostics parity: kotlinc server unavailable");
        return;
    };
    let kotlinc_error = first_error(&kotlinc_stderr);
    let _ = fs::remove_dir_all(&root);

    assert_eq!(krusty_error, kotlinc_error);
}
