//! An `operator fun Recv.invoke(...)` EXTENSION makes `recv(args)` call it (`"a"(12)` →
//! `invoke("a", 12)`). Lowered as `invokestatic <facade>.invoke(recv, args)`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn invoke_extension_on_string_literal() {
    const SRC: &str = "operator fun String.invoke(i: Int) = \"$this$i\"\n\
fun box() = if (\"a\"(12) == \"a12\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("String.invoke extension"), "OK");
}

#[test]
fn invoke_extension_on_user_type() {
    const SRC: &str = "class V(val n: Int)\n\
operator fun V.invoke(d: Int): Int = n + d\n\
fun box(): String {\n\
    val v = V(40)\n\
    return if (v(2) == 42) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("user-type invoke extension"), "OK");
}

#[test]
fn member_extension_invoke_in_super_ctor_receiver_lambda() {
    const SRC: &str = r#"
        class SpecScope {
            val seen = mutableListOf<String>()
            operator fun String.invoke(body: () -> Unit) {
                seen.add(this)
                body()
            }
        }
        open class Spec(val init: SpecScope.() -> Unit) {
            constructor(label: String) : this({})
        }
        class A : Spec({
            "test" {
            }
        })
        fun box(): String {
            val a = A()
            val scope = SpecScope()
            a.init(scope)
            return if (scope.seen == listOf("test")) "OK" else "fail"
        }
    "#;
    let diagnostics = common::checker_diags_with_stdlib(SRC).expect("checker prerequisites");
    assert!(
        diagnostics.is_empty(),
        "same-file receiver lambda diagnostics: {diagnostics:?}"
    );
    assert_eq!(run(SRC).expect("member extension invoke"), "OK");
}

#[test]
fn named_super_ctor_lambda_uses_its_mapped_parameter_type() {
    const SRC: &str = r#"
        class DslScope {
            operator fun String.invoke(body: () -> Unit) {
                body()
            }
        }
        open class DslBase(val marker: String, val init: DslScope.() -> Unit)
        class A : DslBase(
            init = {
                "case" {}
            },
            marker = "ready",
        )
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "named constructor mapping must type the lambda from its selected source slot: {diagnostics:?}"
    );
}

#[test]
fn sibling_file_super_ctor_receiver_lambda_uses_shared_frontend_resolution() {
    const DSL: &str = r#"
        package sample

        class DslScope {
            val seen = mutableListOf<String>()
            operator fun String.invoke(body: () -> Unit) {
                seen.add(this)
                body()
            }
        }
        open class DslBase(val init: DslScope.() -> Unit)
    "#;
    const MAIN: &str = r#"
        package sample

        class A : DslBase({
            "module" {}
        })
        fun box(): String {
            val value = A()
            val scope = DslScope()
            value.init(scope)
            return if (scope.seen == listOf("module")) "OK" else "fail"
        }
    "#;
    common::expect_front_end_ok_files_with_stdlib(&[DSL, MAIN], "sibling receiver lambda");
}

#[test]
fn classpath_super_ctor_receiver_lambda_uses_shared_resolution() {
    const LIB: &str = r#"
        package api

        class DslScope {
            val seen = mutableListOf<String>()
            operator fun String.invoke(body: () -> Unit) {
                seen.add(this)
                body()
            }
        }
        open class DslBase(val init: DslScope.() -> Unit)
    "#;
    const MAIN: &str = r#"
        import api.DslBase
        import api.DslScope

        class A : DslBase({
            "classpath" {}
        })
        fun box(): String {
            val value = A()
            val scope = DslScope()
            value.init(scope)
            return if (scope.seen == listOf("classpath")) "OK" else "fail"
        }
    "#;

    let Some(output) = common::run_box_against("invoke_classpath_super", LIB, MAIN) else {
        return;
    };
    assert_eq!(
        output, "OK",
        "classpath constructor resolution must use the same contextual lambda path"
    );
}

#[test]
fn secondary_super_delegation_receiver_lambda_uses_shared_resolution() {
    const SRC: &str = r#"
        class DslScope {
            val seen = mutableListOf<String>()
            operator fun String.invoke(body: () -> Unit) {
                seen.add(this)
                body()
            }
        }
        open class DslBase(val init: DslScope.() -> Unit)
        class A : DslBase {
            constructor() : super({
                "secondary" {}
            })
        }
        fun box(): String {
            val value = A()
            val scope = DslScope()
            value.init(scope)
            return if (scope.seen == listOf("secondary")) "OK" else "fail"
        }
    "#;

    assert_eq!(
        run(SRC).expect("secondary super receiver lambda"),
        "OK",
        "secondary and primary super delegation must share contextual lambda resolution"
    );
}

#[test]
fn member_extension_invoke_in_with_receiver_lambda() {
    const SRC: &str = r#"
        class SpecScope {
            val seen = mutableListOf<String>()
            operator fun String.invoke(body: () -> Unit) {
                seen.add(this)
                body()
            }
        }
        fun box(): String {
            val scope = SpecScope()
            with(scope) {
                "test" {
                }
            }
            return if (scope.seen == listOf("test")) "OK" else "fail"
        }
    "#;
    assert_eq!(run(SRC).expect("with-receiver invoke"), "OK");
}

#[test]
fn non_operator_member_extension_invoke_not_used_by_call_syntax() {
    const SRC: &str = r#"
        class SpecScope {
            fun String.invoke() {
            }
        }
        open class Spec(init: SpecScope.() -> Unit)
        class A : Spec({
            "test"()
        })
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("expression is not callable")),
        "expected 'expression is not callable', got: {diagnostics:?}"
    );
}

#[test]
fn non_operator_top_level_extension_invoke_not_used_by_call_syntax() {
    const SRC: &str = r#"
        fun String.invoke() {
        }
        fun box(): String {
            "not callable"()
            return "FAIL"
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("expression is not callable")),
        "a non-operator top-level invoke must remain explicit: {diagnostics:?}"
    );
}
