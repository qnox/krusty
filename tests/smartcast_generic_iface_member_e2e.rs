//! A member read (synthetic property or method) on a LOCAL `var` receiver smart-cast by an
//! `is`-check to a generic interface — the intellij `ActionUtil.kt` shape
//! (`while (delegate is ActionWithDelegate<*>) { delegate = delegate.delegate }` against a Java
//! `interface ActionWithDelegate<T> { T getDelegate(); }`). The `is`-narrowing used to apply only
//! to STABLE roots (val/parameter/`this`); a local `var` was dropped by the stability gate, so the
//! read fell back to the declared type (`Any`) and reported `unresolved reference 'delegate'.`
//! even though kotlinc smart-casts a local var no closure writes. Runs on the JVM.
use super::common;

/// `common::javac_compile` output: the jar path plus the compiled class bytes.
type JavacFixtures = (std::path::PathBuf, Vec<(String, Vec<u8>)>);

fn with_delegate_fixtures() -> Option<JavacFixtures> {
    let java = [
        (
            "WithDelegate.java".into(),
            r#"
                package fixtures;
                public interface WithDelegate<T> {
                    T getDelegate();
                }
            "#
            .into(),
        ),
        (
            "Wrapper.java".into(),
            r#"
                package fixtures;
                public final class Wrapper implements WithDelegate<Object> {
                    private final Object delegate;
                    public Wrapper(Object delegate) { this.delegate = delegate; }
                    public Object getDelegate() { return delegate; }
                }
            "#
            .into(),
        ),
        (
            "StringWrapper.java".into(),
            r#"
                package fixtures;
                public final class StringWrapper implements WithDelegate<String> {
                    private final String delegate;
                    public StringWrapper(String delegate) { this.delegate = delegate; }
                    public String getDelegate() { return delegate; }
                }
            "#
            .into(),
        ),
    ];
    common::javac_compile(&java, &[])
}

#[test]
fn var_smartcast_unwraps_java_generic_interface_chain() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some((library, _)) = with_delegate_fixtures() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.WithDelegate
        import fixtures.Wrapper

        fun unwrap(action: Any): Any {
            var d: Any = action
            while (d is WithDelegate<*>) {
                d = d.delegate
            }
            return d
        }

        fun box(): String {
            val root = unwrap(Wrapper(Wrapper("OK")))
            if (root != "OK") return "chain: $root"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn var_smartcast_calls_synthetic_method_form() {
    // The METHOD spelling of the same member (`d.getDelegate()` instead of the synthetic property
    // read `d.delegate`) resolves through the same narrowed receiver — pin both.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some((library, _)) = with_delegate_fixtures() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.WithDelegate
        import fixtures.Wrapper

        fun unwrap(action: Any): Any {
            var d: Any = action
            while (d is WithDelegate<*>) {
                d = d.getDelegate()
            }
            return d
        }

        fun box(): String {
            val root = unwrap(Wrapper(Wrapper("OK")))
            if (root != "OK") return "chain: $root"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn var_smartcast_to_concrete_subtype_binds_type_argument() {
    // Smart-cast to a CLASS that implements the generic interface with a CONCRETE type argument
    // (`StringWrapper implements WithDelegate<String>`): the synthetic property instantiates to
    // `String`, so `d.delegate.length` resolves on the narrowed receiver.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some((library, _)) = with_delegate_fixtures() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.StringWrapper

        fun labelLength(action: Any): Int {
            var d: Any = action
            if (d is StringWrapper) {
                return d.delegate.length
            }
            return -1
        }

        fun box(): String {
            if (labelLength(StringWrapper("OK")) != 2) return "length"
            if (labelLength("x") != -1) return "guard"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn var_smartcast_kotlin_generic_interface_member() {
    // Same shape against a KOTLIN-declared generic interface (no classpath/Java interop): a var
    // smart-cast to `KChain<*>` must offer the interface's members too.
    let source = r#"
        interface KChain<T> {
            val next: T
        }

        class KNode(override val next: Any) : KChain<Any>

        fun unwrap(action: Any): Any {
            var d: Any = action
            while (d is KChain<*>) {
                d = d.next
            }
            return d
        }

        fun box(): String {
            val root = unwrap(KNode(KNode("OK")))
            if (root != "OK") return "chain: $root"
            return "OK"
        }
    "#;
    let output = common::compile_and_run_with_stdlib(source, "Main").unwrap_or_else(|| {
        panic!(
            "{:?}",
            common::front_end_diagnostics(source, &[common::stdlib_jar()], None)
        )
    });
    assert_eq!(output.trim(), "OK");
}

#[test]
fn var_smartcast_unknown_member_still_unresolved() {
    // Negative pin: a member the narrowed type does NOT declare still reports the existing
    // `unresolved reference` message — the narrowing only widens the receiver to the proven type.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some((library, _)) = with_delegate_fixtures() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.WithDelegate

        fun bad(action: Any): Any {
            var d: Any = action
            if (d is WithDelegate<*>) {
                return d.notAMember
            }
            return d
        }
    "#;
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'notAMember'")),
        "an unknown member on the smart-cast var must stay unresolved, got {diagnostics:?}"
    );
}

/// stdlib-only front-end diagnostics for the soundness pins below.
fn stdlib_diagnostics(source: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(source, &[common::stdlib_jar()], Some(jdk.as_path()))
}

#[test]
fn var_smartcast_does_not_leak_into_lambda() {
    // kotlinc: "smart cast is impossible, because 'a' is a local variable that is mutated in a
    // capturing closure". The lambda can run after `a = 1`, so the cast must not flow into it —
    // krusty previously compiled this and crashed with a ClassCastException at `f()`.
    let diagnostics = stdlib_diagnostics(
        r#"
        fun box(): Int {
            var a: Any = "hi"
            if (a is String) {
                val f = { a.length }
                a = 1
                return f()
            }
            return 0
        }
    "#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'length'")),
        "a var smart-cast must not flow into a lambda, got {diagnostics:?}"
    );
}

#[test]
fn var_smartcast_does_not_leak_into_while_condition() {
    // The condition re-executes every iteration, after the body may have written the var — an
    // outer smart-cast must not flow into it (kotlinc reports `a.length` unresolved). krusty
    // previously typed the condition with the cast and crashed on the second iteration.
    let diagnostics = stdlib_diagnostics(
        r#"
        fun box(): Int {
            var a: Any = "hi"
            if (a is String) {
                while (a.length > 0) {
                    a = 1
                }
            }
            return 0
        }
    "#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'length'")),
        "a var smart-cast must not flow into a while condition, got {diagnostics:?}"
    );
}

#[test]
fn var_smartcast_blocked_by_local_function_write() {
    // A write inside a LOCAL FUNCTION is a closure write too (kotlinc: "mutated in a capturing
    // closure") — the var must never smart-cast while such a function can run.
    let diagnostics = stdlib_diagnostics(
        r#"
        fun box(): Int {
            var a: Any = "hi"
            fun g() { a = 1 }
            if (a is String) {
                return a.length
            }
            g()
            return 0
        }
    "#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'length'")),
        "a local-function write must block the var smart-cast, got {diagnostics:?}"
    );
}
