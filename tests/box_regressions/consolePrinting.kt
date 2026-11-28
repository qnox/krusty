// WITH_STDLIB

fun box(): String {
    println("text")
    println()
    print("no newline")
    println(42)
    println(listOf(1, 2))
    print('c')
    println(null as Any?)
    // Larger than a pipe buffer, so a host that queues it must keep it ahead of the answer.
    println("x".repeat(200_000))
    return "OK"
}
