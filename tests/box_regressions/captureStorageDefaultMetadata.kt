
interface Token { fun value(): Int }
private fun copy(value: Int, token: Token = object : Token { override fun value(): Int = value }): Token = token
fun box(): String = if (copy(7).value() == 7) "OK" else "default"
