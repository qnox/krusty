// `this@label` names the receiver of the labeled function literal itself, including inside an
// initializer or expression body whose type is inferred, and inside nested receiver literals.
package p

const val REASON = "r"

annotation class Ann(val v: String)

// An unresolved labeled receiver in an inferred signature must not suppress this annotation's
// constant argument.
@Ann(REASON)
class Uses

fun run2(f: String.() -> String): String = "x".f()

fun <T> run3(t: T, f: T.() -> T): T = t.f()

val inferredProperty = run2 l@{ this@l }

fun inferredFunction() = run2 l@{ this@l + this@l.length }

fun implicitLabel() = run2 { this@run2 }

fun outerLabel() = run3(1) outer@{ run2 { this@outer.toString() + this }.length }

class Member {
    val tag = "member"
    val inferred = run2 l@{ this@l + this@Member.tag.length }
}

class Builder {
    class Case(
        val name: String,
        val relocated: (Case.() -> Case)? = null,
    )

    val input =
        Case("a") original@{
            Case("b") {
                this@original
            }
        }
}

fun box(): String {
    if (inferredProperty != "x") return "property: $inferredProperty"
    if (inferredFunction() != "x1") return "function: ${inferredFunction()}"
    if (implicitLabel() != "x") return "implicit: ${implicitLabel()}"
    if (outerLabel() != 2) return "outer: ${outerLabel()}"
    val member = Member().inferred
    if (member != "x6") return "member: $member"
    val input = Builder().input
    val relocated = input.relocated!!(input)
    if (relocated.name != "b") return "builder: ${relocated.name}"
    val inner = relocated.relocated!!(Builder.Case("c"))
    if (inner.name != "a") return "inner: ${inner.name}"
    return "OK"
}
