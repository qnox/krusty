package krusty

import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.Project
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.invocation.Gradle
import org.gradle.api.provider.MapProperty
import org.gradle.api.provider.Property
import org.gradle.api.provider.Provider
import org.gradle.api.services.BuildService
import org.gradle.api.services.BuildServiceParameters
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.Optional
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.OutputFile
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.api.tasks.TaskProvider
import org.gradle.build.event.BuildEventsListenerRegistry
import org.gradle.process.ExecOperations
import org.gradle.tooling.events.FinishEvent
import org.gradle.tooling.events.OperationCompletionListener
import org.gradle.tooling.events.task.TaskFinishEvent
import org.gradle.tooling.events.task.TaskSuccessResult
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import javax.inject.Inject

// Verify mode. `gradle krustyVerify` compiles every Kotlin/JVM compilation with kotlinc, as a build
// without krusty would, and ships kotlinc's classes. Beside each one, krusty compiles the same request
// into a directory of its own, and `krusty verify-compile` records whether krusty supports it, how
// long it took, its peak memory and how its classes differ from kotlinc's. The `krustyVerify` task
// then writes the build's report with `krusty verify-report`, and an anonymized issue the user can
// submit. A compile krusty does not support is reported as unsupported, not as a failure.

internal const val VERIFY_TASK_NAME = "krustyVerify"

private const val VERIFY_TASK_PROPERTY = "krusty.verifyTaskProvider"

/** Whether this build runs `krustyVerify`: the Kotlin tasks then compile with kotlinc. */
internal fun verifyRequested(gradle: Gradle): Boolean =
    gradle.startParameter.taskNames.any { it == VERIFY_TASK_NAME || it.endsWith(":$VERIFY_TASK_NAME") }

/** The root project's `krustyVerify` task, registered by the first project applying the plugin. */
internal fun verifyReportTask(project: Project): TaskProvider<KrustyVerifyReportTask> {
    val root = project.rootProject
    val extra = root.extensions.extraProperties
    if (extra.has(VERIFY_TASK_PROPERTY)) {
        return root.tasks.named(VERIFY_TASK_NAME, KrustyVerifyReportTask::class.java)
    }
    return root.tasks.register(VERIFY_TASK_NAME, KrustyVerifyReportTask::class.java) {
        group = "verification"
        description = "Compiles every Kotlin/JVM compilation with kotlinc and with krusty, and reports " +
            "what krusty compiles and how its classes differ from kotlinc's"
        reportDirectory.set(root.layout.buildDirectory.dir("reports/krusty-verify"))
        unsupportedRecords.set(root.layout.buildDirectory.dir("krusty/verify-unsupported"))
        gradleVersion.set(root.gradle.gradleVersion)
        jdk.set(System.getProperty("java.version"))
        shareNames.set(
            root.providers.gradleProperty("krusty.verify.shareNames").map(String::toBoolean).orElse(false),
        )
    }.also {
        extra.set(VERIFY_TASK_PROPERTY, true)
    }
}

/**
 * The time each task took, from Gradle's task completion events: the Kotlin tasks' kotlinc time, as
 * the build measured it.
 */
abstract class TaskTimings : BuildService<BuildServiceParameters.None>, OperationCompletionListener {
    private val millis = ConcurrentHashMap<String, Long>()

    override fun onFinish(event: FinishEvent) {
        if (event !is TaskFinishEvent) return
        val result = event.result
        if (result is TaskSuccessResult && !result.isUpToDate && !result.isFromCache) {
            millis[event.descriptor.taskPath] = result.endTime - result.startTime
        }
    }

    fun millis(taskPath: String): Long? = millis[taskPath]
}

internal fun taskTimings(project: Project, buildEvents: BuildEventsListenerRegistry) =
    project.gradle.sharedServices.registerIfAbsent("krustyTaskTimings", TaskTimings::class.java) {}
        .also(buildEvents::onTaskCompletion)

/**
 * Register the krusty compile that verifies [kotlinTask] against kotlinc. kotlinc compiles every
 * source again rather than incrementally or from the build cache, so its time is a full compile's,
 * as krusty's is.
 */
internal fun registerVerifyCompile(
    project: Project,
    kotlinTask: KotlinJvmCompile,
    report: TaskProvider<KrustyVerifyReportTask>,
    timings: Provider<TaskTimings>,
    configure: KrustyVerifyCompileTask.() -> Unit,
): TaskProvider<KrustyVerifyCompileTask> {
    kotlinTask.outputs.upToDateWhen { false }
    kotlinTask.outputs.doNotCacheIf("the build verifies krusty against kotlinc's full compile") { true }
    val verify = project.tasks.register("${kotlinTask.name}KrustyVerify", KrustyVerifyCompileTask::class.java) {
        description = "Compiles what ${kotlinTask.path} compiles with krusty and compares it with kotlinc's classes"
        compilationPath.set(kotlinTask.path)
        kotlinTask.sourceSetName.orNull?.let(sourceSet::set)
        reference.set(kotlinTask.destinationDirectory)
        krustyClasses.set(project.layout.buildDirectory.dir("krusty/verify/${kotlinTask.name}/classes"))
        record.set(project.layout.buildDirectory.file("krusty/verify/${kotlinTask.name}.json"))
        taskTimings.set(timings)
        usesService(timings)
        dependsOn(kotlinTask)
        configure()
    }
    report.configure { records.from(verify.flatMap { it.record }) }
    return verify
}

