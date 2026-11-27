// MODULE: lib
// kotlinc lets a file that suppresses INVISIBLE_REFERENCE and INVISIBLE_MEMBER call a dependency's
// internal extensions through an import, defaulted parameters included.
package okio.internal

internal fun ByteArray.commonToUtf8String(beginIndex: Int = 0, endIndex: Int = size): String =
    String(this, beginIndex, endIndex - beginIndex)

internal fun String.commonAsUtf8ToByteArray(): ByteArray = toByteArray()

// MODULE: main(lib)
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

import okio.internal.commonAsUtf8ToByteArray
import okio.internal.commonToUtf8String

fun ByteArray.decode(): String = commonToUtf8String()

fun box(): String = "OK".commonAsUtf8ToByteArray().decode()
