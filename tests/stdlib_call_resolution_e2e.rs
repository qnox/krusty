//! Regression guard: top-level kotlin-stdlib calls (`mutableListOf`, `listOf`, `emptyList`) and
//! `kotlin.test.assertEquals` must RESOLVE against the classpath the shared `krusty::toolchain`
//! builds — the exact classpath the conformance gate and the box-corpus `survey` use.
//!
//! These once dominated the survey's "unresolved function" buckets (`mutableListOf` was #1 at 406
//! files) NOT because krusty couldn't resolve them, but because the survey reimplemented jar location
//! and silently dropped the core `kotlin-stdlib.jar`/`kotlin-test.jar` from the classpath. Now that
//! the survey reuses `toolchain::classpath_jars_for`, a missing core jar (or a future drift) fails
//! here instead of masquerading as a krusty capability gap in the survey output.

use super::common;

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::symbol_source::SymbolSource;

/// Compile-check `src` against the shared toolchain classpath; return any error diagnostics' messages.
/// `None` means the toolchain (stdlib jar / JDK modules) isn't available — the caller should skip.
fn resolve_errors(src: &str) -> Option<Vec<String>> {
    // The shared classpath the gate/survey use: stdlib family per directives + JDK `lib/modules`.
    let mut cp_paths = common::classpath_jars_for(src);
    let jdk = common::jdk_modules();
    // No stdlib jar located → can't meaningfully assert resolution; signal skip.
    if cp_paths.is_empty() {
        return None;
    }
    cp_paths.push(jdk);

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    let _ = check_file(&files[0], &mut syms, &mut diags);
    Some(
        diags
            .diags
            .iter()
            .filter(|d| d.severity == krusty::diag::Severity::Error)
            .map(|d| d.msg.clone())
            .collect(),
    )
}

fn assert_resolves(label: &str, src: &str) {
    match resolve_errors(src) {
        None => {
            eprintln!(
                "skip {label}: no stdlib jar / JDK modules (run `just kotlinc`; set JAVA_HOME)"
            )
        }
        Some(errs) => assert!(
            errs.is_empty(),
            "{label}: expected clean resolution, got diagnostics: {errs:?}"
        ),
    }
}

#[test]
fn mutable_list_of_resolves() {
    assert_resolves(
        "mutableListOf(varargs)",
        "// WITH_STDLIB\nfun box(): String { val l = mutableListOf(1, 2, 3); l.add(4); return if (l.size == 4) \"OK\" else \"F\" }",
    );
    assert_resolves(
        "mutableListOf<T>() empty",
        "// WITH_STDLIB\nfun box(): String { val l = mutableListOf<Int>(); l.add(1); return \"OK\" }",
    );
}

#[test]
fn list_of_and_empty_list_resolve() {
    assert_resolves(
        "listOf(varargs)",
        "// WITH_STDLIB\nfun box(): String { val l = listOf(1, 2, 3); return if (l.size == 3) \"OK\" else \"F\" }",
    );
    assert_resolves(
        "emptyList<T>()",
        "// WITH_STDLIB\nfun box(): String { val l = emptyList<Int>(); return if (l.isEmpty()) \"OK\" else \"F\" }",
    );
}

#[test]
fn kotlin_test_assert_equals_resolves() {
    assert_resolves(
        "assertEquals",
        "// WITH_STDLIB\nimport kotlin.test.assertEquals\nfun box(): String { assertEquals(2, 1 + 1); return \"OK\" }",
    );
}

#[test]
fn kotlin_test_assert_fails_with_resolves() {
    assert_resolves(
        "assertFailsWith",
        "// WITH_STDLIB\nimport kotlin.test.assertFailsWith\nfun box(): String { assertFailsWith<IllegalArgumentException> { throw IllegalArgumentException() }; return \"OK\" }",
    );
}

#[test]
fn receiver_scope_function_accepts_function_value_argument() {
    assert_resolves(
        "Buildee.apply(instructions)",
        "// WITH_STDLIB\n\
class Buildee<T> { fun yield(arg: T) {} }\n\
fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>().apply(instructions)\n\
fun box(): String { build<String> { yield(\"OK\") }; return \"OK\" }\n",
    );
}

