package fixture

fun referenceStep(value: Int): Int = value + 1
fun otherReferenceStep(value: Int): Int = value + 2

fun makeReference(): (Int) -> Int = ::referenceStep
inline fun inlineMakeReference(): (Int) -> Int = ::referenceStep
fun makeOtherReference(): (Int) -> Int = ::otherReferenceStep
