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
import org.gradle.api.tasks.OutputFile
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
import org.jetbrains.kotlin.gradle.tasks.KaptGenerateStubs
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import java.io.ByteArrayOutputStream
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
 *
 * A compile that requests a compiler plugin (Compose, Parcelize, any third-party FIR/IR plugin) or
 * a kotlinc argument krusty does not implement stays with kotlinc: a check task asks the krusty
 * binary what of the request it refuses, reports it, and then the Kotlin task runs instead of its
 * replacement. kapt's stub generation is never replaced; see [replaceKotlinJvmCompiles].
 */
abstract class KrustyKotlinPlugin @Inject constructor(
    private val javaToolchains: JavaToolchainService,
) : Plugin<Project> {
    override fun apply(project: Project) {
        if (KOTLIN_PLUGIN_IDS.none(project.pluginManager::hasPlugin)) {
            throw GradleException(
                "the krusty plugin must be applied after one of ${KOTLIN_PLUGIN_IDS.joinToString()}",
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

/**
 * The Android Gradle plugin's base plugin. From AGP 9 it compiles Kotlin itself (built-in Kotlin,
 * through the Kotlin Gradle plugin's API), without `org.jetbrains.kotlin.android`.
 */
private const val ANDROID_BASE_PLUGIN_ID = "com.android.base"

/** The plugins whose Kotlin/JVM compile tasks krusty replaces; one must be applied before krusty. */
private val KOTLIN_PLUGIN_IDS = listOf("org.jetbrains.kotlin.jvm", "org.jetbrains.kotlin.android", ANDROID_BASE_PLUGIN_ID)

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
    val android = project.pluginManager.hasPlugin(ANDROID_BASE_PLUGIN_ID)
    project.tasks.withType(KotlinJvmCompile::class.java).all {
        val kotlinTask = this
        // A KotlinJvmCompile outside every source-set compilation is not a krusty compile either:
        // kotlin-dsl's `compilePluginsBlocks` compiles the `plugins {}` blocks it extracts from
        // precompiled script plugins as scripts, with no source set, Java sibling or classes output.
        // kapt's stub generation is a KotlinJvmCompile too, but it runs kotlinc with the kapt
        // plugin to write Java stubs, not classes; the compile after annotation processing is the
        // krusty compile, and a replacement here would depend on the processing that needs the stubs.
        val sourceSetName = kotlinTask.sourceSetName.orNull
        val kotlincReason = when {
            kotlinTask is KaptGenerateStubs -> "it generates kapt stubs"
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
        val binaryPath = project.providers.gradleProperty("krusty.binary")
            .orElse(project.providers.environmentVariable("KRUSTY_BIN"))
        val compilerPluginArguments = kotlinTask.pluginOptions.map(::pluginOptionArguments)
        val kotlincArguments = pluginVersion.map { version ->
            compilerArguments(kotlinTask, supportedKotlinPluginVersion(version))
        }
        val compilerChoice = project.layout.buildDirectory
            .file("krusty/${kotlinTask.name}/refusals.txt").get().asFile
        val supportCheck = project.tasks.register(
            "${kotlinTask.name}KrustyCheck",
            KrustySupportCheckTask::class.java,
        ) {
            description = "Checks whether krusty implements everything ${kotlinTask.path} requests"
            compileTaskPath.set(kotlinTask.path)
            compilerArguments.set(kotlincArguments)
            compilerPluginClasspath.from(kotlinTask.pluginClasspath)
            compilerPluginOptions.set(compilerPluginArguments)
            kotlinPluginVersion.set(pluginVersion)
            binary.set(project.layout.file(binaryPath.map(::File)))
            refusals.set(compilerChoice)
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
            compilerArguments.set(kotlincArguments)
            kotlinTarget.set(kotlinTask.compilerOptions.jvmTarget.map { it.target })
            targetValidationMode.set(kotlinTask.jvmTargetValidationMode.map { it.name })
            kotlinPluginVersion.set(pluginVersion)
            // The compiler-plugin request kotlinc receives from this task: KGP's resolved plugin
            // classpath (`-Xplugin`) and its per-plugin options (`-P plugin:<id>:<key>=<value>`).
            compilerPluginClasspath.from(kotlinTask.pluginClasspath)
            compilerPluginOptions.set(compilerPluginArguments)
            compilerVersion.set(
                project.extensions.getByType(KotlinTopLevelExtension::class.java)
                    .compilerVersion.orElse(pluginVersion),
            )
            binary.set(project.layout.file(binaryPath.map(::File)))
            // A build that adds a source directory by path wires its generator onto the Kotlin
            // task (`compileKotlin { dependsOn(generator) }`), not onto the directory. The compile
            // runs here now, so it needs those dependencies; the Kotlin task's dependency on this
            // replacement is the one it must not take back.
            dependsOn(
                project.provider {
                    kotlinTask.dependsOn.filterNot { it is TaskProvider<*> && it.name == replacementName }
                },
            )
            dependsOn(supportCheck)
            onlyIf("krusty implements everything the compile requests") { !compilerChoice.refuses() }
        }
        // The aggregate reaches the replacement through the Kotlin task, which depends on it: a
        // compile left to kotlinc runs only as that task's own action.
        aggregate.configure { dependsOn(kotlinTask) }
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
            if (android) {
                // An Android variant is no Java source set. kotlinc reads the Java sources the
                // variant's javac task compiles, generated ones (BuildConfig, AIDL) included; that
                // task is named after the variant.
                val variant = requireNotNull(sourceSetName)
                val javaCompileName = "compile${variant.replaceFirstChar(Char::uppercaseChar)}JavaWithJavac"
                val javaCompile = project.provider {
                    project.tasks.findByName(javaCompileName) as? JavaCompile
                        ?: throw GradleException("${kotlinTask.path} has no Android Java compile task $javaCompileName")
                }
                replacement.configure {
                    javaSourceFiles.from(javaCompile.map { it.source })
                    javaTarget.set(
                        javaCompile.flatMap { task ->
                            task.options.release.map { it.toString() }
                                .orElse(project.provider { task.targetCompatibility })
                        },
                    )
                }
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
        // compileKotlin/compileTestKotlin. A skipped task runs its dependencies but none of its own
        // compiler actions, so no private action list is mutated. kotlinc runs only for a compile
        // whose request krusty refused.
        kotlinTask.dependsOn(replacement)
        kotlinTask.onlyIf("krusty refused part of the compile's request") { compilerChoice.refuses() }
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

/**
 * Asks the krusty binary which of a compile's kotlinc arguments and requested compiler plugins it
 * refuses, and records them, one per line, in [refusals]: an empty file means krusty compiles the
 * request itself. The binary's argument dispositions and plugin registry are the single place that
 * knows what krusty implements.
 */
abstract class KrustySupportCheckTask @Inject constructor(
    private val execOperations: ExecOperations,
) : DefaultTask() {
    @get:Input
    abstract val compileTaskPath: Property<String>

    @get:Input
    abstract val compilerArguments: ListProperty<String>

    @get:Classpath
    abstract val compilerPluginClasspath: ConfigurableFileCollection

    @get:Input
    abstract val compilerPluginOptions: ListProperty<String>

    @get:Input
    abstract val kotlinPluginVersion: Property<String>

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val binary: RegularFileProperty

    @get:OutputFile
    abstract val refusals: RegularFileProperty

    @TaskAction
    fun check() {
        val arguments = ArrayList<String>()
        arguments.add("refusals")
        arguments.addAll(compilerArguments.get())
        arguments.add("-Xkotlin-reference-version=${supportedKotlinPluginVersion(kotlinPluginVersion.get())}")
        if (!compilerPluginClasspath.isEmpty) {
            arguments.add(
                "-Xplugin=" + compilerPluginClasspath.files.joinToString(",", transform = File::getAbsolutePath),
            )
        }
        compilerPluginOptions.get().forEach { option ->
            arguments.add("-P")
            arguments.add(option)
        }
        val executable = binary.get().asFile.absolutePath
        val output = ByteArrayOutputStream()
        val errors = ByteArrayOutputStream()
        val result = execOperations.exec {
            this.executable = executable
            args(arguments)
            standardOutput = output
            errorOutput = errors
            isIgnoreExitValue = true
        }
        val refused = output.toString(Charsets.UTF_8).lines().filter(String::isNotBlank)
        when (result.exitValue) {
            0 -> refusals.get().asFile.writeText("")
            1 -> {
                logger.warn(
                    "krusty: task '{}' is left to kotlinc: krusty does not implement everything it requests\n{}",
                    compileTaskPath.get(),
                    refused.joinToString("\n") { "  $it" },
                )
                refusals.get().asFile.writeText(refused.joinToString("\n", postfix = "\n"))
            }
            // A command line kotlinc itself rejects is no refusal: the build fails with the
            // errors the compilation would report, and neither compiler runs.
            2 -> throw GradleException(errors.toString(Charsets.UTF_8).trimEnd())
            else -> throw GradleException("$executable exited with ${result.exitValue} for $path")
        }
    }
}

private fun File.refuses(): Boolean = length() > 0

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

private fun compilerArguments(task: KotlinJvmCompile, kotlinVersion: String): List<String> {
    val options = task.compilerOptions
    fun reject(condition: Boolean, name: String) {
        if (condition) throw GradleException("krusty does not support compilerOptions.$name")
    }
    val languageVersion = options.languageVersion.orNull?.version
    val apiVersion = options.apiVersion.orNull?.version
    reject(options.extraWarnings.getOrElse(false), "extraWarnings")
    reject(options.suppressWarnings.getOrElse(false), "suppressWarnings")
    // Named `-Xwarning-level` policy is forwarded to the compiler, whose diagnostic registry is
    // the authoritative place to validate names and apply severities. `-Werror` is the global
    // form of that policy: the compiler fails the compilation when any warning is emitted.
    reject(options.verbose.getOrElse(false), "verbose")
    reject(task.multiPlatformEnabled.getOrElse(false), "multiPlatformEnabled")
    // `useModuleDetection` (set on every Android compile) only configures kotlinc's multi-module
    // incremental compilation, never a compiler argument; a full krusty compile has nothing to detect.

    val structuredOptIns = validateStructuredOptIns(options.optIn.getOrElse(emptyList()))
    val freeArguments = options.freeCompilerArgs.getOrElse(emptyList())
    val arguments = validateFreeArguments(freeArguments, kotlinVersion)
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
    if (options.allWarningsAsErrors.getOrElse(false) && "-Werror" !in freeArguments) {
        arguments.add("-Werror")
    }
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

private fun supportedKotlinPluginVersion(version: String): String =
    if (version in supportedKotlinVersions) {
        version
    } else {
        val expected = supportedKotlinVersions.dropLast(1).joinToString(", ") +
            ", or " + supportedKotlinVersions.last()
        throw GradleException("unsupported Kotlin Gradle plugin $version; expected $expected")
    }

private fun MutableList<String>.addPair(name: String, value: String) {
    add(name)
    add(value)
}
