// WITH_STDLIB
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

open class Channel<T> {
    var trail = ""
    open fun parentJob(job: String): Channel<T> {
        trail += "member:$job;"
        return this
    }
}

@Suppress("NOTHING_TO_INLINE")
@kotlin.internal.HidesMembers
internal inline fun <T> Channel<T>.parentJob(job: String?): Channel<T> =
    if (job != null) parentJob(job) else this.also { trail += "extension;" }

fun box(): String {
    val channel = Channel<Int>()
    channel.parentJob(null).parentJob("x")
    return if (channel.trail == "extension;member:x;") "OK" else channel.trail
}
