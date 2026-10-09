package krusty

import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.JavaVersion
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.Property
import org.gradle.api.plugins.JavaPluginExtension
import org.gradle.api.tasks.Classpath
import org.gradle.api.tasks.CacheableTask
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.compile.JavaCompile
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.api.tasks.TaskProvider
import org.gradle.jvm.toolchain.JavaToolchainService
import org.gradle.process.ExecOperations
import org.jetbrains.kotlin.buildtools.api.ExperimentalBuildToolsApi
import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.dsl.KotlinTopLevelExtension
import org.jetbrains.kotlin.gradle.plugin.CompilerPluginConfig
import org.jetbrains.kotlin.gradle.plugin.KotlinBasePlugin
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import java.io.File
import javax.inject.Inject

/**
 * Replaces each Kotlin/JVM compile action with a separate, cacheable Gradle task which invokes
 * krusty. The Kotlin Gradle plugin remains the owner of source discovery, dependency ordering,
 * classpaths, friend paths and compiler options.
 *
 * A dirty task always compiles the complete source set into a clean output directory. Gradle owns
 * task incrementality; krusty does not try to infer Kotlin dependency invalidation from class-file
 * names or partial ABI snapshots.
 */
abstract class KrustyKotlinPlugin @Inject constructor(
    private val javaToolchains: JavaToolchainService,
) : Plugin<Project> {
    override fun apply(project: Project) {
        if (!project.pluginManager.hasPlugin("org.jetbrains.kotlin.jvm")) {
            throw GradleException(
                "the krusty plugin must be applied after org.jetbrains.kotlin.jvm",
            )
        }
        val aggregate = aggregateTask(project.rootProject)
        // Wire replacements at the end of project configuration, not during plugin application.
        // Realizing a KotlinJvmCompile while a convention plugin is mid-apply reads — and so
        // finalizes — KotlinTopLevelExtension.compilerVersion before that convention sets it (the
        // Kotlin repository's gradle-plugin-common-configuration sets compilerVersion after
        // applying the Kotlin JVM plugin), and Gradle forbids registering a task from inside a
        // task configuration callback.
        project.afterEvaluate {
            replaceKotlinJvmCompiles(project, javaToolchains, aggregate)
        }
    }
}

private const val AGGREGATE_TASK_PROPERTY = "krusty.aggregateTaskProvider"

private const val KOTLIN_DSL_BASE_PLUGIN_ID = "org.gradle.kotlin.kotlin-dsl.base"

private fun aggregateTask(root: Project): TaskProvider<Task> {
    val extra = root.extensions.extraProperties
    if (extra.has(AGGREGATE_TASK_PROPERTY)) {
        return root.tasks.named("krustyCompile")
    }
    return root.tasks.register("krustyCompile") {
        group = "build"
        description = "Compiles every Kotlin/JVM source set owned by the krusty plugin"
    }.also {
        extra.set(AGGREGATE_TASK_PROPERTY, true)
    }
}

