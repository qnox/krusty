package krusty

import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.JavaVersion
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileCollection
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
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.compile.JavaCompile
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.OutputFile
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.api.tasks.TaskProvider
import org.gradle.build.event.BuildEventsListenerRegistry
import org.gradle.jvm.toolchain.JavaToolchainService
import org.gradle.process.ExecOperations
import org.gradle.work.DisableCachingByDefault
import org.jetbrains.kotlin.buildtools.api.ExperimentalBuildToolsApi
import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.dsl.KotlinTopLevelExtension
import org.jetbrains.kotlin.gradle.plugin.CompilerPluginConfig
import org.jetbrains.kotlin.gradle.plugin.KotlinBasePlugin
import org.jetbrains.kotlin.gradle.plugin.KotlinCompilation
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType
import org.jetbrains.kotlin.gradle.plugin.KotlinTargetsContainer
import org.jetbrains.kotlin.gradle.tasks.KaptGenerateStubs
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.OutputStream
import javax.inject.Inject

/**
 * Replaces each Kotlin/JVM compile action with a separate, cacheable Gradle task which invokes
 * krusty. The Kotlin Gradle plugin remains the owner of source discovery, dependency ordering,
 * classpaths, friend paths and compiler options. In a Kotlin Multiplatform project the compile
 * actions of its JVM targets are replaced, and each compilation's source sets reach krusty as
 * kotlinc's fragments: one per source set, refining the source sets it depends on.
 *
 * A dirty task always compiles the complete source set into a clean output directory. Gradle owns
 * task incrementality. A compile is keyed on its classpath through `krusty classpath-fingerprint`,
 * krusty's own reading of what a dependent compilation observes of each entry, not on the
 * classpath's bytes: a body-only edit in a dependency leaves its dependents up to date, and every
 * edit a dependent could observe still recompiles it. krusty does not infer Kotlin dependency
 * invalidation from class-file names or partial ABI snapshots.
 *
 * A compile that requests a compiler plugin (Compose, Parcelize, any third-party FIR/IR plugin) or
 * a kotlinc argument krusty does not implement stays with kotlinc: a check task asks the krusty
 * binary what of the request it refuses, reports it, and then the Kotlin task runs instead of its
 * replacement. So does a request this plugin cannot derive krusty's arguments from, such as a free
 * argument that reaches a plugin-owned one. kapt's stub generation is never replaced; see
 * [replaceKotlinJvmCompiles].
 */
abstract class KrustyKotlinPlugin @Inject constructor(
    private val javaToolchains: JavaToolchainService,
    private val buildEvents: BuildEventsListenerRegistry,
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
            replaceKotlinJvmCompiles(project, javaToolchains, buildEvents, aggregate)
        }
    }
}

private const val AGGREGATE_TASK_PROPERTY = "krusty.aggregateTaskProvider"

/** The Gradle property that asks krusty to fix kotlinc's defects, and the argument it passes. */
internal const val FIX_KOTLINC_DEFECTS_PROPERTY = "krusty.fixKotlincDefects"

internal const val FIX_KOTLINC_DEFECTS_ARGUMENT = "-Xfix-kotlinc-defects"

private const val KOTLIN_JVM_PLUGIN_ID = "org.jetbrains.kotlin.jvm"

private const val KOTLIN_MULTIPLATFORM_PLUGIN_ID = "org.jetbrains.kotlin.multiplatform"

private const val KOTLIN_DSL_BASE_PLUGIN_ID = "org.gradle.kotlin.kotlin-dsl.base"

/**
 * The Android Gradle plugin's base plugin. From AGP 9 it compiles Kotlin itself (built-in Kotlin,
 * through the Kotlin Gradle plugin's API), without `org.jetbrains.kotlin.android`.
 */
private const val ANDROID_BASE_PLUGIN_ID = "com.android.base"

