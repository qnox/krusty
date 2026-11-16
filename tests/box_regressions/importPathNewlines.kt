// A qualified import continues across a newline before each dot, and a file annotation across
// newlines around its colon: the import after each still opens a directive.
@file
:
Suppress("UNUSED_VARIABLE")

import kotlin
    .math
    .abs
import kotlin.math.max

fun box(): String {
    if (abs(-2) != 2) return "FAIL abs"
    if (max(1, 3) != 3) return "FAIL max"
    return "OK"
}
