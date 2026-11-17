external fun linked(): String
interface Contract { fun member(): String }
abstract class Base { abstract fun member(): String }
fun missing(): String
fun <T> generic(value: T): T
fun String.extension(): String
fun implemented(): String = "OK"
val independentError: String = 1