/** The plugins whose Kotlin/JVM compile tasks krusty replaces; one must be applied before krusty. */
private val KOTLIN_PLUGIN_IDS = listOf(
    KOTLIN_JVM_PLUGIN_ID,
    KOTLIN_MULTIPLATFORM_PLUGIN_ID,
    "org.jetbrains.kotlin.android",
    ANDROID_BASE_PLUGIN_ID,
)

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
    buildEvents: BuildEventsListenerRegistry,
    aggregate: TaskProvider<Task>,
) {
    // Gradle's kotlin-dsl (applied through `kotlin-dsl` or `kotlin-dsl.base`) owns every Kotlin
    // compilation of its project: precompiled script plugins are `.gradle.kts` scripts compiled
    // against the Gradle script templates, the SAM-with-receiver and assignment compiler plugins
    // change call resolution, and its compiler settings add arguments krusty does not model. A
    // replacement would drop the scripts or reject the build, so kotlinc keeps those compiles.
    val kotlinDsl = project.pluginManager.hasPlugin(KOTLIN_DSL_BASE_PLUGIN_ID)
    val multiplatform = project.pluginManager.hasPlugin(KOTLIN_MULTIPLATFORM_PLUGIN_ID)
    val android = project.pluginManager.hasPlugin(ANDROID_BASE_PLUGIN_ID)
    // Without an explicit binary, the compiler of the release this plugin was published with.
    val gradleUserHome = project.gradle.gradleUserHomeDir
    val offline = project.gradle.startParameter.isOffline
    val releases = project.providers.gradleProperty("krusty.releases").orElse(GITHUB_RELEASES)
    val releasedBinary = releases.map { releases ->
        val release = pluginRelease ?: throw GradleException(
            "this krusty plugin build has no release version; set -Pkrusty.binary=<path> or KRUSTY_BIN",
        )
        releasedCompiler(release, releases, gradleUserHome, offline).absolutePath
    }
    // `-Pkrusty.fixKotlincDefects=true`: krusty fixes the reference release's defects in the
    // compiles it replaces instead of reproducing them. A compile left to kotlinc keeps them.
    val defectFixes = project.providers.gradleProperty(FIX_KOTLINC_DEFECTS_PROPERTY)
        .map(String::toBoolean).orElse(false)
    val binaryPath = project.providers.gradleProperty("krusty.binary")
        .orElse(project.providers.environmentVariable("KRUSTY_BIN"))
        .orElse(releasedBinary)
    // Verify tasks exist only in a build that runs krustyVerify.
    val verifyReport = if (verifyRequested(project.gradle)) verifyReportTask(project, binaryPath) else null
    val verifying = verifyReport != null
    val timings = if (verifying) taskTimings(project, buildEvents) else null
    // The applied Kotlin Gradle plugin's release, read through the plugin interface every release
    // since 1.7 shares. This plugin is compiled against the API of the releases krusty supports, and
    // an older release's API differs in kind (KotlinTopLevelExtension was an abstract class before
    // 2.1), so a compile under an unsupported release is left to kotlinc before the rest of the API
    // is read.
    val kotlinPluginRelease by lazy {
        project.plugins.withType(KotlinBasePlugin::class.java).single().pluginVersion
    }
    project.tasks.withType(KotlinJvmCompile::class.java).all {
        val kotlinTask = this
        // A KotlinJvmCompile outside every source-set compilation is not a krusty compile either:
        // kotlin-dsl's `compilePluginsBlocks` compiles the `plugins {}` blocks it extracts from
        // precompiled script plugins as scripts, with no source set, Java sibling or classes output.
        // kapt's stub generation is a KotlinJvmCompile too, but it runs kotlinc with the kapt
        // plugin to write Java stubs, not classes; the compile after annotation processing is the
        // krusty compile, and a replacement here would depend on the processing that needs the stubs.
        val sourceSetName = kotlinTask.sourceSetName.orNull
        // Read only for a compile no earlier reason leaves to kotlinc: a kotlin-dsl project applies
        // more than one Kotlin plugin.
        val supportedRelease by lazy { kotlinPluginRelease in supportedKotlinVersions }
        // A multiplatform project's KotlinJvmCompile tasks also compile Android targets, which
        // krusty does not compile.
        val compilation by lazy { if (multiplatform) jvmCompilation(project, kotlinTask.name) else null }
        val kotlincReason = when {
            kotlinTask is KaptGenerateStubs -> "it generates kapt stubs"
            sourceSetName == null -> "it belongs to no source set"
            kotlinDsl -> "its project applies kotlin-dsl"
            !supportedRelease -> unsupportedKotlinPluginVersion(kotlinPluginRelease)
            multiplatform && compilation == null -> "it compiles no Kotlin/JVM target"
            else -> null
        }
        if (kotlincReason != null) {
            project.logger.lifecycle(
                "krusty: {} is left to kotlinc: {}",
                kotlinTask,
                kotlincReason,
            )
            // A kotlin-dsl compile is a compilation of the project that krusty does not take; kapt
            // stubs and script `plugins {}` blocks are no compilation of their own.
            if (verifyReport != null && kotlinDsl && kotlinTask !is KaptGenerateStubs && sourceSetName != null) {
                reportUnsupported(
                    verifyReport,
                    kotlinTask,
                    "krusty: the project applies kotlin-dsl, whose compiles stay with kotlinc",
                )
            } else if (verifyReport != null && sourceSetName != null && kotlinTask !is KaptGenerateStubs &&
                !supportedRelease
            ) {
                reportUnsupported(verifyReport, kotlinTask, kotlincReason)
            }
            return@all
        }
        val replacementName = "${kotlinTask.name}WithKrusty"
        val pluginVersion = project.provider { kotlinPluginRelease }
        val compilerPluginArguments = kotlinTask.pluginOptions.map(::pluginOptionArguments)
        val compilerVersion = project.extensions.getByType(KotlinTopLevelExtension::class.java)
            .compilerVersion.orElse(pluginVersion)
        val request = pluginVersion.zip(compilerVersion) { version, compiler ->
            kotlincRequest(kotlinTask, version, compiler)
        }
        val kotlincArguments = request.map(KotlincRequest::arguments)
        val compilerChoice = project.layout.buildDirectory
            .file("krusty/${kotlinTask.name}/refusals.txt").get().asFile
        val supportCheck = project.tasks.register(
            "${kotlinTask.name}KrustyCheck",
            KrustySupportCheckTask::class.java,
        ) {
            description = "Checks whether krusty implements everything ${kotlinTask.path} requests"
            compileTaskPath.set(kotlinTask.path)
            compilerArguments.set(kotlincArguments)
            pluginRefusals.set(request.map(KotlincRequest::refusals))
            toolchainRefusals.convention(emptyList())
            compilerPluginClasspath.from(kotlinTask.pluginClasspath)
            compilerPluginOptions.set(compilerPluginArguments)
            kotlinPluginVersion.set(pluginVersion)
            binary.set(project.layout.file(binaryPath.map(::File)))
            refusals.set(compilerChoice)
            fixKotlincDefects.set(defectFixes)
        }
        // KGP infers producer dependencies from task inputs as well as explicit dependsOn.
        // Every classpath reader needs that complete graph. The Kotlin task also depends on
        // our replacement, so exclude only that owned task by identity.
        val replacement = project.tasks.register(replacementName, KrustyCompileTask::class.java)
        val compilePrerequisites = project.provider {
            val replacementTask = replacement.get()
            kotlinTask.taskDependencies.getDependencies(kotlinTask).filterNot { it === replacementTask }
        }
        val classpathFingerprint = project.tasks.register(
            "${kotlinTask.name}KrustyClasspath",
            KrustyClasspathFingerprintTask::class.java,
        ) {
            description = "Fingerprints what ${kotlinTask.path} observes of its classpath"
            libraries.from(kotlinTask.libraries)
            friendPaths.from(kotlinTask.friendPaths)
            dependsOn(compilePrerequisites)
            binary.set(project.layout.file(binaryPath.map(::File)))
            viewCache.set(File(project.gradle.gradleUserHomeDir, "caches/krusty/dependency-views").absolutePath)
            fingerprint.set(project.layout.buildDirectory.file("krusty/${kotlinTask.name}/classpath-fingerprint.txt"))
        }
        // Both krusty compiles of this Kotlin task, the replacement and the verify compile beside
        // kotlinc, receive the same request.
        val configureCompilation: KrustyCompilation.() -> Unit = {
            sourceFiles.from(
                kotlinTask.sources.asFileTree.matching { include("**/*.kt") },
            )
            libraries.from(kotlinTask.libraries)
            friendPaths.from(kotlinTask.friendPaths)
            this.classpathFingerprint.set(classpathFingerprint.flatMap { it.fingerprint })
            val jvmCompilation = compilation
            if (jvmCompilation != null) {
                val fragments = project.provider { fragmentStructure(jvmCompilation, project.projectDir) }
                fragmentNames.set(fragments.map { it.names })
                fragmentSources.set(fragments.map { it.sources })
                fragmentRefines.set(fragments.map { it.refines })
            } else {
                fragmentNames.set(emptyList())
                fragmentSources.set(emptyList())
                fragmentRefines.set(emptyList())
            }
            projectDirectory.set(project.projectDir.absolutePath)
            compilerArguments.set(kotlincArguments)
            kotlinTarget.set(kotlinTask.compilerOptions.jvmTarget.map { it.target })
            // A compilation with no Java source set (a multiplatform JVM target) has no Java target
            // to disagree with.
            javaTarget.convention(kotlinTarget)
            targetValidationMode.set(kotlinTask.jvmTargetValidationMode.map { it.name })
            kotlinPluginVersion.set(pluginVersion)
            // The compiler-plugin request kotlinc receives from this task: KGP's resolved plugin
            // classpath (`-Xplugin`) and its per-plugin options (`-P plugin:<id>:<key>=<value>`).
            compilerPluginClasspath.from(kotlinTask.pluginClasspath)
            compilerPluginOptions.set(compilerPluginArguments)
            binary.set(project.layout.file(binaryPath.map(::File)))
            // The verify compile is compared with kotlinc's, so it reproduces kotlinc's defects.
            fixKotlincDefects.convention(false)
        }
        replacement.configure {
            group = kotlinTask.group ?: "build"
            description = "${kotlinTask.description ?: kotlinTask.name}, using krusty"
            configureCompilation()
            fixKotlincDefects.set(defectFixes)
            destinationDirectory.set(kotlinTask.destinationDirectory)
            kotlinTaskPath.set(kotlinTask.path)
            handoff.set(project.layout.buildDirectory.file("krusty/${kotlinTask.name}/handoff.txt"))
            dependsOn(compilePrerequisites)
            dependsOn(supportCheck)
            onlyIf("krusty implements everything the compile requests") { !compilerChoice.refuses() }
            onlyIf("the build verifies krusty against kotlinc, which compiles") { !verifying }
        }
        val verify = if (verifyReport != null && timings != null) {
            registerVerifyCompile(project, kotlinTask, verifyReport, timings) {
                configureCompilation()
                refusals.set(compilerChoice)
                dependsOn(supportCheck)
            }
        } else {
            null
        }
        val compilations = listOfNotNull(replacement, verify).map {
            project.tasks.named(it.name, KrustyCompilation::class.java)
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
            val kotlinJavaVersion = kotlinTask.kotlinJavaToolchainProvider
                .flatMap { it.javaVersion }
                .map { it.majorVersion }
                .orElse(selectedJavaVersion)
            for (compilation in compilations) {
                compilation.configure {
                    jdkHome.set(selectedJdkHome)
                    javaVersion.set(selectedJavaVersion)
                    this.kotlinJavaVersion.set(kotlinJavaVersion)
                }
            }
            // KGP compiles against the Kotlin task's own Java toolchain (`kotlinJavaToolchain`),
            // which defaults to the project's. Its JDK home is no public KGP API, so a compile that
            // sets a different one is left to kotlinc, which compiles it against that JDK.
            supportCheck.configure {
                toolchainRefusals.set(
                    kotlinJavaVersion.zip(selectedJavaVersion) { kotlin, java ->
                        listOfNotNull(kotlinJavaToolchainRefusal(kotlin, java))
                    },
                )
            }
            // KGP checks the Kotlin JVM target against the Java task's targetCompatibility, whose
            // convention follows `options.release`. It is read as a value: a mapped task provider
            // would make the compile depend on the Java task, which depends on the compile.
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
                for (compilation in compilations) {
                    compilation.configure {
                        javaSourceFiles.from(javaCompile.map { it.source })
                        javaTarget.set(javaCompile.flatMap { task -> project.provider { task.targetCompatibility } })
                    }
                }
            }
            java.sourceSets.configureEach {
                val sourceSet = this
                if (sourceSet.name == sourceSetName) {
                    val javaCompile = project.tasks.named(sourceSet.compileJavaTaskName, JavaCompile::class.java)
                    for (compilation in compilations) {
                        compilation.configure {
                            javaSourceFiles.from(sourceSet.allJava)
                            javaTarget.set(javaCompile.flatMap { task -> project.provider { task.targetCompatibility } })
                        }
                    }
                }
            }
        }

        // Preserve the public task graph: callers and downstream tasks can still depend on
        // compileKotlin/compileTestKotlin. A skipped task runs its dependencies but none of its own
        // compiler actions, so no private action list is mutated. kotlinc runs only for a compile
        // whose request krusty refused, or whose module krusty did not compile.
        kotlinTask.dependsOn(replacement)
        val handedBack = replacement.flatMap { it.handoff }
        kotlinTask.onlyIf("krusty refused part of the compile's request, handed the module back, or the build verifies krusty") {
            verifying || compilerChoice.refuses() || handedBack.get().asFile.refuses()
        }
    }
}

