// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Gauge.kt
package gauge

interface Gauge {
    context(unit: Int, tag: String)
    var level: Int
}

context(unit: Int, tag: String)
fun Gauge.raise(delta: Int) {
    level = level + delta
}

// MODULE: main(lib)
// FILE: Main.kt
import gauge.Gauge
import gauge.raise

class Tank : Gauge {
    var stored = 0
    var trace = ""

    context(unit: Int, tag: String)
    override var level: Int
        get() { trace += "g" + tag; return stored * unit }
        set(value) { trace += "s" + tag; stored = value / unit }
}

fun box(): String {
    val tank = Tank()
    val gauge: Gauge = tank
    with(4) {
        with("a") {
            gauge.level = 20
            if (tank.stored != 5) return "setter stored " + tank.stored
            if (gauge.level != 20) return "getter " + gauge.level
            gauge.raise(8)
            if (tank.stored != 7) return "producer helper stored " + tank.stored
        }
    }
    if (tank.trace != "sagagasa") return "trace " + tank.trace
    return "OK"
}
