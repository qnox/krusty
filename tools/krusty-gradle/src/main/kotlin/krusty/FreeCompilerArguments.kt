package krusty

import org.gradle.api.GradleException

// Free arguments reach krusty verbatim, as KGP passes them to kotlinc: krusty parses them with
// kotlinc's argument table, reports kotlinc's errors and warnings, and refuses an argument it does
// not implement. What this plugin checks is only the boundary of the arguments it derives from
// structured task inputs. A free argument must not reach them under another spelling (a short or
// deprecated alias), hide them in an argument file, or turn them into sources after `--`.

/** A kotlinc argument's canonical name, and whether a bare occurrence consumes the next token. */
private class KotlincArgument(val name: String, val takesValue: Boolean)

/**
 * Every spelling (name, short name, deprecated name) that a supported kotlinc release declares,
 * read from the release tables the compiler itself vendors. A spelling is reserved if any
 * supported release gives it to a reserved argument.
 */
private val kotlincArguments: Map<String, KotlincArgument> by lazy {
    val resource = "/krusty/kotlinc-arguments.tsv"
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
    arguments
}

/** The structured input that owns each argument the plugin derives, by canonical name. */
private val reservedArguments: Map<String, String> = mapOf(
    "-d" to "the plugin-owned destination",
    "-Xbuild-file" to "the task sources and the plugin-owned destination",
    "-classpath" to "the task libraries classpath",
    "-Xfriend-paths" to "the task friend paths",
    "-module-name" to "compilerOptions.moduleName",
    "-jvm-target" to "compilerOptions.jvmTarget",
    "-java-parameters" to "compilerOptions.javaParameters",
    "-jdk-home" to "compilerOptions.noJdk and the Java toolchain",
    "-no-jdk" to "compilerOptions.noJdk and the Java toolchain",
    "-no-stdlib" to "the plugin-owned dependency policy",
    "-no-reflect" to "the plugin-owned dependency policy",
    "-kotlin-home" to "the plugin-owned dependency policy",
    // krusty's own argument, not in kotlinc's table.
    "-Xkotlin-reference-version" to "the Kotlin Gradle plugin version",
    "-language-version" to "compilerOptions.languageVersion",
    "-api-version" to "compilerOptions.apiVersion",
    "-progressive" to "compilerOptions.progressiveMode",
    "-Xplugin" to "compiler plugin configuration",
    "-P" to "compiler plugin configuration",
    "-Xcompiler-plugin" to "compiler plugin configuration",
    "-Xcompiler-plugin-order" to "compiler plugin configuration",
)

/**
 * Walk the free arguments the way kotlinc's parser does and refuse any that would reach a
 * plugin-owned argument. kotlinc expands an `@argfile` wherever it appears, before parsing, so a
 * token starting with `@` is refused in any position.
 */
internal fun validateFreeArguments(input: List<String>): ArrayList<String> {
    input.firstOrNull { it.startsWith("@") }?.let { argument ->
        throw GradleException(
            "freeCompilerArg '$argument' is an argument file, which could hide plugin-owned arguments; pass its arguments as freeCompilerArgs instead",
        )
    }
    var index = 0
    while (index < input.size) {
        val argument = input[index++]
        if (argument == "--") {
            throw GradleException(
                "freeCompilerArg '--' would make the arguments after it sources; add sources to the task's source set instead",
            )
        }
        val key = argument.substringBefore(if (argument.startsWith("-XXLanguage")) ':' else '=')
        val spec = kotlincArguments[key]
        reservedArguments[spec?.name ?: key]?.let { owner ->
            throw GradleException(
                "freeCompilerArg '$argument' conflicts with $owner; configure the structured Gradle input instead",
            )
        }
        if (spec == null) {
            if (!argument.startsWith("-")) {
                throw GradleException(
                    "freeCompilerArg '$argument' would be compiled as a source; add sources to the task's source set instead",
                )
            }
        } else if (spec.takesValue && key == argument) {
            // The next token is this argument's value, whatever it looks like.
            index++
        }
    }
    return ArrayList(input)
}
