// A value class's computed `var` stores through its setter over the carrier: the assigned value
// reaches the setter as the declaration takes it, a value-class value boxed where the property is
// declared `Any?`, and the getter reads it back.
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
    fun offer(item: Any): Boolean { sink.last = item; return sink.last == item }
}

fun box(): String {
    val sink = object : Sink { override var last: Any? = null }
    val wrap = Wrap(sink)
    val count = Count(7)
    wrap.stored = count
    if (sink.last != count) return "fail setter boxes"
    if (wrap.stored !is Count) return "fail getter keeps the box"
    wrap.total = 5
    if (wrap.total != 5) return "fail primitive property"
    if (!wrap.offer(Count(8))) return "fail any argument"
    if (sink.last != Count(8)) return "fail any stored"
    return "OK"
}
