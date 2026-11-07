
open class Base(val saved: Int)
private fun copy(value: Int) = object {
    fun read(): String {
        val nested = object : Base(value + 1) { fun original() = value }
        return if (nested.saved == 8 && nested.original() == 7) "OK" else "owner"
    }
}
fun box(): String = copy(7).read()
