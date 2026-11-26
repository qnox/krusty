// Object and companion `const val`s are storage of their classifier. Every read inlines the
// constant, and the declaring classifier still owns the declaration on every backend.

object Limits {
    const val COUNT = 3
    const val TOTAL: Long = 1L shl 40
    const val RATIO = 0.5
    const val NAME = "limits"
    const val ENABLED = true

    fun describe() = "$NAME:$COUNT"
}

class Gauge(val value: Int) {
    fun clamped() = if (value > MAX) MAX else value

    companion object {
        const val MAX = 10
        const val UNIT = 'u'
    }
}

fun box(): String {
    if (Limits.COUNT != 3) return "fail count"
    if (Limits.TOTAL != 1099511627776L) return "fail total"
    if (Limits.RATIO != 0.5) return "fail ratio"
    if (!Limits.ENABLED) return "fail enabled"
    if (Limits.describe() != "limits:3") return "fail describe ${Limits.describe()}"
    if (Gauge(42).clamped() != Gauge.MAX) return "fail clamp"
    if (Gauge(4).clamped() != 4) return "fail value"
    if ("${Gauge.UNIT}${Gauge.MAX}" != "u10") return "fail unit"
    return "OK"
}
