package krusty.pluginhost

import java.io.DataInputStream
import java.io.DataOutputStream

// The frame and value encodings of the protocol (`src/protocol.rs`): big-endian integers, strings as
// a u32 byte length and UTF-8, lists as a u32 count and the elements.

internal class Frame(bytes: ByteArray) : DataInputStream(bytes.inputStream())

internal fun DataInputStream.readFrame(): ByteArray {
    val bytes = ByteArray(readInt())
    readFully(bytes)
    return bytes
}

internal fun DataInputStream.readString(): String {
    val bytes = ByteArray(readInt())
    readFully(bytes)
    return String(bytes, Charsets.UTF_8)
}

internal fun <T> DataInputStream.readList(element: DataInputStream.() -> T): List<T> =
    List(readInt()) { element() }

internal fun DataOutputStream.writeString(value: String) {
    val bytes = value.toByteArray(Charsets.UTF_8)
    writeInt(bytes.size)
    write(bytes)
}

internal fun send(frames: DataOutputStream, body: DataOutputStream.() -> Unit) {
    val payload = java.io.ByteArrayOutputStream()
    DataOutputStream(payload).apply(body).flush()
    frames.writeInt(payload.size())
    payload.writeTo(frames)
    frames.flush()
}
