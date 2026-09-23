//! Joining a Java unbounded wildcard argument with a concrete one.
//!
//! `C<*>` is the supertype of every `C<X>`, so joining a star argument with anything yields the
//! star. krusty destructured the star into `(out, Any?)` and rejoined it as an ordinary `out`
//! projection, which says the argument is BOUNDED by `Any?` rather than unknown:
//!
//! ```text
//! kotlinc:  actual 'Pub<out Resp<*>!>!'
//! krusty:   actual 'Pub<out Resp<out Any!>!>!'
//! ```
//!
//! An invariant expectation written `Pub<Resp<*>>` rejects the second and accepts the first, which
//! is how this reached a corpus module: a filter declaring `Publisher<MutableHttpResponse<*>>`
//! could not return its own body.
//!
//! The OUTER `out` is kotlinc's too — a fix that removed it would be wrong, and one test pins it.
//!
//! The Java half is required: a wildcard is the only way to get a star-projected argument into an
//! invariant position from a declaration a Kotlin source cannot write.
use std::path::PathBuf;

use super::common;

fn library() -> Option<PathBuf> {
    let java = vec![
        ("Pub.java".into(), "public interface Pub<T> { }\n".into()),
        (
            "Mono.java".into(),
            "public interface Mono<T> extends Pub<T> { }\n".into(),
        ),
        ("Resp.java".into(), "public interface Resp<T> { }\n".into()),
        (
            "Payload.java".into(),
            "public final class Payload { }\n".into(),
        ),
        (
            "Src.java".into(),
            "public interface Src { Pub<Resp<?>> viaPub(); }\n".into(),
        ),
        (
            "Factory.java".into(),
            "public final class Factory {\n\
                 public static <T> Mono<T> just(T value) { return null; }\n\
                 public static Resp<Payload> resp() { return null; }\n\
             }\n"
            .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

fn diagnostics(src: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    let library = library().expect("javac must compile the wildcard fixture");
    common::front_end_diagnostics(src, &[library, common::stdlib_jar()], Some(jdk.as_path()))
}

/// A wildcard-typed arm joined with an arm of the same written argument. Both arms say `Resp<*>`,
/// but one said it through a Java wildcard, so the two arguments were different `Ty` constructors
/// and the join manufactured an `out` that the declared return then rejected.
#[test]
fn a_wildcard_arm_joins_with_an_arm_of_the_same_argument() {
    const SRC: &str = "fun pick(src: Src, flag: Boolean): Pub<Resp<*>> =\n\
        \x20   when (flag) {\n\
        \x20       true -> src.viaPub()\n\
        \x20       else -> Factory.just<Resp<*>>(Factory.resp())\n\
        \x20   }\n";
    assert_eq!(diagnostics(SRC), Vec::<String>::new());
}

/// The joined type itself, read off an initializer mismatch. kotlinc reports exactly
/// `Pub<out Resp<*>!>!` for this expression: the star survives the argument join, and the OUTER
/// `out` stays.
#[test]
fn the_join_keeps_the_star_and_the_outer_out() {
    const SRC: &str = "class JoinExpected\n\
        fun probe(src: Src, flag: Boolean) {\n\
        \x20   val x = when (flag) { true -> src.viaPub(); else -> Factory.just(Factory.resp()) }\n\
        \x20   val y: JoinExpected = x\n\
        }\n";
    assert_eq!(
        diagnostics(SRC),
        vec![
            "initializer type mismatch: expected 'JoinExpected', actual 'Pub<out Resp<*>!>!'."
                .to_string()
        ]
    );
}

/// The control: two arms with ORDINARY arguments still join to an `out` projection of their common
/// supertype, which is what both compilers do. The rule is "a star argument wins", not "every
/// invariant argument join becomes a star".
#[test]
fn two_ordinary_arguments_still_join_to_an_out_projection() {
    const SRC: &str = "class JoinLeft\n\
        class JoinRight\n\
        class JoinExpected\n\
        fun probe(a: Pub<Resp<JoinLeft>>, b: Mono<Resp<JoinRight>>, flag: Boolean) {\n\
        \x20   val x = when (flag) { true -> a; else -> b }\n\
        \x20   val y: JoinExpected = x\n\
        }\n";
    assert_eq!(
        diagnostics(SRC),
        vec![
            "initializer type mismatch: expected 'JoinExpected', actual 'Pub<out Resp<out Any>>'."
                .to_string()
        ]
    );
}
