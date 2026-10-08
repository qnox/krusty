//! The generic `Signature` of a lifted local function. The lifted method declares a copy of each
//! type parameter its closure captures from the declarations around it, in the order the closure
//! first sees them, ahead of its own: `fun <T, S> outer() { fun <U> f(a: U, b: T) }` lifts to
//! `outer$f` signed `<T:Ljava/lang/Object;U:Ljava/lang/Object;>(TU;TT;)I`. An anonymous function
//! is signed the same way; a lambda literal's method carries no `Signature`.
//!
//! The captured values lead the lifted method's parameters in the order kotlinc's closure first
//! sees them: what the function's defaults and body read, in source order (a condition before the
//! branch the checker publishes first, including a shared `var` cell), a lambda's reads in place,
//! and then the captures of each local function it calls, never in the order of their names.

use super::common;

const LOCALS: &str = "class Cell<T>(val v: T)\n\
interface Ordered<V>\n\
class Rank(val n: Int) : Ordered<Rank>\n\
class Gen<A>(val a: A) {\n\
    fun read(): A {\n\
        fun get(): A = a\n\
        return get()\n\
    }\n\
}\n\
fun <T, S> outer(t: T, s: S): Int {\n\
    fun <U> mixed(a: U, b: T): Int = 1\n\
    fun bodyOnly(): Int { val c: Cell<S>? = null; return if (c == null) 1 else 2 }\n\
    fun <V : Ordered<V>> bounded(v: V): Int = 1\n\
    fun <U> nest(u: U): Int {\n\
        fun <W> inner(w: W, t2: T, u2: U): Int = 1\n\
        return inner(1, t, u)\n\
    }\n\
    fun retFirst(x: T): Cell<S>? = null\n\
    fun caller(): Int = bodyOnly()\n\
    fun T.ext(y: S): Int = 1\n\
    val literal = { c: Cell<T> -> 1 }\n\
    val anonymous = fun(c: Cell<T>): Int = 1\n\
    val plain = fun(c: Cell<String>): Int = 1\n\
    return mixed(1, t) + bodyOnly() + bounded(Rank(1)) + nest(2) +\n\
        (if (retFirst(t) == null) 1 else 0) + caller() + t.ext(s) +\n\
        literal(Cell(t)) + anonymous(Cell(t)) + plain(Cell(\"\"))\n\
}\n\
fun box(): String = if (outer(1, \"x\") == 10) Gen(\"OK\").read() else \"fail\"\n";

const CLASSES: [&str; 2] = ["LocalsKt", "Gen"];

const CAPTURES: &str = "class Holder<T>(val item: T)\n\
fun <T, S> order(beta: Holder<S>, alpha: Holder<T>, gamma: Int, delta: String): String {\n\
    fun readsBetaFirst(): String = \"\" + beta.item + alpha.item\n\
    fun first(): T = alpha.item\n\
    fun callsFirst(): String = \"\" + first() + beta.item\n\
    fun lambdaInPlace(): String {\n\
        val read = { \"\" + gamma + alpha.item }\n\
        return read() + beta.item\n\
    }\n\
    fun withDefault(p: String = delta): String = p + beta.item\n\
    fun last(): Int = gamma\n\
    fun calls(): String = delta + last() + first()\n\
    fun declaresFirst(): String {\n\
        fun inner(): T = alpha.item\n\
        return \"\" + beta.item + inner()\n\
    }\n\
    return readsBetaFirst() + callsFirst() + lambdaInPlace() + withDefault() + calls() +\n\
        declaresFirst()\n\
}\n\
fun box(): String =\n\
    if (order(Holder(\"b\"), Holder(\"a\"), 1, \"d\") == \"baab1abdbd1aba\") \"OK\" else \"fail\"\n";

#[test]
fn local_function_signatures_run() {
    common::expect_box_ok_with_stdlib(LOCALS, "Locals");
}

#[test]
fn local_functions_sign_as_kotlinc() {
    signs_as_kotlinc(LOCALS, "Locals", &CLASSES);
}

#[test]
fn captured_values_in_first_use_order_run() {
    common::expect_box_ok_with_stdlib(CAPTURES, "Captures");
}

#[test]
fn captured_values_lead_in_first_use_order_as_kotlinc() {
    signs_as_kotlinc(CAPTURES, "Captures", &["CapturesKt"]);
}

/// A shared `var` cell and a plain value, each used first in the condition and first in the
/// branch. kotlinc's closure sees the condition before the branch.
const SHARED_CELL: &str = "fun box(): String = if (parse() == \"xy\") \"OK\" else \"fail\"\n\
fun parse(): String {\n\
    val parsedArgs = mutableListOf<String>()\n\
    var current = StringBuilder()\n\
    fun save(wasQuoted: Boolean) {\n\
        if (wasQuoted || current.isNotBlank()) {\n\
            parsedArgs.add(current.toString())\n\
            current = StringBuilder()\n\
        }\n\
    }\n\
    fun listFirst() {\n\
        if (parsedArgs.isEmpty()) {\n\
            current.append('x')\n\
        }\n\
    }\n\
    fun branchWrites(flag: Boolean) {\n\
        if (flag) current.append('y')\n\
        parsedArgs.add(current.toString())\n\
    }\n\
    save(false)\n\
    listFirst()\n\
    branchWrites(true)\n\
    return current.toString()\n\
}\n";

#[test]
fn shared_var_capture_follows_source_order_at_runtime() {
    common::expect_box_ok_with_stdlib(SHARED_CELL, "SharedCell");
}

#[test]
fn shared_var_capture_leads_in_source_order_as_kotlinc() {
    signs_as_kotlinc(SHARED_CELL, "SharedCell", &["SharedCellKt"]);
}

/// Compare the methods of each of `classes` that krusty and kotlinc compile `source` to.
fn signs_as_kotlinc(source: &str, stem: &str, classes: &[&str]) {
    let krusty = common::expect_classes_with_stdlib(source, stem);
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join(format!("{stem}.kt"));
    std::fs::write(&path, source).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for &class in classes {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(methods(emitted), methods(&reference), "{class}: methods");
    }
}

/// Every method's name, descriptor and generic `Signature`, in classfile order.
fn methods(bytes: &[u8]) -> Vec<(String, String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
            )
        })
        .collect()
}
