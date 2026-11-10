// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `java.nio.Buffer` declares a private field `limit` beside its `limit()` methods.
// The private field is an invisible member candidate for `buffer.limit`; the extension property is
// the read's and the write's visible candidate.
inline var java.nio.Buffer.limit: Int
    get() = limit()
    set(value) {
        limit(value)
    }

fun box(): String {
    val buffer = java.nio.ByteBuffer.allocate(8)
    buffer.limit = 3
    return if (buffer.limit == 3) "OK" else "limit=${buffer.limit}"
}
