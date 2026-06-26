//! Receiver function-type parameters `fun build(f: B.() -> Unit)` and a trailing receiver-lambda
//! `build { member = v }`: the lambda body sees `this` = the receiver (unqualified member access, like
//! `apply`), but — unlike the inlined `apply`/`run` — the lambda is emitted as a real `Function1` whose
//! first parameter is the receiver, then invoked via `f(b)`. This is the general builder-DSL surface (the
//! foundation the `Json { … }` config builder needs). Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "B", &[sl], Some(&jdk))
}

#[test]
fn receiver_lambda_member_assignment() {
    // `mk { flag = true }`: the body assigns the receiver's `var` through `this`; `mk` invokes the
    // `Function1` with the fresh `B` as receiver.
    const SRC: &str = "class B { var flag = false }\n\
fun mk(f: B.() -> Unit): B { val b = B(); f(b); return b }\n\
fun box(): String = if (mk { flag = true }.flag) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("receiver-lambda member assignment compiles + runs"),
        "OK"
    );
}

#[test]
fn receiver_lambda_member_call() {
    // The body calls a member function of the receiver (unqualified) — resolved via `this`.
    const SRC: &str = "class B { var n = 0; fun bump() { n = n + 1 } }\n\
fun mk(f: B.() -> Unit): B { val b = B(); f(b); return b }\n\
fun box(): String { val b = mk { bump(); bump() }; return if (b.n == 2) \"OK\" else \"n=${b.n}\" }\n";
    assert_eq!(
        run(SRC).expect("receiver-lambda member call compiles + runs"),
        "OK"
    );
}
