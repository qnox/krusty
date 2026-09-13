use super::common;
use std::path::PathBuf;

fn library() -> Option<PathBuf> {
    let java = vec![
        (
            "Resp.java".into(),
            "package jl;\n\
             public interface Resp<B> { B body(); }\n"
                .into(),
        ),
        (
            "MutableResp.java".into(),
            "package jl;\n\
             public interface MutableResp<B> extends Resp<B> {\n\
             \x20 <T> MutableResp<T> body(T value);\n\
             \x20 static <T> MutableResp<T> status(int code) { return null; }\n\
             \x20 static <T> MutableResp<T> noContent() { return null; }\n\
             }\n"
            .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

/// A `try` used as an expression joins its branches the way `if` and `when` do — against the EXPECTED
/// type. Without it the branches of
/// `try { MutableResp.noContent() } catch (…) { MutableResp.status<Any>(403).body(…) }` joined to
/// `MutableResp<out Any!>`, which is not assignable to the declared `Resp<Any>`: an invariant
/// parameter cannot take an out-projection. kotlinc accepts the same source, so the whole file (and
/// with it its module) was rejected over a join that never consulted the expectation.
#[test]
fn a_try_expression_joins_its_branches_against_the_expected_type() {
    let Some(library) = library() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let source = "package demo\n\
        import jl.MutableResp\n\
        import jl.Resp\n\
        class Api {\n\
        \x20 fun revoke(flag: Boolean): Resp<Any> =\n\
        \x20\x20 try {\n\
        \x20\x20\x20 if (flag) throw IllegalStateException()\n\
        \x20\x20\x20 MutableResp.noContent()\n\
        \x20\x20 } catch (_: IllegalStateException) {\n\
        \x20\x20\x20 MutableResp.status<Any>(403).body(\"denied\")\n\
        \x20\x20 }\n\
        }\n";
    let classpath = vec![library, common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("TryJoin.kt", source)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc accepts the classpath try-join shape"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty accepts exactly the source set kotlinc accepts"
    );
}

/// The expected join is shared by every value-producing catch, while an initializer without an
/// annotation and a statement-position `try` retain their ordinary inference rules.
#[test]
fn expected_multi_catch_and_unexpected_controls_match_kotlinc() {
    let source = "class Box<T>\n\
        fun <T> boxed(value: T): Box<T> = Box()\n\
        fun expected(flag: Boolean): Box<Any> = try {\n\
        \x20 if (flag) throw IllegalArgumentException()\n\
        \x20 boxed(1)\n\
        } catch (_: IllegalArgumentException) {\n\
        \x20 boxed(\"text\")\n\
        } catch (_: RuntimeException) {\n\
        \x20 boxed(false)\n\
        }\n\
        fun inferred(flag: Boolean) {\n\
        \x20 val value = try {\n\
        \x20\x20 if (flag) throw RuntimeException()\n\
        \x20\x20 boxed(1)\n\
        \x20 } catch (_: RuntimeException) { boxed(\"text\") }\n\
        \x20 println(value)\n\
        }\n\
        fun number(): Int = 1\n\
        fun text(): String = \"recovered\"\n\
        fun statement(flag: Boolean) {\n\
        \x20 try {\n\
        \x20\x20 if (flag) throw RuntimeException()\n\
        \x20\x20 number()\n\
        \x20 } catch (_: RuntimeException) { text() }\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Controls.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc accepts the conditional-result controls"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty preserves no-expectation and statement-position behavior"
    );
}

#[test]
fn an_incompatible_expected_type_rejects_the_try_result_exactly_once() {
    let source = "class Box<T>\n\
        fun <T> boxed(value: T): Box<T> = Box()\n\
        fun bad(): Box<String> = try {\n\
        \x20 boxed(1)\n\
        } catch (_: RuntimeException) {\n\
        \x20 boxed(\"text\")\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Bad.kt", source)], &[common::stdlib_jar()]);
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc names the rejected file");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:3:26: error: return type mismatch: expected 'Box<String>', actual \
                 'Box<out Comparable<*> & Serializable>'.\nfun bad(): Box<String> = try {{\n\
                 \x20                        ^^^^^\n"
            )
            .as_str()
        ),
    );
    let krusty_path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty names the rejected file");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{krusty_path}:3:26: error: return type mismatch: expected 'Box<String>', actual \
                 'Box<out Any>'.\nkrusty: 1 error(s)\n"
            )
            .as_str()
        ),
    );
}
