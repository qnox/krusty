// A nested generic call whose result is an enclosing declaration's type parameter, or a type built
// from one, is an ordinary argument of that fixed type: it does not fit a function-typed overload
// or a function-typed constructor competitor (kotlinc 2.4.21).
interface Engine

class Config<T>

class Client(val engine: Engine, val config: Config<out Any> = Config<Any>()) {
    var made = "constructor"
}

fun Client(engine: Engine, block: Config<*>.() -> Unit): Client = Client(engine).also { it.made = "function" }

fun <X> id(x: X): X = x

fun pick(block: () -> Unit): String = "function"

fun pick(value: Any): String = "value"

fun <T> boxed(): String = pick(id(Config<T>()))

fun <T : Any> plain(value: T): String = pick(id(value))

fun <T> applied(): String = pick(Config<T>().apply { })

fun <T : Any> constructed(): String = Client(object : Engine {}, Config<T>().apply { }).made

// A nested callee's own formal that shares the enclosing type parameter's name stays open.
fun <X> named(): String = pick(id { })

class Bag<T>(val size: Int)

fun <T> emptyBag(): Bag<T> = Bag(0)

class Duo<A, B>(val first: A, val second: B)

infix fun <A, B> A.with(that: B): Duo<A, B> = Duo(this, that)

fun take(p: Duo<Any, Bag<Any>>): Int = p.second.size

fun <B> shadowed(): Int = take("OK" with emptyBag())

fun box(): String {
    if (boxed<Int>() != "value") return "FAIL boxed"
    if (plain("x") != "value") return "FAIL plain"
    if (applied<Int>() != "value") return "FAIL applied"
    val made = constructed<Int>()
    if (made != "constructor") return "FAIL $made"
    if (named<Int>() != "function") return "FAIL named"
    if (shadowed<Int>() != 0) return "FAIL shadowed"
    return "OK"
}
