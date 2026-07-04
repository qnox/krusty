//! Non-capturing lambdas `{ a -> … }` passed to a function-typed parameter, lowered to
//! `invokedynamic` + `LambdaMetafactory` producing a `kotlin/jvm/functions/FunctionN`, then invoked
//! through `FunctionN.invoke`. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn lambdas_run() {
    let src = "fun call1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun call0(f: () -> Int): Int = f()\n\
fun call2(f: (Int, Int) -> Int): Int = f(20, 22)\n\
fun box(): String {\n\
if (call1({ n -> n + 1 }, 41) != 42) return \"f1\"\n\
if (call0({ 7 }) != 7) return \"f2\"\n\
if (call1({ it * 2 }, 41) != 82) return \"f3\"\n\
if (call2({ a, b -> a + b }) != 42) return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "L");
}