/**
 * What krusty receives for one Kotlin/JVM compile: the Kotlin task's sources, classpath, options and
 * compiler-plugin request, and the Java toolchain it compiles against. [KrustyCompileTask] compiles
 * it in place of kotlinc; [KrustyVerifyCompileTask] compiles it beside kotlinc.
 */
abstract class KrustyCompilation : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sourceFiles: ConfigurableFileCollection

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val javaSourceFiles: ConfigurableFileCollection

    /**
     * The compile's classpath. The compile reads it, but is keyed on [classpathFingerprint] in its
     * place, which the fingerprint task takes after every task producing an entry has run.
     */
    @get:Internal
    abstract val libraries: ConfigurableFileCollection

    /** The classpath entries whose internal declarations the compile sees, keyed like [libraries]. */
    @get:Internal
    abstract val friendPaths: ConfigurableFileCollection

    /** `krusty classpath-fingerprint` of [libraries] and [friendPaths]. */
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val classpathFingerprint: RegularFileProperty

    @get:Input
    abstract val compilerArguments: ListProperty<String>

    /** A multiplatform compilation's source sets, as `-Xfragments` names; empty otherwise. */
    @get:Input
    abstract val fragmentNames: ListProperty<String>

    /** `<fragment>:<source>` with each source relative to [projectDirectory]. */
    @get:Input
    abstract val fragmentSources: ListProperty<String>

    /** `<fragment>:<refined fragment>`, one per `dependsOn` edge. */
    @get:Input
    abstract val fragmentRefines: ListProperty<String>

    @get:Internal
    abstract val projectDirectory: Property<String>

    @get:Input
    abstract val kotlinPluginVersion: Property<String>

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

    /** Whether krusty fixes the reference release's defects (`-Xfix-kotlinc-defects`). */
    @get:Input
    abstract val fixKotlincDefects: Property<Boolean>

    /**
     * The Kotlin and Java sources krusty compiles, in path order. None without a Kotlin source:
     * KGP's task then has no source and compiles nothing, so neither Java sources nor the JVM
     * target are checked.
     */
    protected fun sources(): List<File> {
        val kotlinSources = sourceFiles.asFileTree.files.map(::normalized).sortedBy(File::getAbsolutePath)
        if (kotlinSources.isEmpty()) return emptyList()
        jvmTargetMismatch(kotlinTarget.get(), javaTarget.get())?.let { mismatch ->
            when (targetValidationMode.get()) {
                "ERROR" -> throw GradleException(mismatch)
                "WARNING" -> logger.warn(mismatch)
            }
        }
        val unsupported = kotlinSources.filter { it.extension != "kt" }
        if (unsupported.isNotEmpty()) {
            throw GradleException(
                "unsupported Kotlin source input(s): ${unsupported.joinToString { it.path }}",
            )
        }
        val javaSources = javaSourceFiles.asFileTree.files.filter { it.extension == "java" }
        return (kotlinSources + javaSources).distinct().sortedBy(File::getAbsolutePath)
    }

    /** krusty's command line for [sources], writing to [destination]. */
    protected fun krustyArguments(sources: List<File>, destination: File): List<String> {
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
        arguments.addAll(compilerPluginArguments(compilerPluginClasspath, compilerPluginOptions.get()))
        arguments.addAll(compilerArguments.get())
        if (fragmentNames.get().isNotEmpty()) {
            if ("-Xmulti-platform" !in arguments) arguments.add("-Xmulti-platform")
            arguments.add("-Xfragments=" + fragmentNames.get().joinToString(","))
            val projectDir = File(projectDirectory.get())
            fragmentSources.get().forEach { source ->
                val (fragment, path) = source.split(':', limit = 2)
                arguments.add("-Xfragment-sources=$fragment:" + normalized(File(projectDir, path)).absolutePath)
            }
            if (fragmentRefines.get().isNotEmpty()) {
                arguments.add("-Xfragment-refines=" + fragmentRefines.get().joinToString(","))
            }
        }
        // The support check leaves such a compile to kotlinc; krusty never compiles it against
        // another JDK.
        kotlinJavaToolchainRefusal(kotlinJavaVersion.get(), javaVersion.get())?.let { throw GradleException(it) }
        val pluginVersion = supportedKotlinPluginVersion(kotlinPluginVersion.get())
        arguments.add("-Xkotlin-reference-version=$pluginVersion")
        if (fixKotlincDefects.get()) arguments.add(FIX_KOTLINC_DEFECTS_ARGUMENT)
        if ("-no-jdk" !in arguments) {
            arguments.add("-jdk-home")
            arguments.add(jdkHome.get())
        }
        arguments.add("-no-stdlib")
        arguments.add("-no-reflect")
        arguments.add("-d")
        arguments.add(destination.absolutePath)
        return arguments
    }
}

