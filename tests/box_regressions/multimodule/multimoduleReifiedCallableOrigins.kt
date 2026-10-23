// LANGUAGE: +ContextParameters
// MODULE: lib
inline fun <reified T> top(value: T, probe: Any): Boolean = probe is T
inline fun <reified T> Any?.topExtension(): Boolean = this is T
context(marker: String)
inline fun <reified T> contextual(probe: Any): Boolean = probe is T
inline fun <reified T> defaulted(probe: Any? = null): Boolean = probe is T
class Host {
    inline fun <reified T> member(value: T, probe: Any): Boolean = probe is T
    inline fun <reified T> Any?.memberExtension(): Boolean = this is T
}
fun host(): Host = Host()
// MODULE: main(lib)
fun box(): String {
    val host = host()
    if (!top("top", "probe")) return "top"
    if (!top(probe = "named probe", value = "named")) return "named"
    if (!"extension".topExtension<String>()) return "extension"
    with("context") {
        if (!contextual<String>("probe")) return "context"
    }
    if (defaulted<String>()) return "default"
    if (!host.member("member", "probe")) return "member"
    if (!host.run { "member extension".memberExtension<String>() }) return "member extension"
    return "OK"
}
