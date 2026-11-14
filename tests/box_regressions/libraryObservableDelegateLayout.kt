// WITH_STDLIB

import kotlin.properties.Delegates

var observedCalls: Int = 0
var observedChanges: Int = 0

open class ObservedOwner {
    var label: String by Delegates.observable("O") { _, old, new ->
        observedCalls++
        if (old != new) observedChanges++
    }
}

class ObservedChild : ObservedOwner() {
    init { label = "OK" }
}

fun box(): String {
    observedCalls = 0
    observedChanges = 0
    val owner: ObservedOwner = ObservedChild()
    if (owner.label != "OK" || observedCalls != 1 || observedChanges != 1) return "initialization"
    owner.label = "OK"
    if (observedCalls != 2 || observedChanges != 1) return "unchanged value"
    owner.label = "K"
    if (owner.label != "K" || observedCalls != 3 || observedChanges != 2) return "updated value"
    return "OK"
}
