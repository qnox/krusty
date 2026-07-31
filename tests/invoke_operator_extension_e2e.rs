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
        open class Spec(val init: SpecScope.() -> Unit)
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
    assert_eq!(run(SRC).expect("member extension invoke"), "OK");
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
