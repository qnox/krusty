// WITH_STDLIB

fun box(): String {
    println("text")
    println()
    print("no newline")
    println(42)
    println(listOf(1, 2))
    print('c')
    println(null as Any?)
    return "OK"
}
