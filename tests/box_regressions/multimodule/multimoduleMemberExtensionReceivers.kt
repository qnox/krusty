// MODULE: lib
class Host(private val prefix: String) {
    fun String.decorate(suffix: String): String = prefix + this + suffix
}
fun host(): Host = Host("O")
// MODULE: main(lib)
fun Host.decorateOk(): String = "K".decorate("!")
fun box(): String {
    val decorated = host().decorateOk()
    return if (decorated == "OK!") "OK" else decorated
}
