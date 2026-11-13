// WITH_STDLIB
// A receiver-less callable reference passed where overloads differ only in the result of their
// function parameter (`flatMap` over Iterable and Sequence results) selects the overload whose
// result the referenced function produces, for a generic, an overloaded and a library target.
fun <T> single(x: T): List<T> = listOf(x)

fun twice(x: Int): List<Int> = listOf(x, x)
fun twice(x: String): List<String> = listOf(x, x)

fun box(): String {
    val generic = listOf(1, 2).flatMap(::single)
    if (generic != listOf(1, 2)) return "FAIL generic $generic"
    val overloaded = listOf(1, 2).flatMap(::twice)
    if (overloaded != listOf(1, 1, 2, 2)) return "FAIL overloaded $overloaded"
    val library = listOf(1, 2).flatMap(::listOf)
    if (library != listOf(1, 2)) return "FAIL library $library"
    val nested = listOf(arrayOf("a", "b")).flatMap(::listOf)
    if (nested.size != 1) return "FAIL nested ${nested.size}"
    return "OK"
}
