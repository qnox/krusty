package lib.sub

const val GREETING: String = "hi"
var counter: Int = 0
val list: List<String> = listOf("a")

fun answer(): String = "OK"
fun <T> id(value: T): T = value
fun Int.twice(x: Long = 2L): Long = this * x
internal fun hidden(vararg xs: Int): Int = xs.size
inline fun <reified R> cast(x: Any?): R? = x as? R