@CacheableTask
abstract class KrustyCompileTask @Inject constructor(
    private val execOperations: ExecOperations,
    private val fileSystemOperations: FileSystemOperations,
) : KrustyCompilation() {
    @get:OutputDirectory
    abstract val destinationDirectory: DirectoryProperty

    /** The Kotlin task this compile replaces, which compiles the module with kotlinc on a handoff. */
    @get:Internal
    abstract val kotlinTaskPath: Property<String>

    /**
     * Empty when krusty compiled the module. Otherwise why it did not ([KotlincHandoff]): the Kotlin
     * task then compiles the module with kotlinc. A krusty failure never fails the build.
     */
    @get:OutputFile
    abstract val handoff: RegularFileProperty

    @TaskAction
    fun compile() {
        val sources = sources()
        val destination = destinationDirectory.get().asFile
        val arguments = krustyArguments(sources, destination)

        // A source removal and changes to multifile facades or module metadata must not leave any
        // product of the previous full invocation behind. Validate every option before mutating it.
        fileSystemOperations.delete { delete(destination) }
        destination.mkdirs()
        handoff.get().asFile.writeText("")
        if (sources.isEmpty()) return

        classpathFingerprintFailure(classpathFingerprint.get().asFile)?.let { reason ->
            handOff(internalErrorHandoff("$FINGERPRINT_FAILURE\n$reason"))
            return
        }
        val executable = binary.get().asFile.absolutePath
        val errors = ByteArrayOutputStream()
        val result = try {
            execOperations.exec {
                this.executable = executable
                args(arguments)
                errorOutput = TeeOutputStream(System.err, errors)
                isIgnoreExitValue = true
            }
        } catch (failure: GradleException) {
            handOff(internalErrorHandoff("$executable could not be run: ${failure.message}"))
            return
        }
        if (result.exitValue == 0) return
        // kotlinc writes the module's classes into the same directory; nothing krusty wrote stays.
        fileSystemOperations.delete { delete(destination) }
        destination.mkdirs()
        handOff(kotlincHandoff(result.exitValue, errors.toString(Charsets.UTF_8)))
    }

    private fun handOff(handoff: KotlincHandoff) {
        // The same line the support check logs for a request krusty refuses up front.
        logger.warn(
            "krusty: task '{}' is left to kotlinc: {}\n{}",
            kotlinTaskPath.get(),
            handoff.failure.description,
            handoff.details.joinToString("\n") { "  $it" },
        )
        this.handoff.get().asFile.writeText(handoff.record())
    }
}

