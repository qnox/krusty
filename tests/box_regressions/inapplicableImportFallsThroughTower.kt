// A scope-tower level whose candidates do not accept the arguments does not own the call: the
// nearest level with an applicable candidate does. An explicitly imported class and its factory
// functions leave `Bytes(buffer)` to the current package, an explicitly imported function leaves
// `pick(buffer)` to the current package, and a star import leaves `pick("x", 1)` to the next
// star-imported package. When a nearer level applies, it still wins over a farther one.
// FILE: lib.kt
package lib

class Bytes(val data: IntArray, val start: Int = 0)

fun Bytes(vararg values: Int): Bytes = Bytes(values)

fun Bytes(): Bytes = Bytes(IntArray(0))

fun pick(value: String): String = "imported $value"

// FILE: stars.kt
package stars

fun pick(value: String, count: Int): String = "star $value $count"

// FILE: main.kt
package app

import lib.Bytes
import lib.pick
import stars.*

class Buffer(val size: Int)

fun Bytes(buffer: Buffer): Bytes = Bytes(IntArray(buffer.size))

fun skip(buffer: Buffer): Int = skip(Bytes(buffer))

fun skip(bytes: Bytes): Int = bytes.data.size

fun pick(buffer: Buffer): String = "package ${buffer.size}"

fun pick(value: String): String = "package $value"

fun box(): String {
    if (skip(Buffer(3)) != 3) return "FAIL bytes: ${skip(Buffer(3))}"
    if (Bytes(1, 2).data.size != 2) return "FAIL factory"
    if (pick(Buffer(2)) != "package 2") return "FAIL package: ${pick(Buffer(2))}"
    if (pick("x") != "imported x") return "FAIL nearest: ${pick("x")}"
    if (pick("x", 1) != "star x 1") return "FAIL star: ${pick("x", 1)}"
    return "OK"
}
