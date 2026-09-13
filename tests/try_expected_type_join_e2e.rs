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
    let classes = common::compile_in_process_files(&[("TryJoin", source)], &classpath, None)
        .expect("krusty compiles a try-expression whose branches need the expected type");
    assert!(
        classes.iter().any(|(name, _)| name == "demo/Api"),
        "expected demo/Api among {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}