/** Writes to both streams, so krusty's report reaches the build output and the task keeps it. */
private class TeeOutputStream(private val first: OutputStream, private val second: OutputStream) : OutputStream() {
    override fun write(byte: Int) {
        first.write(byte)
        second.write(byte)
    }

    override fun write(bytes: ByteArray, offset: Int, length: Int) {
        first.write(bytes, offset, length)
        second.write(bytes, offset, length)
    }

    override fun flush() {
        first.flush()
        second.flush()
    }
}

/**
 * `krusty classpath-fingerprint` of a compile's classpath and friend paths: what the compilation
 * observes of them. A class directory or a jar enters by its declarations and by the bytecode an
 * inline call copies out of it, not by its method bodies, so a dependency's body-only edit reruns
 * this task and leaves the fingerprint, and with it the compile keyed on it, unchanged.
 */
@DisableCachingByDefault(because = "Reading the classpath again is what a cache lookup would cost")
abstract class KrustyClasspathFingerprintTask @Inject constructor(
    private val execOperations: ExecOperations,
) : DefaultTask() {
    @get:Classpath
    abstract val libraries: ConfigurableFileCollection

    @get:Classpath
    abstract val friendPaths: ConfigurableFileCollection

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val binary: RegularFileProperty

    /**
     * Where krusty keeps each jar's view by the digest of its bytes, shared by every build of this
     * Gradle user home, so an unchanged library jar is parsed once. An entry is keyed by contents
     * alone and never stale, so the directory is no input.
     */
    @get:Internal
    abstract val viewCache: Property<String>

    @get:OutputFile
    abstract val fingerprint: RegularFileProperty

    @TaskAction
    fun digest() {
        // The compile's own classpath arguments; the JDK and the compile's other inputs are keyed
        // by the compile task itself.
        val arguments = mutableListOf(
            "classpath-fingerprint",
            "--view-cache",
            viewCache.get(),
            "-no-stdlib",
            "-no-reflect",
            "-no-jdk",
        )
        if (!libraries.isEmpty) {
            arguments.add("-classpath")
            arguments.add(libraries.files.joinToString(File.pathSeparator, transform = File::getAbsolutePath))
        }
        if (!friendPaths.isEmpty) {
            arguments.add("-Xfriend-paths=" + friendPaths.files.joinToString(",", transform = File::getAbsolutePath))
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
        if (result.exitValue != 0) {
            // Not a build failure: the compile keyed on this fingerprint hands its module to kotlinc.
            fingerprint.get().asFile.writeText(
                "$FINGERPRINT_FAILURE\n$executable classpath-fingerprint exited with ${result.exitValue} for $path\n" +
                    errors.toString(Charsets.UTF_8).trimEnd() + "\n",
            )
            return
        }
        fingerprint.get().asFile.writeText(output.toString(Charsets.UTF_8))
    }
}

/** The first line of a fingerprint krusty could not take; what follows it says why. */
private const val FINGERPRINT_FAILURE = "krusty classpath-fingerprint failed"

/** Why krusty could not fingerprint a compile's classpath, or null when the fingerprint is one. */
private fun classpathFingerprintFailure(fingerprint: File): String? {
    val text = fingerprint.readText()
    return if (text.startsWith("$FINGERPRINT_FAILURE\n")) text.removePrefix("$FINGERPRINT_FAILURE\n") else null
}

private fun normalized(file: File): File = file.toPath().toAbsolutePath().normalize().toFile()

/** The `-Xfragments` structure of a multiplatform compilation, read from its source sets. */
private class FragmentStructure(val names: List<String>, val sources: List<String>, val refines: List<String>)

/**
 * kotlinc's fragments are the compilation's source sets: its default source set and every source
 * set that one depends on, transitively. A fragment refines each source set it directly depends on.
 */
private fun fragmentStructure(compilation: KotlinCompilation<*>, projectDir: File): FragmentStructure {
    val sourceSets = compilation.allKotlinSourceSets.sortedBy { it.name }
    val root = projectDir.toPath().toAbsolutePath().normalize()
    return FragmentStructure(
        names = sourceSets.map { it.name },
        sources = sourceSets.flatMap { sourceSet ->
            sourceSet.kotlin.asFileTree.matching { include("**/*.kt") }.files
                .map { "${sourceSet.name}:" + root.relativize(normalized(it).toPath()) }
                .sorted()
        },
        refines = sourceSets.flatMap { sourceSet ->
            sourceSet.dependsOn.filter { it in sourceSets }.map { "${sourceSet.name}:${it.name}" }.sorted()
        },
    )
}

/** The Kotlin/JVM target compilation that `taskName` compiles in a multiplatform project. */
private fun jvmCompilation(project: Project, taskName: String): KotlinCompilation<*>? =
    (project.extensions.getByName("kotlin") as KotlinTargetsContainer).targets
        .filter { it.platformType == KotlinPlatformType.jvm }
        .flatMap { it.compilations }
        .singleOrNull { it.compileKotlinTaskName == taskName }

/**
 * Asks the krusty binary which of a compile's kotlinc arguments and requested compiler plugins it
 * refuses, and records them, one per line, in [refusals]: an empty file means krusty compiles the
 * request itself. The binary's argument dispositions and plugin registry are the single place that
 * knows what krusty implements. A request this plugin cannot derive krusty's arguments from
 * ([pluginRefusals]) is refused without asking.
 */
abstract class KrustySupportCheckTask @Inject constructor(
    private val execOperations: ExecOperations,
) : DefaultTask() {
    @get:Input
    abstract val compileTaskPath: Property<String>

    @get:Input
    abstract val compilerArguments: ListProperty<String>

    @get:Input
    abstract val pluginRefusals: ListProperty<String>

    /** Why krusty cannot compile against the Kotlin task's Java toolchain; empty when it can. */
    @get:Input
    abstract val toolchainRefusals: ListProperty<String>

    @get:Classpath
    abstract val compilerPluginClasspath: ConfigurableFileCollection

    @get:Input
    abstract val compilerPluginOptions: ListProperty<String>

    @get:Input
    abstract val kotlinPluginVersion: Property<String>

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val binary: RegularFileProperty

    /** Whether the build asked krusty to fix kotlinc's defects, which a compile left to kotlinc keeps. */
    @get:Input
    abstract val fixKotlincDefects: Property<Boolean>

    @get:OutputFile
    abstract val refusals: RegularFileProperty

    @TaskAction
    fun check() {
        val refusedByPlugin = pluginRefusals.get() + toolchainRefusals.get()
        if (refusedByPlugin.isNotEmpty()) {
            record(refusedByPlugin)
            return
        }
        val arguments = ArrayList<String>()
        arguments.add("refusals")
        arguments.addAll(compilerPluginArguments(compilerPluginClasspath, compilerPluginOptions.get()))
        arguments.addAll(compilerArguments.get())
        arguments.add("-Xkotlin-reference-version=${supportedKotlinPluginVersion(kotlinPluginVersion.get())}")
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
            1 -> record(refused)
            // A command line krusty rejects is left to kotlinc too: kotlinc reports it if it is
            // invalid, and compiles it if krusty was wrong to reject it.
            2 -> record(listOf("krusty rejects the command line") + errorLines(errors))
            else -> record(
                listOf("krusty refusals exited with ${result.exitValue}") + errorLines(errors),
            )
        }
    }

    private fun record(refused: List<String>) {
        logger.warn(
            "krusty: task '{}' is left to kotlinc: krusty does not implement everything it requests\n{}",
            compileTaskPath.get(),
            refused.joinToString("\n") { "  $it" },
        )
        if (fixKotlincDefects.get()) {
            logger.warn(
                "krusty: task '{}' keeps kotlinc's defects: {} applies only to compiles krusty runs",
                compileTaskPath.get(),
                FIX_KOTLINC_DEFECTS_PROPERTY,
            )
        }
        refusals.get().asFile.writeText(refused.joinToString("\n", postfix = "\n"))
    }
}

