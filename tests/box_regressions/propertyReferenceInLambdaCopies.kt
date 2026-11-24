class Holder(val value: String)

fun <T> evaluate(body: () -> T): T = body()

fun box(): String {
    val bound = Holder("OK")
    val read = evaluate { bound::value }
    return read.get()
}
