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
        .sortedWith(compareBy<String> { it.versionParts()[0] }
            .thenBy { it.versionParts()[1] }
            .thenBy { it.versionParts()[2] })
        .toList()
}

private fun String.versionParts(): List<Int> = split('.').map { it.toInt() }

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
