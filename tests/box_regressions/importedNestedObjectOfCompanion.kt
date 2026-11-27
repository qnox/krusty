// An explicit import of an object nested in a companion names the nested classifier, not a
// member of the companion, so its constants read through the imported object.

// FILE: sample/Bill.kt
package sample

interface Bill {
    fun generate()

    companion object {
        const val STEP_ID: String = "sbom"

        object Suppliers {
            const val GOOGLE: String = "Google LLC"
            const val APACHE: String = "The Apache Software Foundation"
        }
    }
}

// FILE: main.kt
package consumer

import sample.Bill.Companion.Suppliers

fun box(): String =
    if (Suppliers.GOOGLE == "Google LLC" && Suppliers.APACHE.startsWith("The Apache")) "OK" else "fail"
