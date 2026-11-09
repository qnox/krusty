
interface Token { fun value(): Int }
private fun copy(value: Int) = object : Token { override fun value(): Int = value }
fun box(): String = if (copy(7).value() == 7) "OK" else "ordinary"
