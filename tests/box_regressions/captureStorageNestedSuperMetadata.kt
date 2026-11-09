
open class Base(val saved: Int)
private fun copy(value: Int) = object {
    fun read(): String {
        val nested = object : Base(value) {}
        return if (nested.saved == 7) "OK" else "coordinate"
    }
}
fun box(): String = copy(7).read()
