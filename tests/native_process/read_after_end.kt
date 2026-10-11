// `readln` at the end of input raises; the program reports what it caught.
fun main() {
    println(readln())
    try {
        readln()
        println("no exception")
    } catch (e: RuntimeException) {
        println(e::class.simpleName)
        println(e.message)
    }
}
