// A top-level `lateinit` property: an uninitialized read throws a Kotlin exception naming it,
// `::p.isInitialized` reads the raw storage, and a write makes the property readable.
private lateinit var pending: String
lateinit var published: Any

class Observer {
    fun initialized() = ::pending.isInitialized
    fun fill(value: String) {
        pending = value
    }
}

fun box(): String {
    val observer = Observer()
    if (observer.initialized()) return "FAIL: initialized before any write"
    try {
        pending
        return "FAIL: uninitialized read returned"
    } catch (e: RuntimeException) {
        if (e.message != "lateinit property pending has not been initialized") return "FAIL: ${e.message}"
    }
    observer.fill("O")
    if (!observer.initialized()) return "FAIL: not initialized after a write"
    published = pending + "K"
    return published as String
}
