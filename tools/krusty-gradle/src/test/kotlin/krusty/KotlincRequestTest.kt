package krusty

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * The kotlinc request the plugin derives from a compile task's options, as KGP derives kotlinc's
 * arguments: what it refuses before asking krusty, and how free arguments follow the structured
 * options. The Gradle integration suite drives one case of each kind through a real build.
 */
class KotlincRequestTest {
    private val release = supportedKotlinVersions.last()

    /** The compiler slice's `compiler/util` options, which every case below starts from. */
    private val slice = CompilerOptionValues(
        languageVersion = "2.4",
        apiVersion = "2.4",
        optIn = listOf("krusty.fixture.ExperimentalFirstApi", "krusty.fixture.ExperimentalSecondApi"),
        freeCompilerArgs = listOf("-Xconsistent-data-class-copy-visibility", "-Xskip-prerelease-check"),
        moduleName = "kotlin-compiler-util",
        jvmTarget = "17",
        jvmDefault = "no-compatibility",
        javaParameters = true,
    )

    private val structured = listOf(
        "-language-version", "2.4",
        "-api-version", "2.4",
        "-opt-in=krusty.fixture.ExperimentalFirstApi",
        "-opt-in=krusty.fixture.ExperimentalSecondApi",
        "-module-name", "kotlin-compiler-util",
        "-jvm-target", "17",
        "-jvm-default", "no-compatibility",
        "-java-parameters",
    )

    private fun request(options: CompilerOptionValues, compilerVersion: String = release) =
        kotlincRequest(options, release, compilerVersion)

    private fun withFree(vararg free: String) = slice.copy(freeCompilerArgs = slice.freeCompilerArgs + free)

    private fun assertRefused(options: CompilerOptionValues, expected: String, compilerVersion: String = release) {
        val request = request(options, compilerVersion)
        assertEquals(listOf(expected), request.refusals)
        assertEquals(emptyList(), request.arguments)
    }

    private fun assertArguments(options: CompilerOptionValues, expected: List<String>) {
        val request = request(options)
        assertEquals(emptyList(), request.refusals)
        assertEquals(expected, request.arguments)
    }

    @Test
    fun aCompilerVersionOtherThanThePluginsIsLeftToKotlinc() {
        assertRefused(slice, "Kotlin compiler 2.4.0 differs from Kotlin Gradle plugin $release", "2.4.0")
        assertRefused(
            slice,
            "Kotlin compiler $release-dev-7885 differs from Kotlin Gradle plugin $release",
            "$release-dev-7885",
        )
    }

    @Test
    fun anUnsupportedStructuredOptionIsLeftToKotlinc() {
        assertRefused(slice.copy(extraWarnings = true), "krusty does not support compilerOptions.extraWarnings")
    }

    @Test
    fun anEmptyOptInMarkerIsLeftToKotlinc() {
        assertRefused(
            slice.copy(optIn = listOf("krusty.fixture.ExperimentalFirstApi", "")),
            "compilerOptions.optIn contains an empty marker",
        )
    }

    @Test
    fun aFreeArgumentReachingAPluginOwnedArgumentIsLeftToKotlinc() {
        for ((free, expected) in listOf(
            listOf("-d=forbidden") to
                "freeCompilerArg '-d=forbidden' conflicts with the plugin-owned destination; configure the structured Gradle input instead",
            listOf("-Xplugin=forbidden.jar") to
                "freeCompilerArg '-Xplugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
            listOf("--", "-d", "elsewhere") to
                "freeCompilerArg '--' would make the arguments after it sources; add sources to the task's source set instead",
            listOf("@overrides.args") to
                "freeCompilerArg '@overrides.args' is an argument file, which could hide plugin-owned arguments; pass its arguments as freeCompilerArgs instead",
            listOf("Extra.kt") to
                "freeCompilerArg 'Extra.kt' would be compiled as a source; add sources to the task's source set instead",
            listOf("-cp", "elsewhere.jar") to
                "freeCompilerArg '-cp' conflicts with the task libraries classpath; configure the structured Gradle input instead",
            listOf("-Xcompiler-plugin=forbidden.jar") to
                "freeCompilerArg '-Xcompiler-plugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
            listOf("-Xkotlin-reference-version=2.4.0") to
                "freeCompilerArg '-Xkotlin-reference-version=2.4.0' conflicts with the Kotlin Gradle plugin version; configure the structured Gradle input instead",
            listOf("-no-stdlib=false") to
                "freeCompilerArg '-no-stdlib=false' conflicts with the plugin-owned dependency policy; configure the structured Gradle input instead",
        )) {
            assertRefused(withFree(*free.toTypedArray()), expected)
        }
    }

    @Test
    fun freeArgumentsFollowTheStructuredOnes() {
        for (free in listOf(
            "-progressive",
            "-Xprogressive",
            "-module-name=overridden",
            "-jvm-default=disable",
            "-opt-in=krusty.fixture.ExperimentalFirstApi",
            "-Xwarning-level=REDUNDANT_CLI_ARG:disabled",
        )) {
            assertArguments(withFree(free), structured + slice.freeCompilerArgs + free)
        }
    }

    @Test
    fun progressiveModeIsKotlincsProgressiveArgument() {
        assertArguments(
            slice.copy(progressiveMode = true),
            structured.dropLast(1) + "-progressive" + "-java-parameters" + slice.freeCompilerArgs,
        )
    }

    @Test
    fun aRepeatedOptInMarkerIsPassedAsOftenAsItIsListed() {
        val markers = listOf(
            "krusty.fixture.ExperimentalFirstApi",
            "krusty.fixture.ExperimentalSecondApi",
            "krusty.fixture.ExperimentalFirstApi",
        )
        assertArguments(
            slice.copy(optIn = markers),
            listOf("-language-version", "2.4", "-api-version", "2.4") +
                markers.map { "-opt-in=$it" } +
                structured.drop(6) +
                slice.freeCompilerArgs,
        )
    }

    @Test
    fun allWarningsAsErrorsAndAFreeWerrorAreOneWarningPolicy() {
        assertArguments(slice.copy(allWarningsAsErrors = true), structured + "-Werror" + slice.freeCompilerArgs)
        assertArguments(withFree("-Werror"), structured + slice.freeCompilerArgs + "-Werror")
        assertArguments(
            withFree("-Werror").copy(allWarningsAsErrors = true),
            structured + slice.freeCompilerArgs + "-Werror",
        )
        assertArguments(
            withFree("-Xcontext-parameters", "-Xwarning-level=REDUNDANT_CLI_ARG:warning")
                .copy(allWarningsAsErrors = true),
            structured + "-Werror" + slice.freeCompilerArgs +
                listOf("-Xcontext-parameters", "-Xwarning-level=REDUNDANT_CLI_ARG:warning"),
        )
    }

    @Test
    fun theStructuredLanguageVersionReachesTheCompilerUnchanged() {
        assertArguments(
            slice.copy(languageVersion = "2.2", apiVersion = "2.2"),
            listOf("-language-version", "2.2", "-api-version", "2.2") + structured.drop(4) + slice.freeCompilerArgs,
        )
    }
}
