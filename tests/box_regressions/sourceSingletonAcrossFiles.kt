// WITH_STDLIB
// FILE: SourceSingletonUse.kt
fun box(): String {
    val first: Any = SourceState
    if (initializations != 1) return "first initialization"
    if (first !== SourceState || first !== declaredSingleton()) return "singleton identity"
    advanceSingleton()
    if (singletonState() != 3 || mutations != 1 || initializations != 1) return "first state"
    advanceSingleton()
    if (singletonState() != 5 || mutations != 2 || initializations != 1) return "second state"
    if (first !== declaredSingleton()) return "identity after mutation"
    return "OK"
}

// FILE: SourceSingletonDeclarations.kt
var initializations = 0
var mutations = 0
object SourceState {
    var value = 1
    init { initializations++ }
}
fun declaredSingleton(): Any = SourceState
fun advanceSingleton() {
    SourceState.value += 2
    mutations++
}
fun singletonState(): Int = SourceState.value
