// kotlinc stores an inline function's type-parameter value as its erased bound (`Object` for an
// unconstrained `T`), and its inliner hands each inlined lambda parameter over as the `Object`
// `invoke` takes. Each use casts the value back to the call-site type. The optimizer removes the
// casts of values whose class it knows exactly, so they stay for `this` and for a subclass instance.
// An empty one-line lambda keeps the `nop` of its closing brace on a line of its own.
open class C(val tag: String) {
    var touched = 0

    fun touch() {
        touched++
    }

    fun applied(): C = apply { touch() }

    fun alsoTouched(): C = also { it.touch() }

    fun emptyApply(): C = apply { }

    fun emptyAlso(): C = also { }

    fun letted(): Any = let { it }

    fun withed(): C = with(this) { this }

    fun parameter(other: C): C = other.apply { }
}

class D(tag: String) : C(tag)

fun widened(d: D): C = d.apply<C> { touch() }

fun widenedLet(d: D): C = d.let<C, C> { it }

fun box(): String {
    val c = C("c")
    if (c.applied() !== c || c.alsoTouched() !== c || c.touched != 2) return "FAIL: touched ${c.touched}"
    if (c.emptyApply() !== c || c.emptyAlso() !== c) return "FAIL: empty"
    if (c.letted() !== c || c.withed() !== c) return "FAIL: let/with"
    if (c.parameter(c) !== c) return "FAIL: parameter"
    val d = D("d")
    if (widened(d) !== d || d.touched != 1) return "FAIL: widened"
    if (widenedLet(d).tag != "d") return "FAIL: widenedLet"
    return "OK"
}
