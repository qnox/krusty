// An override whose expression body is the null literal publishes Nothing?, in an anonymous object
// that initializes a property with a declared type too, suspend or not, as kotlinc does.
interface Tracer {
    fun plain(): String?
    suspend fun activity(): Any?
    companion object {
        var current: Tracer = object : Tracer {
            override fun plain() = null
            override suspend fun activity() = null
        }
    }
}
val typed: Tracer = object : Tracer {
    override fun plain() = null
    override suspend fun activity() = null
}
fun box(): String = if (Tracer.current.plain() == null && typed.plain() == null) "OK" else "fail"