@OptIn(ExperimentalBuildToolsApi::class, ExperimentalKotlinGradlePluginApi::class)
private fun replaceKotlinJvmCompiles(
    project: Project,
    javaToolchains: JavaToolchainService,
    aggregate: TaskProvider<Task>,
) {
    // Gradle's kotlin-dsl (applied through `kotlin-dsl` or `kotlin-dsl.base`) owns every Kotlin
    // compilation of its project: precompiled script plugins are `.gradle.kts` scripts compiled
    // against the Gradle script templates, the SAM-with-receiver and assignment compiler plugins
    // change call resolution, and its compiler settings add arguments krusty does not model. A
    // replacement would drop the scripts or reject the build, so kotlinc keeps those compiles.
    val kotlinDsl = project.pluginManager.hasPlugin(KOTLIN_DSL_BASE_PLUGIN_ID)
    project.tasks.withType(KotlinJvmCompile::class.java).all {
        val kotlinTask = this
        // A KotlinJvmCompile outside every source-set compilation is not a krusty compile either:
        // kotlin-dsl's `compilePluginsBlocks` compiles the `plugins {}` blocks it extracts from
        // precompiled script plugins as scripts, with no source set, Java sibling or classes output.
        val sourceSetName = kotlinTask.sourceSetName.orNull
        val kotlincReason = when {
            sourceSetName == null -> "it belongs to no source set"
            kotlinDsl -> "its project applies kotlin-dsl"
            else -> null
        }
        if (kotlincReason != null) {
            project.logger.lifecycle(
                "krusty: {} is left to kotlinc: {}",
                kotlinTask,
                kotlincReason,
            )
            return@all
        }
        val replacementName = "${kotlinTask.name}WithKrusty"
        val pluginVersion = project.provider {
            project.plugins.withType(KotlinBasePlugin::class.java).single().pluginVersion
        }
        val replacement = project.tasks.register(replacementName, KrustyCompileTask::class.java) {
            group = kotlinTask.group ?: "build"
            description = "${kotlinTask.description ?: kotlinTask.name}, using krusty"
            sourceFiles.from(
                kotlinTask.sources.asFileTree.matching { include("**/*.kt") },
            )
            libraries.from(kotlinTask.libraries)
            friendPaths.from(kotlinTask.friendPaths)
            destinationDirectory.set(kotlinTask.destinationDirectory)
            compilerArguments.set(project.provider {
                compilerArguments(kotlinTask)
            })
            kotlinTarget.set(kotlinTask.compilerOptions.jvmTarget.map { it.target })
            targetValidationMode.set(kotlinTask.jvmTargetValidationMode.map { it.name })
            kotlinPluginVersion.set(pluginVersion)
            // The compiler-plugin request kotlinc receives from this task: KGP's resolved plugin
            // classpath (`-Xplugin`) and its per-plugin options (`-P plugin:<id>:<key>=<value>`).
            compilerPluginClasspath.from(kotlinTask.pluginClasspath)
            compilerPluginOptions.set(kotlinTask.pluginOptions.map(::pluginOptionArguments))
            compilerVersion.set(
                project.extensions.getByType(KotlinTopLevelExtension::class.java)
                    .compilerVersion.orElse(pluginVersion),
            )
            val binaryPath = project.providers.gradleProperty("krusty.binary")
                .orElse(project.providers.environmentVariable("KRUSTY_BIN"))
            binary.set(
                project.layout.file(binaryPath.map(::File)),
            )
        }
        aggregate.configure { dependsOn(replacement) }
        project.pluginManager.withPlugin("java-base") {
            val java = project.extensions.getByType(JavaPluginExtension::class.java)
            val launcher = javaToolchains.launcherFor(java.toolchain)
            val selectedJdkHome = launcher
                .map { it.metadata.installationPath.asFile.absolutePath }
                .orElse(project.providers.systemProperty("java.home"))
            val selectedJavaVersion = launcher
                .map { it.metadata.languageVersion.asInt().toString() }
                .orElse(project.provider { JavaVersion.current().majorVersion })
            replacement.configure {
                jdkHome.set(selectedJdkHome)
                javaVersion.set(selectedJavaVersion)
                kotlinJavaVersion.set(
                    kotlinTask.kotlinJavaToolchainProvider
                        .flatMap { it.javaVersion }
                        .map { it.majorVersion }
                        .orElse(selectedJavaVersion),
                )
            }
            java.sourceSets.configureEach {
                val sourceSet = this
                if (sourceSet.name == sourceSetName) {
                    val javaCompile = project.tasks.named(sourceSet.compileJavaTaskName, JavaCompile::class.java)
                    replacement.configure {
                        javaSourceFiles.from(sourceSet.allJava)
                        javaTarget.set(
                            javaCompile.flatMap { task ->
                                task.options.release.map { it.toString() }
                                    .orElse(project.provider { task.targetCompatibility })
                            },
                        )
                    }
                }
            }
        }

        // Preserve the public task graph: callers and downstream tasks can still depend on
        // compileKotlin/compileTestKotlin. A disabled task runs its dependencies but none of its own
        // compiler actions, so no private action list is mutated.
        kotlinTask.dependsOn(replacement)
        kotlinTask.enabled = false
    }
}

