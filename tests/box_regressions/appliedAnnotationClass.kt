// An annotation class that is only applied, never constructed, compiles on every backend.

annotation class Tag(val n: Int)

annotation class Marker

@Tag(1)
class Marked(@Tag(2) val value: String) {
    @Marker
    fun read(): String = value
}

@Marker
fun box(): String {
    @Tag(3) val marked = Marked("OK")
    return marked.read()
}
