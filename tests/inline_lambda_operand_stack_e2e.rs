//! A classpath inline call taking a lambda splices under a non-empty operand stack.
//!
//! `Envelope(populate { … })` reaches the splice with `new Envelope; dup` already on the stack.
//! The relocated StackMapTable frames carry no operand prefix, so a splice that records frames has
//! to refuse a non-empty baseline — but the old test for "records frames" asked whether the lambda
//! had a BODY, not whether any body produced a frame. Every inline call with a lambda argument
//! answered yes and declined here. The fixtures deliberately use a repository-owned inline
//! function rather than a stdlib builder. It is `@InlineOnly`, so there is no callable fallback:
//! successful compilation and execution prove that the generic classpath splice happened.
use super::common;

const LIB: &str = r#"
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

class Token(val value: Int)

class Payload(
    var first: Token? = null,
    var second: Token? = null,
)

fun createPayload(): Payload = Payload()

fun finishPayload(payload: Payload): Payload = payload

@kotlin.internal.InlineOnly
inline fun populate(block: Payload.() -> Unit): Payload {
    val payload = createPayload()
    payload.block()
    return finishPayload(payload)
}
"#;

fn run_against_kotlinc_library(source: &str, stem: &str) -> String {
    let library = common::kotlinc_library(LIB).expect("reference compiler unavailable");
    let jdk = common::jdk_modules();
    common::expect_box_run(
        source,
        stem,
        &[library, common::stdlib_jar()],
        Some(jdk.as_path()),
    )
}

const BRANCHLESS_LAMBDA_IN_AN_ARGUMENT: &str = r#"
class Envelope(val payload: Payload)

fun make(left: Token, right: Token): Envelope =
    Envelope(
        populate {
            first = left
            second = right
        },
    )

fun box(): String {
    val first = Token(1)
    val second = Token(2)
    val payload = make(first, second).payload
    return if (payload.first === first && payload.second === second) "OK" else "FAIL"
}
"#;

/// The branchless lambda has no frame of its own, so the splice is free to land on the
/// constructor's uninitialized prefix. `populate` has no callable fallback, so the file is emitted
/// only if the user-defined inline body really splices; the JVM run also pins verifier correctness.
#[test]
fn a_branchless_inline_lambda_splices_under_a_constructor_prefix() {
    let output = run_against_kotlinc_library(
        BRANCHLESS_LAMBDA_IN_AN_ARGUMENT,
        "InlineLambdaOperandBranchless",
    );
    assert_eq!(output, "OK");
}

const BRANCHY_LAMBDA_IN_AN_ARGUMENT: &str = r#"
class Envelope(val payload: Payload)

fun make(flag: Boolean, token: Token): Envelope =
    Envelope(
        populate {
            if (flag) first = token
        },
    )

fun box(): String {
    val token = Token(1)
    val on = make(true, token).payload
    val off = make(false, token).payload
    return if (on.first === token && off.first == null) "OK" else "FAIL"
}
"#;

/// A lambda body that does record a frame must still make the operand sequence spill the
/// constructor prefix before the splice. The `@InlineOnly` callee again makes this fail closed.
#[test]
fn a_branchy_inline_lambda_still_reaches_an_empty_baseline() {
    let output =
        run_against_kotlinc_library(BRANCHY_LAMBDA_IN_AN_ARGUMENT, "InlineLambdaOperandBranchy");
    assert_eq!(output, "OK");
}
