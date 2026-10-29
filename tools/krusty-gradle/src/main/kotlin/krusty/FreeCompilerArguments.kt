package krusty

import org.gradle.api.GradleException

// Free arguments reach krusty verbatim, as KGP passes them to kotlinc: after the arguments derived
// from the structured compiler options, so a free argument that restates one of those options
// (`-progressive`, `-module-name=okhttp`) wins exactly as it does for kotlinc. krusty parses them with
// kotlinc's argument table, reports kotlinc's errors and warnings, and refuses an argument it does
// not implement. What this plugin checks is only the boundary of the arguments it owns: the task's
// sources, destination, libraries and compiler plugins. A free argument must not reach them under
// any spelling (a short or deprecated alias), hide them in an argument file, or turn them into
// sources after `--`.

/** A kotlinc argument's canonical name, and whether a bare occurrence consumes the next token. */
private class KotlincArgument(val name: String, val takesValue: Boolean)

private fun argumentTable(release: String): String = "/krusty/kotlinc-arguments/$release.tsv"

private fun readKotlincArguments(release: String): Map<String, KotlincArgument> {
    val resource = argumentTable(release)
    val text = KrustyKotlinPlugin::class.java.getResource(resource)?.readText()
        ?: throw GradleException("the krusty plugin is missing its kotlinc argument table $resource")
    val arguments = HashMap<String, KotlincArgument>()
    for (line in text.lineSequence()) {
        if (line.isEmpty() || line.startsWith("#")) continue
        val columns = line.split('\t')
        val argument = KotlincArgument(columns[0], takesValue = columns[3] != "bool")
        for (spelling in columns.subList(0, 3)) {
            if (spelling.isNotEmpty()) arguments[spelling] = argument
        }
    }
    return arguments
}

/**
 * Every spelling (name, short name, deprecated name) that the selected kotlinc release declares,
 * read from the same release table the compiler vendors. A release whose argument surface equals
 * its predecessor's has no table of its own and reads the predecessor's.
 */
private val kotlincArgumentsByVersion: Map<String, Map<String, KotlincArgument>> by lazy {
    val tables = HashMap<String, Map<String, KotlincArgument>>()
    supportedKotlinVersions.mapNotNull { version ->
        val release = recordedRelease(version) {
            KrustyKotlinPlugin::class.java.getResource(argumentTable(it)) != null
        } ?: return@mapNotNull null
        version to tables.getOrPut(release) { readKotlincArguments(release) }
    }.toMap()
}

/** The plugin-owned input behind each argument a free argument must not reach, by canonical name. */
private val reservedArguments: Map<String, String> = mapOf(
    "-d" to "the plugin-owned destination",
    "-Xbuild-file" to "the task sources and the plugin-owned destination",
    "-classpath" to "the task libraries classpath",
    "-Xfriend-paths" to "the task friend paths",
    "-Xfragments" to "the multiplatform compilation's source sets",
    "-Xfragment-sources" to "the multiplatform compilation's source sets",
    "-Xfragment-refines" to "the multiplatform compilation's source sets",
    "-jdk-home" to "compilerOptions.noJdk and the Java toolchain",
    "-no-jdk" to "compilerOptions.noJdk and the Java toolchain",
    "-no-stdlib" to "the plugin-owned dependency policy",
    "-no-reflect" to "the plugin-owned dependency policy",
    "-kotlin-home" to "the plugin-owned dependency policy",
    // krusty's own argument, not in kotlinc's table.
    "-Xkotlin-reference-version" to "the Kotlin Gradle plugin version",
    "-Xplugin" to "compiler plugin configuration",
    "-P" to "compiler plugin configuration",
    "-Xcompiler-plugin" to "compiler plugin configuration",
    "-Xcompiler-plugin-order" to "compiler plugin configuration",
)

/**
 * Walk the free arguments the way kotlinc's parser does and return why one would reach a
 * plugin-owned argument, or null when none does. kotlinc expands an `@argfile` wherever it appears,
 * before parsing, so a token starting with `@` is refused in any position.
 */
internal fun freeArgumentRefusal(input: List<String>, kotlinVersion: String): String? {
    val kotlincArguments = kotlincArgumentsByVersion[kotlinVersion]
        ?: throw GradleException("the krusty plugin has no kotlinc argument table for Kotlin $kotlinVersion")
    input.firstOrNull { it.startsWith("@") }?.let { argument ->
        return "freeCompilerArg '$argument' is an argument file, which could hide plugin-owned arguments; pass its arguments as freeCompilerArgs instead"
    }
    var index = 0
    while (index < input.size) {
        val argument = input[index++]
        if (argument == "--") {
            return "freeCompilerArg '--' would make the arguments after it sources; add sources to the task's source set instead"
        }
        val key = argument.substringBefore(if (argument.startsWith("-XXLanguage")) ':' else '=')
        val spec = kotlincArguments[key]
        reservedArguments[spec?.name ?: key]?.let { owner ->
            return "freeCompilerArg '$argument' conflicts with $owner; configure the structured Gradle input instead"
        }
        if (spec == null) {
            if (!argument.startsWith("-")) {
                return "freeCompilerArg '$argument' would be compiled as a source; add sources to the task's source set instead"
            }
        } else if (spec.takesValue && key == argument) {
            // The next token is this argument's value, whatever it looks like.
            index++
        }
    }
    return null
}
