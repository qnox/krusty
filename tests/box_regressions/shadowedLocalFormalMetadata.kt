// WITH_STDLIB
// FILE: ShadowedFormalMetadata.kt

interface Token<T> { fun value(): T }
class Outer<T> {
    private fun <T> copy(value: T) = object : Token<T> {
        override fun value(): T = value
    }
    fun use(value: Int) = copy(value).value()
}
fun box(): String = "OK"