@CacheableTask
abstract class KrustyCompileTask @Inject constructor(
    private val execOperations: ExecOperations,
    private val fileSystemOperations: FileSystemOperations,
) : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sourceFiles: ConfigurableFileCollection

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val javaSourceFiles: ConfigurableFileCollection

    @get:Classpath
    abstract val libraries: ConfigurableFileCollection

    @get:Classpath
    abstract val friendPaths: ConfigurableFileCollection

    @get:Input
    abstract val compilerArguments: ListProperty<String>

    @get:Input
    abstract val kotlinPluginVersion: Property<String>

    @get:Input
    abstract val compilerVersion: Property<String>

    @get:Classpath
    abstract val compilerPluginClasspath: ConfigurableFileCollection

    @get:Input
    abstract val compilerPluginOptions: ListProperty<String>

    @get:Input
    abstract val jdkHome: Property<String>

    @get:Input
    abstract val javaVersion: Property<String>

    @get:Input
    abstract val kotlinJavaVersion: Property<String>

    @get:Input
    abstract val kotlinTarget: Property<String>

    @get:Input
    abstract val javaTarget: Property<String>

    @get:Input
    abstract val targetValidationMode: Property<String>

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val binary: RegularFileProperty

    @get:OutputDirectory
    abstract val destinationDirectory: DirectoryProperty

    @TaskAction
    fun compile() {
        validateJvmTargets(kotlinTarget.get(), javaTarget.get(), targetValidationMode.get()) {
            logger.warn(it)
        }
        val kotlinSources = sourceFiles.asFileTree.files.sortedBy(File::getAbsolutePath)
        val unsupported = kotlinSources.filter { it.extension != "kt" }
        if (unsupported.isNotEmpty()) {
            throw GradleException(
                "unsupported Kotlin source input(s): ${unsupported.joinToString { it.path }}",
            )
        }
        val javaSources = javaSourceFiles.asFileTree.files.filter { it.extension == "java" }
        val sources = (kotlinSources + javaSources).distinct().sortedBy(File::getAbsolutePath)
        val destination = destinationDirectory.get().asFile

        val arguments = ArrayList<String>()
        arguments.addAll(sources.map(File::getAbsolutePath))
        if (!libraries.isEmpty) {
            arguments.add("-classpath")
            arguments.add(libraries.files.joinToString(File.pathSeparator, transform = File::getAbsolutePath))
        }
        if (!friendPaths.isEmpty) {
            // kotlinc splits friend paths on `,`, not on the path separator.
            arguments.add("-Xfriend-paths=" + friendPaths.files.joinToString(",", transform = File::getAbsolutePath))
        }
        arguments.addAll(compilerArguments.get())
        if (javaVersion.get() != kotlinJavaVersion.get()) {
            throw GradleException(
                "Kotlin task Java ${kotlinJavaVersion.get()} differs from Gradle Java toolchain ${javaVersion.get()}",
            )
        }
        val pluginVersion = supportedKotlinPluginVersion(kotlinPluginVersion.get())
        if (compilerVersion.get() != pluginVersion) {
            throw GradleException(
                "Kotlin compiler ${compilerVersion.get()} differs from Kotlin Gradle plugin $pluginVersion",
            )
        }
        if (!compilerPluginClasspath.isEmpty) {
            arguments.add(
                "-Xplugin=" + compilerPluginClasspath.files.joinToString(",", transform = File::getAbsolutePath),
            )
        }
        compilerPluginOptions.get().forEach { option ->
            arguments.add("-P")
            arguments.add(option)
        }
        arguments.add("-Xkotlin-reference-version=$pluginVersion")
        if ("-no-jdk" !in arguments) {
            arguments.add("-jdk-home")
            arguments.add(jdkHome.get())
        }
        arguments.add("-no-stdlib")
        arguments.add("-no-reflect")
        arguments.add("-d")
        arguments.add(destination.absolutePath)

        // A source removal and changes to multifile facades or module metadata must not leave any
        // product of the previous full invocation behind. Validate every option before mutating it.
        fileSystemOperations.delete { delete(destination) }
        destination.mkdirs()
        if (sources.isEmpty()) return

        val executable = binary.get().asFile.absolutePath
        val result = execOperations.exec {
            this.executable = executable
            args(arguments)
            isIgnoreExitValue = true
        }
        if (result.exitValue != 0) {
            throw GradleException("$executable exited with ${result.exitValue} for $path")
        }
    }
}

private fun pluginOptionArguments(configs: List<CompilerPluginConfig>): List<String> =
    configs.flatMap { config ->
        config.allOptions().flatMap { (pluginId, options) ->
            options.map { option -> "plugin:$pluginId:${option.key}=${option.value}" }
        }
    }