/** krusty's compile of one compilation kotlinc compiled too, recorded by `krusty verify-compile`. */
abstract class KrustyVerifyCompileTask @Inject constructor(
    private val execOperations: ExecOperations,
    private val fileSystemOperations: FileSystemOperations,
) : KrustyCompilation() {
    @get:Input
    abstract val compilationPath: Property<String>

    @get:Input
    @get:Optional
    abstract val sourceSet: Property<String>

    /** kotlinc's classes, which the Kotlin task this one depends on writes. */
    @get:Internal
    abstract val reference: DirectoryProperty

    /** The krusty support check's refusals: a compile it refuses is unsupported. */
    @get:Internal
    abstract val refusals: RegularFileProperty

    @get:Internal
    abstract val taskTimings: Property<TaskTimings>

    @get:OutputDirectory
    abstract val krustyClasses: DirectoryProperty

    @get:OutputFile
    abstract val record: RegularFileProperty

    init {
        // The record holds this build's kotlinc time and krusty's measurement; nothing reuses one.
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun verify() {
        val destination = krustyClasses.get().asFile
        fileSystemOperations.delete { delete(destination) }
        destination.mkdirs()
        val arguments = mutableListOf(
            "verify-compile",
            "--reference", reference.get().asFile.absolutePath,
            "--record", record.get().asFile.absolutePath,
            "--compilation", compilationPath.get(),
            "--kotlin-version", kotlinPluginVersion.get(),
        )
        sourceSet.orNull?.let { arguments.addAll(listOf("--source-set", it)) }
        taskTimings.get().millis(compilationPath.get())?.let {
            arguments.addAll(listOf("--kotlinc-millis", it.toString()))
        }
        refusals.asFile.orNull?.takeIf { it.length() > 0 }?.let {
            arguments.addAll(listOf("--unsupported-from", it.absolutePath))
        }
        // Always the full request: without Kotlin sources the record says so, and an earlier run's
        // record never stands in for this one.
        arguments.add("--")
        arguments.addAll(krustyArguments(sources(), destination))
        val output = ByteArrayOutputStream()
        val result = execOperations.exec {
            executable = binary.get().asFile.absolutePath
            args(arguments)
            standardOutput = output
            isIgnoreExitValue = true
        }
        output.toString(Charsets.UTF_8).lines().filter(String::isNotBlank).forEach(logger::lifecycle)
        if (result.exitValue != 0) {
            throw GradleException("krusty verify-compile exited with ${result.exitValue} for $path")
        }
    }
}

/** The root `krustyVerify` task: the build's verify report, from every compilation's record. */
abstract class KrustyVerifyReportTask @Inject constructor(
    private val execOperations: ExecOperations,
    private val fileSystemOperations: FileSystemOperations,
) : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.ABSOLUTE)
    abstract val records: ConfigurableFileCollection

    /** Compilations the plugin leaves to kotlinc without asking krusty, by task path, and why. */
    @get:Input
    abstract val unsupportedCompilations: MapProperty<String, String>

    @get:Internal
    abstract val binary: RegularFileProperty

    @get:Input
    abstract val gradleVersion: Property<String>

    @get:Input
    abstract val jdk: Property<String>

    /** `-Pkrusty.verify.shareNames=true` names compilations and errors in the issue text. */
    @get:Input
    abstract val shareNames: Property<Boolean>

    @get:Internal
    abstract val unsupportedRecords: DirectoryProperty

    @get:OutputDirectory
    abstract val reportDirectory: DirectoryProperty

    init {
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun report() {
        val executable = binary.get().asFile.absolutePath
        // Only this invocation's records and report: nothing an earlier one wrote survives it.
        fileSystemOperations.delete { delete(reportDirectory) }
        val unsupported = unsupportedRecords.get().asFile
        fileSystemOperations.delete { delete(unsupported) }
        val unsupportedRecordFiles = unsupportedCompilations.get().entries.mapIndexed { index, (compilation, reason) ->
            val record = File(unsupported, "records/$index.json")
            exec(
                executable,
                listOf(
                    "verify-compile",
                    "--reference", File(unsupported, "reference").absolutePath,
                    "--record", record.absolutePath,
                    "--compilation", compilation,
                    "--unsupported", reason,
                ),
            )
            record
        }
        // Every current compilation's verify task ran in this build and wrote its record; a missing
        // one fails the report rather than dropping that compilation.
        val recorded = records.files.toList() + unsupportedRecordFiles
        val arguments = mutableListOf(
            "verify-report",
            "--out", reportDirectory.get().asFile.absolutePath,
            "--build-tool", "Gradle ${gradleVersion.get()}",
            "--jdk", jdk.get(),
        )
        if (shareNames.get()) arguments.add("--share-names")
        arguments.addAll(recorded.map(File::getAbsolutePath))
        exec(executable, arguments).lines().filter(String::isNotBlank).forEach(logger::lifecycle)
    }

    private fun exec(executable: String, arguments: List<String>): String {
        val output = ByteArrayOutputStream()
        val result = execOperations.exec {
            this.executable = executable
            args(arguments)
            standardOutput = output
            isIgnoreExitValue = true
        }
        if (result.exitValue != 0) {
            throw GradleException("krusty ${arguments.first()} exited with ${result.exitValue}")
        }
        return output.toString(Charsets.UTF_8)
    }
}
