// MODULE: library
// FILE: Library.kt
package fixture

internal enum class State { IDLE, RUNNING, DONE }

class Job(private val raw: Int) {
    internal val state: State
        get() = State.values()[raw]

    val name: String
        get() = state.name
}

fun stateNames(): String = State.values().joinToString(",") { it.name }

fun stateOrdinal(name: String): Int = State.valueOf(name).ordinal

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (stateNames() != "IDLE,RUNNING,DONE") return "fail: values ${stateNames()}"
    if (stateOrdinal("DONE") != 2) return "fail: valueOf ${stateOrdinal("DONE")}"
    val running = Job(1).name
    return if (running == "RUNNING") "OK" else "fail: member $running"
}
