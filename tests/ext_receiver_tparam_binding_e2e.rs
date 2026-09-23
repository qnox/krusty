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

/// A receiver declared as a supertype (`Collection<T>.plus`) is still a constraint on `T` when the
/// caller's receiver is a subtype carrying a caller type variable (`List<E>`). Before, the
/// structural-only receiver match left `T` open, the collection argument alone fixed it to
/// `List<E>`, and `plus(element: T)` then out-ranked `plus(elements: Iterable<T>)`.
#[test]
fn a_subtype_receiver_over_a_caller_type_variable_binds_the_declared_receiver_formal() {
    common::expect_box_same_as_kotlinc(
        "class Box<out T>(val head: T, val tail: List<T>) : Iterable<T> {\n\
    override fun iterator(): Iterator<T> = (listOf(head) + tail).iterator()\n\
}\n\
fun <T> Box<T>.append(other: Box<T>): Box<T> = Box(head, tail + other)\n\
fun <E> both(a: List<E>, b: List<E>): List<E> = a + b\n\
fun <E> withSet(a: List<E>, b: Set<E>): List<E> = a + b\n\
fun <E> withElement(a: List<E>, x: E): List<E> = a + x\n\
fun <E> setPlus(a: Set<E>, b: List<E>): Set<E> = a + b\n\
fun box(): String {\n\
    val appended = Box(1, listOf(2)).append(Box(3, listOf())).toList()\n\
    if (appended != listOf(1, 2, 3)) return \"append: $appended\"\n\
    if (both(listOf(1), listOf(2, 3)) != listOf(1, 2, 3)) return \"both\"\n\
    if (withSet(listOf(\"a\"), setOf(\"b\")) != listOf(\"a\", \"b\")) return \"withSet\"\n\
    if (withElement(listOf(1), 2) != listOf(1, 2)) return \"withElement\"\n\
    if (setPlus(setOf(1), listOf(1, 2)) != setOf(1, 2)) return \"setPlus\"\n\
    return \"OK\"\n\
}\n",
        "subtype_receiver_binds_declared_formal",
    );
}
