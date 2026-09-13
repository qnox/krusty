//! A lambda SAM-converted to a JAVA functional interface is checked against the interface's
//! declared result.
//!
//! `Mono<T>.onErrorResume(Function<? super Throwable, ? extends Mono<? extends T>>)` is the shape:
//! the fallback lambda's body is a bare `Mono.empty()`, whose own type argument has no evidence
//! except the expected result the SAM supplies. krusty shaped the lambda's INPUTS from the
//! functional interface and dropped its result, so the body stayed `Mono<T>` with `T` unsolved and
//! the argument was reported as `(Throwable!) -> Mono<T>!` against `Function<Throwable!,
//! Mono<Auth>!>!`. Reactive chains are written this way throughout, and one such chain cost a
//! corpus module every class it would have emitted.

use super::common;

const JAVA: &[(&str, &str)] = &[
    ("Pub.java", "package pub;\npublic interface Pub<T> {}\n"),
    (
        "Mono.java",
        "package pub;\n\
         import java.util.function.Function;\n\
         import java.util.concurrent.Callable;\n\
         public final class Mono<T> implements Pub<T> {\n\
         \x20 public static <T> Mono<T> empty() { return new Mono<T>(); }\n\
         \x20 public static <T> Mono<T> just(T value) { return new Mono<T>(); }\n\
         \x20 public static <T> Mono<T> fromCallable(Callable<? extends T> c) { return new Mono<T>(); }\n\
         \x20 public Mono<T> onErrorExact(Function<Throwable, Mono<T>> f) { return this; }\n\
         \x20 public Mono<T> onErrorOuter(Function<Throwable, ? extends Mono<T>> f) { return this; }\n\
         \x20 public Mono<T> onErrorResume(Function<? super Throwable, ? extends Mono<? extends T>> f) {\n\
         \x20\x20 return this;\n\
         \x20 }\n\
         \x20 public <R> Mono<R> flatMap(Function<? super T, ? extends Mono<? extends R>> f) {\n\
         \x20\x20 return new Mono<R>();\n\
         \x20 }\n\
         }\n",
    ),
];

fn assert_both_accept(user: &str, what: &str) {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("Use.kt", user)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source kotlinc accepts for {what}"
    );
}

/// The plainest form: the SAM's result is exactly the generic the body must take.
#[test]
fn a_sam_lambdas_body_takes_the_interfaces_declared_result() {
    assert_both_accept(
        "import pub.Mono\n\
         class Auth(val n: String)\n\
         fun use(m: Mono<Auth>): Mono<Auth> = m.onErrorExact { Mono.empty() }\n",
        "an exact SAM result",
    );
}

/// The same with a wildcard on the SAM's own result.
#[test]
fn an_outer_wildcard_on_the_sam_result_still_contextualizes() {
    assert_both_accept(
        "import pub.Mono\n\
         class Auth(val n: String)\n\
         fun use(m: Mono<Auth>): Mono<Auth> = m.onErrorOuter { Mono.empty() }\n",
        "an outer-wildcard SAM result",
    );
}

/// Wildcards on both levels — the reactive signature as it is actually written.
#[test]
fn nested_wildcards_on_the_sam_result_still_contextualize() {
    assert_both_accept(
        "import pub.Mono\n\
         class Auth(val n: String)\n\
         fun use(m: Mono<Auth>): Mono<Auth> = m.onErrorResume { Mono.empty() }\n",
        "a doubly-wildcarded SAM result",
    );
}

/// The whole chain, which is the corpus shape: a `flatMap` that binds the element type followed by
/// a fallback whose body has no evidence of its own.
#[test]
fn a_reactive_chain_binds_its_element_type_through_to_the_fallback() {
    assert_both_accept(
        "import pub.Mono\n\
         import pub.Pub\n\
         class Auth(val n: String)\n\
         fun use(token: String, lookup: (String) -> String?): Pub<Auth> =\n\
         \x20 Mono\n\
         \x20\x20 .fromCallable { lookup(token) }\n\
         \x20\x20 .flatMap { found -> if (found != null) Mono.just(Auth(found)) else Mono.empty() }\n\
         \x20\x20 .onErrorResume { Mono.empty() }\n",
        "a reactive chain",
    );
}

/// Control: a body that genuinely disagrees with the declared result is still an error, so pushing
/// the expectation has not turned the body's own type into a formality.
#[test]
fn a_body_disagreeing_with_the_declared_result_is_still_rejected() {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import pub.Mono\n\
             class Auth(val n: String)\n\
             fun use(m: Mono<Auth>): Mono<Auth> = m.onErrorExact { \"not a Mono\" }\n",
        )],
        &classpath,
    );
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a body that is not the declared result"
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty must reject a body that is not the declared result"
    );
}
