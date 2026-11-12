package krusty

import org.gradle.api.GradleException

/**
 * The Kotlin versions krusty supports: the compiler's own `kotlin-versions` manifest, packaged
 * with the plugin, oldest first. Nothing else in the plugin names a version.
 */
internal val supportedKotlinVersions: List<String> by lazy {
    val resource = "/krusty/kotlin-versions"
    val text = KrustyKotlinPlugin::class.java.getResource(resource)?.readText()
        ?: throw GradleException("the krusty plugin is missing its Kotlin version manifest $resource")
    text.lineSequence()
        .map { it.substringBefore('#').trim() }
        .filter { it.isNotEmpty() }
        .map { it.split(Regex("\\s+")).first() }
        .sortedWith(compareBy { it.versionKey() })
        .toList()
}

/**
 * A pre-release precedes the stable release of the same numbers: `2.5.0-Beta1` < `2.5.0-Beta2` <
 * `2.5.0-RC` < `2.5.0-RC2` < `2.5.0`. The qualifier is part of the identity.
 */
private data class VersionKey(
    val major: Int,
    val minor: Int,
    val patch: Int,
    val channel: Int,
    val pre: Int,
) : Comparable<VersionKey> {
    override fun compareTo(other: VersionKey): Int {
        major.compareTo(other.major).takeIf { it != 0 }?.let { return it }
        minor.compareTo(other.minor).takeIf { it != 0 }?.let { return it }
        patch.compareTo(other.patch).takeIf { it != 0 }?.let { return it }
        channel.compareTo(other.channel).takeIf { it != 0 }?.let { return it }
        return pre.compareTo(other.pre)
    }
}

private fun String.versionKey(): VersionKey {
    val dash = indexOf('-')
    val numbers = if (dash < 0) this else substring(0, dash)
    val qualifier = if (dash < 0) null else substring(dash + 1)
    val parts = numbers.split('.')
    if (parts.size != 3) throw GradleException("invalid Kotlin version $this")
    val (channel, pre) = when (qualifier) {
        null -> 2 to 0
        "RC" -> 1 to 0
        else -> {
            val beta = Regex("^Beta([1-9][0-9]*)$").matchEntire(qualifier)
            val candidate = Regex("^RC([1-9][0-9]*)$").matchEntire(qualifier)
            when {
                beta != null -> 0 to beta.groupValues[1].toInt()
                candidate != null -> 1 to candidate.groupValues[1].toInt()
                else -> throw GradleException("invalid Kotlin version $this")
            }
        }
    }
    return VersionKey(parts[0].toInt(), parts[1].toInt(), parts[2].toInt(), channel, pre)
}

/**
 * The newest supported version at or below [version] for which [isRecorded] holds. Data recorded
 * from kotlinc releases is kept only where it changes, so a release without its own record reads
 * its predecessor's, as the compiler does.
 */
internal fun recordedRelease(version: String, isRecorded: (String) -> Boolean): String? {
    val index = supportedKotlinVersions.indexOf(version)
    if (index < 0) return null
    return supportedKotlinVersions.subList(0, index + 1).lastOrNull(isRecorded)
}
