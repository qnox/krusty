package krusty

import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile

/** The kotlinc arguments a Kotlin compile task requests, or why krusty leaves it to kotlinc. */
internal class KotlincRequest(val arguments: List<String>, val refusals: List<String>)

/** The compiler options of a Kotlin JVM compile task, read once from its Gradle properties. */
internal data class CompilerOptionValues(
    val extraWarnings: Boolean = false,
    val suppressWarnings: Boolean = false,
    val verbose: Boolean = false,
    val languageVersion: String? = null,
    val apiVersion: String? = null,
    val optIn: List<String> = emptyList(),
    val freeCompilerArgs: List<String> = emptyList(),
    val moduleName: String? = null,
    val jvmTarget: String? = null,
    val jvmDefault: String? = null,
    val multiPlatform: Boolean = false,
    val progressiveMode: Boolean = false,
    val javaParameters: Boolean = false,
    val noJdk: Boolean = false,
    val allWarningsAsErrors: Boolean = false,
)

internal fun kotlincRequest(task: KotlinJvmCompile, pluginVersion: String, compilerVersion: String): KotlincRequest {
    val options = task.compilerOptions
    val values = CompilerOptionValues(
        extraWarnings = options.extraWarnings.getOrElse(false),
        suppressWarnings = options.suppressWarnings.getOrElse(false),
        verbose = options.verbose.getOrElse(false),
        languageVersion = options.languageVersion.orNull?.version,
        apiVersion = options.apiVersion.orNull?.version,
        optIn = options.optIn.getOrElse(emptyList()),
        freeCompilerArgs = options.freeCompilerArgs.getOrElse(emptyList()),
        moduleName = options.moduleName.orNull,
        jvmTarget = options.jvmTarget.orNull?.target,
        jvmDefault = options.jvmDefault.orNull?.compilerArgument,
        multiPlatform = task.multiPlatformEnabled.getOrElse(false),
        progressiveMode = options.progressiveMode.getOrElse(false),
        javaParameters = options.javaParameters.getOrElse(false),
        noJdk = options.noJdk.getOrElse(false),
        allWarningsAsErrors = options.allWarningsAsErrors.getOrElse(false),
    )
    return kotlincRequest(values, pluginVersion, compilerVersion)
}

internal fun kotlincRequest(
    options: CompilerOptionValues,
    pluginVersion: String,
    compilerVersion: String,
): KotlincRequest {
    fun refused(reason: String) = KotlincRequest(emptyList(), listOf(reason))
    if (compilerVersion != pluginVersion) {
        return refused("Kotlin compiler $compilerVersion differs from Kotlin Gradle plugin $pluginVersion")
    }
    val unsupported = listOf(
        "extraWarnings" to options.extraWarnings,
        "suppressWarnings" to options.suppressWarnings,
        // Named `-Xwarning-level` policy is forwarded to the compiler, whose diagnostic registry
        // is the authoritative place to validate names and apply severities. `-Werror` is the
        // global form of that policy: the compiler fails the compilation when any warning is
        // emitted.
        "verbose" to options.verbose,
        // `useModuleDetection` (set on every Android compile) only configures kotlinc's
        // multi-module incremental compilation, never a compiler argument; a full krusty compile
        // has nothing to detect.
    ).filter { (_, enabled) -> enabled }
        .map { (name, _) -> "krusty does not support compilerOptions.$name" }
    if (unsupported.isNotEmpty()) return KotlincRequest(emptyList(), unsupported)

    val freeArguments = options.freeCompilerArgs
    structuredOptInRefusal(options.optIn)?.let { return refused(it) }
    freeArgumentRefusal(freeArguments, pluginVersion)?.let { return refused(it) }
    // KGP fills kotlinc's arguments from the structured options and appends the free arguments
    // after them, so a free argument overrides the structured option it restates.
    val arguments = ArrayList<String>()
    options.languageVersion?.let { arguments.addPair("-language-version", it) }
    options.apiVersion?.let { arguments.addPair("-api-version", it) }
    options.optIn.forEach { marker ->
        arguments.add("-opt-in=$marker")
    }
    options.moduleName?.takeIf(String::isNotEmpty)?.let {
        arguments.addPair("-module-name", it)
    }
    options.jvmTarget?.let { arguments.addPair("-jvm-target", it) }
    options.jvmDefault?.let { arguments.addPair("-jvm-default", it) }
    if (options.multiPlatform) arguments.add("-Xmulti-platform")
    if (options.progressiveMode) arguments.add("-progressive")
    if (options.javaParameters) arguments.add("-java-parameters")
    if (options.noJdk) arguments.add("-no-jdk")
    if (options.allWarningsAsErrors && "-Werror" !in freeArguments) {
        arguments.add("-Werror")
    }
    arguments.addAll(freeArguments)
    return KotlincRequest(arguments, emptyList())
}

// kotlinc accepts a repeated `-opt-in` marker, so a repeated structured marker reaches krusty as
// KGP passes it.
private fun structuredOptInRefusal(markers: List<String>): String? =
    if (markers.any(String::isBlank)) "compilerOptions.optIn contains an empty marker" else null

private fun MutableList<String>.addPair(name: String, value: String) {
    add(name)
    add(value)
}
