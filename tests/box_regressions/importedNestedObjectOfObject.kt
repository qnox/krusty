// An explicit import of an object nested in an object names the nested classifier, not a member
// of the outer object, so its constant reads through the imported object.

// FILE: sample/Holder.kt
package sample

object Holder {
    object Nested {
        const val VALUE: String = "OK"
    }
}

// FILE: main.kt
package consumer

import sample.Holder.Nested

fun box(): String = Nested.VALUE
