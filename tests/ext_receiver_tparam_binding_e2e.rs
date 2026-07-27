use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn lambda_param_binds_from_receiver_type_args() {
    const SRC: &str = "class Box<T>(val head: T)\n\
fun <T> Box<T>.mapHead(f: (T) -> T): Box<T> = Box(f(head))\n\
fun box(): String {\n\
    val a: Int = Box(1).mapHead { it * 2 }.head\n\
    return if (a == 2) \"OK\" else \"fail: $a\"\n\
}\n";
    assert_eq!(run(SRC).expect("receiver-bound lambda param"), "OK");
}

#[test]
fn plain_extensions_unaffected() {
    const SRC: &str = "class C(val v: Int)\n\
fun C.twice(f: (Int) -> Int): Int = f(f(v))\n\
fun box(): String = if (C(10).twice { it + 1 } == 12) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("plain ext"), "OK");
}

#[test]
fn same_named_extension_on_other_class_does_not_bind() {
    const SRC: &str = "class Wrap<T>(val w: T)\n\
class Box<T>(val head: T)\n\
fun <T> Wrap<T>.mapHead(f: (T) -> T): Wrap<T> = Wrap(f(w))\n\
fun <T> Box<T>.mapHead(f: (T) -> T): Box<T> = Box(f(head))\n\
fun box(): String {\n\
    val a: Int = Box(20).mapHead { it + 1 }.head\n\
    val b: String = Wrap(\"O\").mapHead { it + \"K\" }.w\n\
    return if (a == 21 && b == \"OK\") \"OK\" else \"fail: $a $b\"\n\
}\n";
    assert_eq!(run(SRC).expect("overload disambiguation"), "OK");
}
