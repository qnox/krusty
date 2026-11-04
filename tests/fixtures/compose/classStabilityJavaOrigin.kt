// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A Kotlin builtin that the JVM realizes by a Java class (`List`, `CharSequence`) is a Kotlin
// interface to kotlinc's stability inference, so two fields of it leave a class stable. A Java
// interface declared in the same package is a Java class, which makes a class unstable.
// FILE: Foreign.java
package kotlin.collections;

public interface Foreign {}

// FILE: javaOrigin.kt
package model

import kotlin.collections.Foreign

class Lists(val first: List<Int>, val second: List<String>)
class Texts(val first: CharSequence, val second: CharSequence)
class Foreigns(val first: Foreign, val second: Foreign)

// FILE: box.kt
package model

private val expected = listOf(
    "model.Lists" to 0,
    "model.Texts" to 0,
    "model.Foreigns" to 8,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (Class.forName(name).getField("\$stable").getInt(null) != bits) return "$name: not $bits"
    }
    return "OK"
}
