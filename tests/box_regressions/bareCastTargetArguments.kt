open class Gen<T>(val v: T)
class Auto<T>(v: T) : Gen<T>(v)

val Gen<*>.auto: Auto<*>? get() = this as? Auto
fun safe(g: Gen<String>): Auto<String>? = g as? Auto
fun plain(g: Gen<String>): Auto<String> = g as Auto

fun box(): String {
    if (Gen("x").auto != null) return "safe"
    val o = safe(Auto("O"))?.v ?: return "null"
    if (Auto("s").auto?.v != "s") return "star"
    return o + plain(Auto("K")).v
}
