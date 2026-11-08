// WITH_STDLIB
// FILE: GenericAnonymousMetadataBaseline.kt

interface Token<T>
private fun <T> copy(value: T) = object : Token<T> {}
fun box(): String = "OK"
