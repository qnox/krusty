//! The generic `Signature` of a lifted local function. The lifted method declares a copy of each
//! type parameter its closure captures from the declarations around it, in the order the closure
//! first sees them, ahead of its own: `fun <T, S> outer() { fun <U> f(a: U, b: T) }` lifts to
//! `outer$f` signed `<T:Ljava/lang/Object;U:Ljava/lang/Object;>(TU;TT;)I`. An anonymous function
//! is signed the same way; a lambda literal's method carries no `Signature`.

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

#[test]
fn local_function_signatures_run() {
    common::expect_box_ok_with_stdlib(LOCALS, "Locals");
}

#[test]
fn local_functions_sign_as_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(LOCALS, "Locals");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("Locals.kt");
    std::fs::write(&path, LOCALS).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in CLASSES {
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
