
interface Token { fun value(): Int }
private inline fun copy(value: Int) = object : Token {
    val value: Int = value
    override fun value(): Int = this.value
}
fun box(): String = if (copy(7).value() == 7) "OK" else "property"
