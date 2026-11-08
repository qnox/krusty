// WITH_STDLIB
// FILE: NamedLocalArgumentMetadata.kt

private fun <T> enclose(value: T): Any {
    class Holder<U>(val item: U, val seen: T) {
        fun self() = this
    }
    return Holder(value, value).self()
}
fun box(): String = "OK"
