// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable receiver function value is located where its selector starts, whatever separates
// the selector from its receiver: a comment that spells the name, a line break, parentheses, or backticks.
// FILE: receiverInvokeTrivia.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Leaf() {}

@Composable
fun Commented(content: @Composable String.() -> Unit) {
    "x" /* content */ .content()
}

@Composable
fun Broken(content: @Composable String.() -> Unit) {
    "x" // content
        .content()
}

@Composable
fun Wrapped(content: @Composable String.() -> Unit) {
    ("x").content()
}

@Composable
fun Escaped(content: @Composable String.() -> Unit) {
    "x".`content`()
}

@Composable
fun ByName(content: @Composable String.() -> Unit) {
    content("x")
}

@Composable
fun Screen() {
    Commented { Leaf() }
    Broken { Leaf() }
    Wrapped { Leaf() }
    Escaped { Leaf() }
    ByName { Leaf() }
}

fun box(): String = "OK"