private fun validateJvmTargets(kotlin: String, java: String, mode: String, warn: (String) -> Unit) {
    fun normalized(value: String): String = value.removePrefix("1.")
    if (normalized(kotlin) == normalized(java) || mode == "IGNORE") return
    val message = "Kotlin JVM target $kotlin differs from Java target $java"
    if (mode == "WARNING") {
        warn(message)
    } else {
        throw GradleException(message)
    }
}

private fun compilerArguments(task: KotlinJvmCompile): List<String> {
    val options = task.compilerOptions
    fun reject(condition: Boolean, name: String) {
        if (condition) throw GradleException("krusty does not support compilerOptions.$name")
    }
    val languageVersion = options.languageVersion.orNull?.version
    val apiVersion = options.apiVersion.orNull?.version
    reject(options.extraWarnings.getOrElse(false), "extraWarnings")
    reject(options.suppressWarnings.getOrElse(false), "suppressWarnings")
    // `-Werror` is not yet modeled, so its structured equivalent stays rejected. Named
    // `-Xwarning-level` policy is forwarded to the compiler, whose diagnostic registry is the
    // authoritative place to validate names and apply severities.
    reject(options.allWarningsAsErrors.getOrElse(false), "allWarningsAsErrors")
    reject(options.verbose.getOrElse(false), "verbose")
    reject(task.multiPlatformEnabled.getOrElse(false), "multiPlatformEnabled")
    reject(task.useModuleDetection.getOrElse(false), "useModuleDetection")

    val structuredOptIns = validateStructuredOptIns(options.optIn.getOrElse(emptyList()))
    val freeArguments = options.freeCompilerArgs.getOrElse(emptyList())
    val arguments = validateFreeArguments(freeArguments)
    structuredOptIns.firstOrNull { marker -> "-opt-in=$marker" in freeArguments }?.let { marker ->
        throw GradleException(
            "compilerOptions.optIn and freeCompilerArg '-opt-in=$marker' both request marker '$marker'; configure exactly one",
        )
    }
    freeArguments.firstOrNull(::isFreeJvmDefault)?.let { free ->
        if (options.jvmDefault.orNull != null) {
            throw GradleException(
                "compilerOptions.jvmDefault and freeCompilerArg '$free' are both set; configure exactly one",
            )
        }
    }
    languageVersion?.let { arguments.addPair("-language-version", it) }
    apiVersion?.let { arguments.addPair("-api-version", it) }
    structuredOptIns.forEach { marker ->
        arguments.add("-opt-in=$marker")
    }
    options.moduleName.orNull?.takeIf(String::isNotEmpty)?.let {
        arguments.addPair("-module-name", it)
    }
    options.jvmTarget.orNull?.let { arguments.addPair("-jvm-target", it.target) }
    options.jvmDefault.orNull?.let { arguments.addPair("-jvm-default", it.compilerArgument) }
    if (options.progressiveMode.getOrElse(false)) arguments.add("-progressive")
    if (options.javaParameters.getOrElse(false)) arguments.add("-java-parameters")
    if (options.noJdk.getOrElse(false)) arguments.add("-no-jdk")
    return arguments
}

private fun isFreeJvmDefault(argument: String): Boolean =
    argument == "-jvm-default" || argument == "-Xjvm-default" ||
        argument.startsWith("-jvm-default=") || argument.startsWith("-Xjvm-default=")

private fun validateStructuredOptIns(markers: List<String>): List<String> {
    val seen = HashSet<String>()
    for (marker in markers) {
        if (marker.isBlank()) {
            throw GradleException("compilerOptions.optIn contains an empty marker")
        }
        if (!seen.add(marker)) {
            throw GradleException("compilerOptions.optIn contains duplicate marker '$marker'")
        }
    }
    return markers
}

private fun supportedKotlinPluginVersion(version: String): String = when (version) {
    "2.4.0", "2.4.10", "2.4.20" -> version
    else -> throw GradleException("unsupported Kotlin Gradle plugin $version; expected 2.4.0, 2.4.10, or 2.4.20")
}

private fun MutableList<String>.addPair(name: String, value: String) {
    add(name)
    add(value)
}
