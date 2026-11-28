// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +FullValueClasses, +AllowSmartCastsOnValueClassUnderlyingProperties
// MODULE: lib
package lib

value class MultiField(val first: String?, val second: Int?)

value class SingleField(val only: String?)

// MODULE: main(lib)
import lib.MultiField
import lib.SingleField

fun first(v: MultiField): String = if (v.first != null) v.first else "fail: first"

fun second(v: MultiField): Int = if (v.second != null) v.second else -1

fun only(v: SingleField): String = if (v.only != null) v.only else "fail: only"

fun box(): String {
    if (second(MultiField("x", 7)) != 7) return "fail: second"
    if (second(MultiField("x", null)) != -1) return "fail: null second"
    return first(MultiField("O", 1)) + only(SingleField("K"))
}
