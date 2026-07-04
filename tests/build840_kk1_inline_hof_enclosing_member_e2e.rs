//! build.840 kk1: an inline higher-order function (`find`/`filter`/`any`/…) whose lambda calls a METHOD
//! of the ENCLOSING class (`class H { fun f(es) = es.find { same(it.v, 3) }; fun same(a, b) = … }`).
//! The lambda is inline-spliced, and krusty cleared `cur_class` for a spliced lambda's body — so the
//! bare enclosing-member call `same(…)` failed to resolve and the file bailed with "this construct is
//! not yet supported by the IR backend". The lambda now captures the enclosing `this` in the
//! inline-splice case too (the splicer remaps it, like any captured local, to the enclosing slot 0), so
//! the member call resolves and lowers. `forEach { member() }` already worked (a `for`-loop desugar).
use super::common;

const LIB: &str = "package lib\ndata class E(val v: Int)\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn find_lambda_calls_enclosing_member() {
    const MAIN: &str = "import lib.*\n\
        class H {\n\
        \x20 fun f(es: List<E>): E? = es.find { same(it.v, 3) }\n\
        \x20 fun same(a: Int, b: Int) = a == b\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = H().f(listOf(E(1), E(3), E(5)))\n\
        \x20 return if (r?.v == 3) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("kk1_find", MAIN).expect("find with enclosing-member lambda"),
        "OK"
    );
}

#[test]
fn filter_lambda_calls_enclosing_member() {
    const MAIN: &str = "import lib.*\n\
        class H {\n\
        \x20 fun f(es: List<E>): List<E> = es.filter { keep(it.v) }\n\
        \x20 fun keep(x: Int) = x > 2\n\
        }\n\
        fun box(): String = if (H().f(listOf(E(1), E(3), E(5))).size == 2) \"OK\" else \"F\"\n";
    assert_eq!(
        run("kk1_filter", MAIN).expect("filter with enclosing-member lambda"),
        "OK"
    );
}

#[test]
fn any_lambda_calls_enclosing_member() {
    const MAIN: &str = "import lib.*\n\
        class H {\n\
        \x20 fun f(es: List<E>): Boolean = es.any { ok(it.v) }\n\
        \x20 fun ok(x: Int) = x == 3\n\
        }\n\
        fun box(): String = if (H().f(listOf(E(1), E(3)))) \"OK\" else \"F\"\n";
    assert_eq!(
        run("kk1_any", MAIN).expect("any with enclosing-member lambda"),
        "OK"
    );
}

#[test]
fn lambda_reads_enclosing_field_and_member() {
    // The lambda both READS an enclosing property and CALLS an enclosing method through the captured `this`.
    const MAIN: &str = "import lib.*\n\
        class H(val threshold: Int) {\n\
        \x20 fun f(es: List<E>): E? = es.find { it.v > threshold && ok(it.v) }\n\
        \x20 fun ok(x: Int) = x % 2 == 1\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = H(2).f(listOf(E(1), E(4), E(5)))\n\
        \x20 return if (r?.v == 5) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("kk1_field", MAIN).expect("lambda reads enclosing field + member"),
        "OK"
    );
}
