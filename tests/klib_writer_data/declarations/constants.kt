package lib.sub
import kotlin.contracts.*
@OptIn(ExperimentalContracts::class)
fun isStr(x: Any?): Boolean { contract { returns(true) implies (x is String) }; return x is String }
inline fun run2(noinline a: () -> Unit, crossinline b: () -> Unit) { a(); b() }
const val CI: Int = 42
const val CL: Long = 7L
const val CD: Double = 1.5
const val CB: Boolean = true
const val CC: Char = 'x'
private val pv = 1
