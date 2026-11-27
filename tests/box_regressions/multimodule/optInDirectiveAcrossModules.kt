// OPT_IN: lib.Experimental
// MODULE: lib
package lib

@RequiresOptIn
annotation class Experimental

@Experimental
fun greeting(): String = "OK"

// MODULE: main(lib)
import lib.greeting

fun box(): String = greeting()