private fun File.refuses(): Boolean = length() > 0

private fun errorLines(errors: ByteArrayOutputStream): List<String> =
    errors.toString(Charsets.UTF_8).lines().map(String::trimEnd).filter(String::isNotBlank)

/**
 * KGP's compiler-plugin request: the resolved plugin classpath (`-Xplugin`) and the structured
 * per-plugin options (`-P plugin:<id>:<key>=<value>`). KGP places them before the free arguments,
 * and kotlinc accumulates every `-P` in command-line order, so a free `-P` that sets the same key
 * as a structured one replaces it, as in kotlinc.
 */
private fun compilerPluginArguments(classpath: FileCollection, options: List<String>): List<String> {
    val arguments = ArrayList<String>()
    if (!classpath.isEmpty) {
        arguments.add("-Xplugin=" + classpath.files.joinToString(",", transform = File::getAbsolutePath))
    }
    options.forEach { option ->
        arguments.add("-P")
        arguments.add(option)
    }
    return arguments
}

private fun pluginOptionArguments(configs: List<CompilerPluginConfig>): List<String> =
    configs.flatMap { config ->
        config.allOptions().flatMap { (pluginId, options) ->
            options.map { option -> "plugin:$pluginId:${option.key}=${option.value}" }
        }
    }

/**
 * KGP's check of a Kotlin JVM target against its Java compile task's `targetCompatibility`: the
 * Java value is normalized by KGP's own table, the Kotlin one is compared as it is.
 */
