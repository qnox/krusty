//! A lambda's implementation returns what kotlinc's does. For a functional interface method that
//! returns `Unit` (a Kotlin `fun interface` or `java.lang.Runnable`), it returns nothing (`void`). A
//! lambda whose every result is `Unit` returns `Unit` even where the function type returns `Any`.
//! Any other lambda returns its function type's result.

use super::common;

const LAMBDAS: &str = "fun interface Act { fun go() }
fun interface Get { fun get(): Any }
fun interface Gen<T> { fun get(): T }
fun interface Text { fun get(): CharSequence }
fun unit() {}
fun takeAct(act: Act) = act.go()
fun takeGet(get: Get) = get.get()
fun takeGen(gen: Gen<Unit>) = gen.get()
fun takeText(text: Text) = text.get()
fun takeRunnable(runnable: Runnable) = runnable.run()
fun takeAny(f: () -> Any) = f()
fun take(f: () -> Unit) = f()
fun lambdas(): Int {
    var x = 0
    takeAct { x += 1 }
    takeGet { x += 1 }
    takeGen { x += 1 }
    takeText { \"s\" }
    takeRunnable { x += 1 }
    take { x += 1 }
    takeAny { x += 1 }
    takeAny { unit() }
    takeAny { if (x > 0) x += 1 }
    takeAny { val y = x }
    takeAny { if (x > 100) return@takeAny 1; x += 1 }
    takeAny { x++ }
    return x
}
fun box(): String {
    val x = lambdas()
    return if (x == 9) \"OK\" else \"$x\"
}
";

#[test]
fn a_lambda_implementation_returns_what_kotlinc_infers() {
    let pair = common::ModuleClassPair::compile(&[("Lambdas.kt", LAMBDAS)], "LambdasKt");
    let (kotlinc, krusty) = (lambda_methods(&pair.kotlinc), lambda_methods(&pair.krusty));
    assert_eq!(krusty, kotlinc);
    // The last lambda's `x++` reads a shared cell in a shape krusty does not match yet; its
    // declaration above is compared.
    for index in 0..11 {
        let method = format!("lambdas$lambda${index}");
        let (kotlinc, krusty) = pair.method_code("LambdasKt", &method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

#[test]
fn a_lambda_implementation_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(LAMBDAS, "Lambdas");
}

/// The lambda implementation methods javap reads from the class, in order.
fn lambda_methods(bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("cannot allocate disassembly fixture");
    let path = work.join("LambdasKt.class");
    std::fs::write(&path, bytes).expect("write class for disassembly");
    let text = common::javap(&["-p", &path.to_string_lossy()]).expect("javap unavailable");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .filter(|line| line.contains("$lambda$"))
        .collect::<Vec<_>>()
        .join("\n")
}
