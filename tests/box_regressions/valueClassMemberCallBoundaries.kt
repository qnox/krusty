// A call of a value class's own member is taken by the member's declared parameters: a value-class
// argument is boxed where the member declares a reference (`Any`, a type parameter, a nullable
// value class) and stays its carrier where the member declares the value class itself. This
// covers the generated `equals`, a defaulted member parameter, and a member declared over another
// value class.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Count(val value: Int) {
    fun plus(other: Count = Count(42)): Count = Count(value + other.value)
    fun same(other: Count?): Boolean = other != null && other.value == value
    fun describe(other: Any): String = if (other is Count) "count:" + other.value else "other"
    fun <T> echo(other: T): T = other
    val doubled: Int get() = value * 2
    override fun toString(): String = "Count#" + value
}

interface Sink {
    var last: Any?
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class Wrap(val sink: Sink) {
    var stored: Any?
        get() = sink.last
        set(v) { sink.last = v }
    fun offer(item: Any): Boolean { sink.last = item; return sink.last == item }
}

fun box(): String {
    val a = Count(7)
    val b = Count(7)
    if (!a.equals(b)) return "fail equals value"
    if (a.equals(Count(8))) return "fail equals other value"
    val any: Any = b
    if (!a.equals(any)) return "fail equals any"
    if (a.plus().value != 49) return "fail default"
    if (a.plus(Count(1)).value != 8) return "fail explicit"
    if (!a.same(b)) return "fail nullable"
    if (a.same(null)) return "fail nullable null"
    if (a.describe(b) != "count:7") return "fail any param"
    if (a.echo(b).value != 7) return "fail generic"
    if (a.doubled != 14) return "fail computed getter"
    if (a.toString() != "Count#7") return "fail custom toString"
    val sink = object : Sink { override var last: Any? = null }
    val w = Wrap(sink)
    w.stored = b
    if (sink.last != b) return "fail setter boxes"
    if (!w.offer(a)) return "fail any argument"
    if (sink.last != a) return "fail any stored"
    return "OK"
}