internal fun jvmTargetMismatch(kotlin: String, java: String): String? {
    val normalizedJava = when (java) {
        "6" -> "1.6"
        "7" -> "1.7"
        "8" -> "1.8"
        "1.9" -> "9"
        else -> java
    }
    return if (normalizedJava == kotlin) null else "Kotlin JVM target $kotlin differs from Java target $java"
}

/** Why krusty cannot compile against a Kotlin task's own Java toolchain, or null when it can. */
internal fun kotlinJavaToolchainRefusal(kotlinJava: String, gradleJava: String): String? =
    if (kotlinJava == gradleJava) {
        null
    } else {
        "Kotlin task Java toolchain $kotlinJava differs from Gradle Java toolchain $gradleJava, " +
            "whose JDK is the only one krusty receives"
    }

/**
 * The kotlinc arguments krusty receives for a compile, derived from its structured task inputs and
 * its free arguments, or the reasons this plugin cannot derive them. A compile with refusals is
 * left to kotlinc, which receives the task's own request unchanged.
 */
private fun supportedKotlinPluginVersion(version: String): String =
    if (version in supportedKotlinVersions) {
        version
    } else {
        throw GradleException(unsupportedKotlinPluginVersion(version))
    }

private fun unsupportedKotlinPluginVersion(version: String): String {
    val expected = supportedKotlinVersions.dropLast(1).joinToString(", ") +
        ", or " + supportedKotlinVersions.last()
    return "unsupported Kotlin Gradle plugin $version; expected $expected"
}