#[test]
fn sequence_builder_exposes_its_receiver_members() {
    assert_resolves(
        "sequence builder receiver",
        "// WITH_STDLIB\nfun box(): String { val xs = sequence<Int> { yield(1) }; return \"OK\" }",
    );
}

#[test]
fn kotlin_test_assert_fails_with_default_is_inline_only_callable() {
    let mut cp_paths =
        common::classpath_jars_for("// WITH_STDLIB\nimport kotlin.test.assertFailsWith\n");
    let jdk = common::jdk_modules();
    assert!(
        !cp_paths.is_empty(),
        "kotlin-test classpath unavailable for assertFailsWith metadata test"
    );
    cp_paths.push(jdk);
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let platform = krusty::jvm::jvm_libraries::JvmLibraries::new(cp);
    let overloads = match platform
        .symbols(
            krusty::symbol_source::SymbolNamespace::Package(krusty::types::type_name(
                "kotlin/test",
            )),
            "assertFailsWith$default",
        )
        .callables
        .clone()
    {
        krusty::libraries::Callables::Functions(f) => f.overloads,
        _ => Vec::new(),
    };
    let overload = overloads
        .iter()
        .find(|o| o.callable.params.len() == 2)
        .unwrap_or_else(|| {
            panic!(
                "expected assertFailsWith$default overload, got {} overload(s)",
                overloads.len()
            )
        });
    assert!(
        overload.flags.inline.must_inline(),
        "assertFailsWith$default must be exposed as splice-only inline"
    );
    assert_eq!(overload.call_sig.param_defaults, vec![true, false]);
}

#[test]
fn enum_reflection_functions_are_published_from_stdlib_metadata() {
    let mut cp_paths = common::classpath_jars_for("// WITH_STDLIB\n");
    assert!(!cp_paths.is_empty(), "stdlib classpath unavailable");
    cp_paths.push(common::jdk_modules());
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let platform = krusty::jvm::jvm_libraries::JvmLibraries::new(cp);

    for (name, arity) in [("enumValues", 0), ("enumValueOf", 1)] {
        let overloads = match platform
            .symbols(
                krusty::symbol_source::SymbolNamespace::Package(krusty::types::type_name("kotlin")),
                name,
            )
            .callables
            .clone()
        {
            krusty::libraries::Callables::Functions(functions) => functions.overloads,
            _ => Vec::new(),
        };
        assert!(
            overloads
                .iter()
                .any(|function| function.callable.params.len() == arity),
            "stdlib metadata must publish kotlin/{name}; got {} overloads",
            overloads.len(),
        );
    }
}

#[test]
fn enum_values_is_selected_as_the_stdlib_top_level_callable() {
    let src = "// WITH_STDLIB\nenum class E { A }\nfun use() = enumValues<E>()\n";
    let (errors, selected) =
        common::inspect_checker_with_stdlib(src, |file, info, _| {
            file.expr_arena.iter().enumerate().any(|(index, expression)| {
            let krusty::ast::Expr::Call { callee, .. } = expression else {
                return false;
            };
            matches!(file.expr(*callee), krusty::ast::Expr::Name(name) if name == "enumValues")
                && info
                    .resolved_top_level_call(krusty::ast::ExprId(index as u32))
                    .is_some_and(|target| {
                        target.callable.name == "enumValues"
                            && target.callable.compiler_intrinsic
                                == Some(krusty::libraries::CompilerIntrinsic::EnumValues)
                    })
        })
        });
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        selected,
        "enumValues must first resolve to its stdlib declaration"
    );
}

#[test]
fn string_builder_append_line_resolves() {
    assert_resolves(
        "StringBuilder.appendLine",
        "// WITH_STDLIB\nfun box(): String { val sb = StringBuilder(); sb.appendLine(\"OK\"); return sb.toString().trim() }",
    );
}
