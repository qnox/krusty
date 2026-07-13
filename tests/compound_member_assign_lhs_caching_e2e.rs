//! A compound assignment to a member (`recv.a += v`) must evaluate the receiver EXACTLY ONCE, even
//! when the receiver is a side-effecting property getter or a function call. The parser desugars
//! `recv.a += v` to `recv.a = recv.a op v`, reusing the receiver expr; the lowerer spills a non-pure
//! receiver so the store and the embedded read share one evaluation (kotlinc's LHS-caching semantics).
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn property_getter_receiver_evaluated_once() {
    const SRC: &str = "class Box(var n: Int)\n\
        var log = \"\"\n\
        val prop: Box get() { log += \"g;\"; return holder }\n\
        val holder = Box(1)\n\
        fun box(): String {\n\
        \x20 prop.n += 10\n\
        \x20 return if (holder.n == 11 && log == \"g;\") \"OK\" else \"fail:\" + log + holder.n\n\
        }\n";
    assert_eq!(run(SRC).expect("getter receiver once"), "OK");
}

#[test]
fn function_result_receiver_evaluated_once() {
    const SRC: &str = "class Box(var n: Int)\n\
        var log = \"\"\n\
        val holder = Box(1)\n\
        fun get(): Box { log += \"c;\"; return holder }\n\
        fun box(): String {\n\
        \x20 get().n += 5\n\
        \x20 return if (holder.n == 6 && log == \"c;\") \"OK\" else \"fail:\" + log + holder.n\n\
        }\n";
    assert_eq!(run(SRC).expect("call receiver once"), "OK");
}
