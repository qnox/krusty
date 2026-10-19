package sample

fun twice(x: Int): Int {
    val y = x
    return add(y, y)
}

fun add(a: Int, b: Int): Int = a

fun greet(name: String) {
    println(name)
    var n = 1
    n = 2
}
