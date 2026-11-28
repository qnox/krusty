// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda passed from a member function of a class, as an Android activity passes its
// content: the lambda captures a local, so it is built where it is passed.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

open class Host {
    open fun onCreate(state: String?) {}
}

fun Host.setContent(content: @Composable () -> Unit) {}

fun Host.setContentWith(parent: String? = null, content: @Composable () -> Unit) {}

@Composable
fun Label(text: String) {}

// MODULE: main(lib)
// FILE: activity.kt
package app

import lib.Host
import lib.Label
import androidx.compose.runtime.Composable
import lib.setContent
import lib.setContentWith

class Activity : Host() {
    override fun onCreate(state: String?) {
        super.onCreate(state)
        val restored = state ?: "none"
        setContent { Label(restored) }
    }

    fun show(text: String) {
        setContent { Label(text) }
    }

    fun fixed() {
        val label = "fixed"
        setContent { Label(label) }
    }

    fun launch(state: String?) {
        val restored = state ?: "launch"
        setContentWith { Screen(restored) }
    }

    fun nothing() {
        setContent { Label("none") }
    }
}

// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Label

@Composable
fun Screen(initial: String) {
    Label(initial)
}

// FILE: box.kt
package app

fun box(): String = "OK"
