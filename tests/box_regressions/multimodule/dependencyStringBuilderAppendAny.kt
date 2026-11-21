// MODULE: lib
// FILE: Describe.kt
package fixture

class Tag(private val n: Int) {
    override fun toString(): String = "tag$n"
}

// `StringBuilder.append(Any?)` appends the value's `toString()`, or `null`.
fun describe(value: Any?): String = StringBuilder().append("<").append(value).append(">").toString()

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (describe(Tag(7)) != "<tag7>") return "tag ${describe(Tag(7))}"
    if (describe(null) != "<null>") return "null ${describe(null)}"
    if (describe(42) != "<42>") return "int ${describe(42)}"
    if (describe("s") != "<s>") return "string ${describe("s")}"
    return "OK"
}
