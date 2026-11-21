// A value class's writable computed property declared in one file is assigned and read from
// another file of the module: the entry point the declaring file exports takes the carrier, and
// the setter it calls takes it as the declaration does.
// FILE: declaration.kt
interface Sink {
    var last: Any?
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class Count(val value: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Wrap(val sink: Sink) {
    var stored: Any?
        get() = sink.last
        set(v) { sink.last = v }
    var total: Int
        get() = (sink.last as? Int) ?: 0
        set(v) { sink.last = v }
}

// FILE: main.kt
fun box(): String {
    val sink = object : Sink { override var last: Any? = null }
    val wrap = Wrap(sink)
    val count = Count(7)
    wrap.stored = count
    if (sink.last != count) return "fail setter boxes"
    if (wrap.stored !is Count) return "fail getter keeps the box"
    if (wrap.stored != count) return "fail getter value"
    wrap.total = 5
    if (wrap.total != 5) return "fail primitive property"
    if (sink.last != 5) return "fail stored primitive"
    return "OK"
}
