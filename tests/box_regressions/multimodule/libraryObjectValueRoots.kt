// MODULE: lib
// FILE: State.kt
var initializations = 0
object State {
    init { initializations++ }
    var value = 7
}
fun stateFromLibrary(): Any = State

// MODULE: middle(lib)
// FILE: Values.kt
import kotlin.properties.Delegates

// These reads select no member of either object. Their actual instance must already exist
// before a later file selects notNull, success, or failure.
fun delegatesFromMiddle(): Any = Delegates
fun resultCompanionFromMiddle(): Any = Result.Companion
fun stateFromMiddle(): Any = State
fun <T> keep(value: T): T = value

// MODULE: main(lib, middle)
// FILE: Early.kt
val earlyDelegates: Any = delegatesFromMiddle()
val earlyResult: Any = resultCompanionFromMiddle()
val earlyState: Any = stateFromMiddle()

// FILE: Main.kt
import kotlin.properties.Delegates

fun box(): String {
    if (earlyDelegates == null || earlyResult == null) return "null object"
    if (earlyDelegates !== Delegates || earlyResult !== Result.Companion) return "object identity"
    if (keep(delegatesFromMiddle()) !== earlyDelegates) return "generic delegates"
    if (keep(resultCompanionFromMiddle()) !== earlyResult) return "generic result"
    if (earlyDelegates === earlyResult || earlyDelegates === Unit || earlyResult === Int.Companion)
        return "distinct objects"
    if (earlyDelegates !is Delegates || earlyResult !is Result.Companion) return "object type"
    if (earlyDelegates.hashCode() != Delegates.hashCode()) return "delegates hash"
    if (earlyResult.hashCode() != Result.Companion.hashCode()) return "result hash"
    if (earlyDelegates.toString().isEmpty() || earlyResult.toString().isEmpty()) return "object text"
    val property = Delegates.notNull<String>()
    if (property == null) return "delegate member"
    if (Result.success("OK").getOrThrow() != "OK") return "success"
    val failure = IllegalStateException("expected")
    if (Result.failure<String>(failure).exceptionOrNull() !== failure) return "failure"
    if (earlyDelegates !== Delegates || earlyResult !== Result.Companion) return "member identity"
    if (earlyState !== State || stateFromLibrary() !== State || initializations != 1)
        return "state identity or initialization"
    State.value++
    if ((earlyState as State).value != 8 || initializations != 1) return "state mutation"
    return "OK"
}
