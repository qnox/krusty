package krusty.buildtools;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import kotlin.KotlinVersion;
import org.jetbrains.kotlin.buildtools.api.BaseCompilationOperation;
import org.jetbrains.kotlin.buildtools.api.CompilationResult;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer;
import org.jetbrains.kotlin.buildtools.api.ExecutionPolicy;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.KotlinToolchains;
import org.jetbrains.kotlin.buildtools.api.SourcesChanges;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmPlatformToolchain;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmCompilationOperation;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * Drives the implementation the way a build tool does: load it through the Build Tools API's own
 * service lookup, configure a compilation with command-line arguments, and run it in a session.
 */
class KrustyToolchainsTest {
    @TempDir
    Path work;

    private final KotlinToolchains toolchains =
        KotlinToolchains.loadImplementation(KrustyToolchainsTest.class.getClassLoader());

    @Test
    void loadsAsTheBuildToolsApiImplementationForItsKotlinRelease() {
        assertInstanceOf(KrustyToolchains.class, toolchains);
        // The exact release identity the build was configured for, pre-release classifier and all.
        String release = System.getProperty("krusty.kotlinVersion");
        assertEquals(release, toolchains.getCompilerVersion());
        // The stdlib resolved for that release agrees; KotlinVersion.CURRENT omits a classifier
        // such as -Beta2, so it is the release's numeric prefix.
        assertTrue(release.startsWith(KotlinVersion.CURRENT + "-") || release.equals(KotlinVersion.CURRENT.toString()), release);
    }

    @Test
    void forwardsOneCommandLineApplicationAndRefusesToReplaceArgumentsWithinIt() {
        CompilerArguments.Builder arguments = new CompilerArguments.Builder();
        arguments.applyCommandLineArguments(List.of("-no-stdlib", "-module-name=box"));
        assertEquals(List.of("-no-stdlib", "-module-name=box"), arguments.build().toArgumentStrings());

        UnsupportedOperationException refusal = assertThrows(
            UnsupportedOperationException.class,
            () -> arguments.applyCommandLineArguments(List.of("-module-name=other")));
        assertEquals(
            "krusty forwards compiler arguments as one command line and cannot replace individual"
                + " arguments applied earlier; apply every argument in one applyCommandLineArguments call",
            refusal.getMessage());
        assertEquals(List.of("-no-stdlib", "-module-name=box"), arguments.build().toArgumentStrings());
    }

    @Test
    void compilesASourceSetIntoAFreshDestination() throws IOException {
        Path source = write("Box.kt", "package p\n\nfun box(): String = \"OK\"\n");
        Path destination = work.resolve("classes");
        Path stale = destination.resolve("p/Deleted.class");
        Files.createDirectories(stale.getParent());
        Files.write(stale, new byte[0]);

        RecordingLogger logger = new RecordingLogger();
        CompilationResult result = compile(List.of(source), destination, logger, null);

        assertEquals(CompilationResult.COMPILATION_SUCCESS, result, String.join("\n", logger.errors));
        assertTrue(Files.isRegularFile(destination.resolve("p/BoxKt.class")));
        assertFalse(Files.exists(stale), "a class of a deleted source survived");
        assertEquals(List.of(), logger.errors);
    }

    @Test
    void compilesUnderTheDaemonPolicyAsInProcess() throws IOException {
        Path source = write("Box.kt", "package p\n\nfun box(): String = \"OK\"\n");
        Path destination = work.resolve("classes");
        ExecutionPolicy.WithDaemon.Builder daemon = toolchains.daemonExecutionPolicyBuilder();
        daemon.set(ExecutionPolicy.WithDaemon.JVM_ARGUMENTS, List.of("-Xmx1g"));
        daemon.set(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS, 1000L);
        ExecutionPolicy.WithDaemon policy = daemon.build();
        // The built policy keeps what was set, and a builder made from it starts from those values.
        daemon.set(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS, 2000L);
        assertEquals(List.of(List.of("-Xmx1g"), 1000L, 1000L), List.of(
            policy.get(ExecutionPolicy.WithDaemon.JVM_ARGUMENTS),
            policy.get(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS),
            policy.toBuilder().get(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS)));

        RecordingLogger logger = new RecordingLogger();
        CompilationResult result = compile(List.of(source), destination, logger, null, stdlib(), null, policy);

        assertEquals(CompilationResult.COMPILATION_SUCCESS, result, String.join("\n", logger.errors));
        assertTrue(Files.isRegularFile(destination.resolve("p/BoxKt.class")));
        assertEquals(List.of(), logger.errors);
    }

    @Test
    void aNewDaemonPolicyAnswersTheDaemonsDefaults() {
        ExecutionPolicy.WithDaemon.Builder fresh = toolchains.daemonExecutionPolicyBuilder();
        ExecutionPolicy.WithDaemon built = fresh.build();
        for (ExecutionPolicy.WithDaemon.Builder builder : List.of(fresh, built.toBuilder())) {
            assertEquals(1048576L, builder.get(ExecutionPolicy.WithDaemon.LOGS_FILE_SIZE_LIMIT));
            assertEquals(3, builder.get(ExecutionPolicy.WithDaemon.LOGS_FILE_COUNT_LIMIT));
            assertEquals(
                java.nio.file.Paths.get(System.getProperty("java.io.tmpdir")),
                builder.get(ExecutionPolicy.WithDaemon.LOGS_PATH));
            assertTrue(builder.get(ExecutionPolicy.WithDaemon.DAEMON_RUN_DIR_PATH).endsWith(
                java.nio.file.Paths.get("kotlin", "daemon")) || builder.get(
                ExecutionPolicy.WithDaemon.DAEMON_RUN_DIR_PATH).endsWith(
                java.nio.file.Paths.get(".kotlin", "daemon")));
        }
        assertEquals(1048576L, built.get(ExecutionPolicy.WithDaemon.LOGS_FILE_SIZE_LIMIT));
        // The JVM arguments and the shutdown delay default to null, in a fresh, built and copied policy.
        for (ExecutionPolicy.WithDaemon.Builder builder : List.of(fresh, built.toBuilder())) {
            assertEquals(null, builder.get(ExecutionPolicy.WithDaemon.JVM_ARGUMENTS));
            assertEquals(null, builder.get(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS));
        }
        assertEquals(null, built.get(ExecutionPolicy.WithDaemon.JVM_ARGUMENTS));
        assertEquals(null, built.get(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS));
        // An option the policy does not declare has neither a value nor a default.
        ExecutionPolicy.WithDaemon.Option<?> undeclared = undeclaredDaemonOption();
        String expected = "UNDECLARED was not set in org.jetbrains.kotlin.buildtools.api.ExecutionPolicy.WithDaemon";
        for (java.util.function.Supplier<Object> read : List.<java.util.function.Supplier<Object>>of(
            () -> fresh.get(undeclared), () -> built.get(undeclared), () -> built.toBuilder().get(undeclared))) {
            assertEquals(expected, assertThrows(IllegalStateException.class, read::get).getMessage());
        }
    }

    /** An option of the daemon policy that no policy declares; the constructor differs by API release. */
    private static ExecutionPolicy.WithDaemon.Option<?> undeclaredDaemonOption() {
        try {
            java.lang.reflect.Constructor<?> constructor =
                ExecutionPolicy.WithDaemon.Option.class.getConstructors()[0];
            Object[] arguments = new Object[constructor.getParameterCount()];
            arguments[0] = "UNDECLARED";
            if (arguments.length > 1) {
                arguments[1] = new org.jetbrains.kotlin.buildtools.api.KotlinReleaseVersion(2, 3, 0);
            }
            return (ExecutionPolicy.WithDaemon.Option<?>) constructor.newInstance(arguments);
        } catch (ReflectiveOperationException e) {
            throw new AssertionError(e);
        }
    }

    @Test
    void reportsEachErrorThroughTheCallersRenderer() throws IOException {
        Path source = write("Broken.kt", "fun broken(): Int = missing()\n");
        List<String> rendered = new ArrayList<>();
        CompilerMessageRenderer renderer = (severity, message, location) -> {
            String text = severity + " " + location.getPath() + ":" + location.getLine() + ":"
                + location.getColumn() + " [" + location.getLineContent() + "] " + message;
            rendered.add(text);
            return text;
        };

        RecordingLogger logger = new RecordingLogger();
        CompilationResult result = compile(List.of(source), work.resolve("classes"), logger, renderer);

        String expected = "ERROR " + source + ":1:21 [fun broken(): Int = missing()] unresolved reference 'missing'.";
        assertEquals(CompilationResult.COMPILATION_ERROR, result);
        assertEquals(List.of(expected), rendered);
        assertEquals(List.of(expected), logger.errors);
    }

    @Test
    void keepsADependentsClassesWhenOnlyAnUpstreamBodyChanged() throws IOException {
        Path library = write("Library.kt", "package lib\n\nfun greet(): String = \"Hello\"\n");
        Path user = write("User.kt", "package app\n\nfun use(): String = lib.greet() + \"!\"\n");
        Path upstream = work.resolve("library-classes");
        Path downstream = work.resolve("user-classes");
        String classpath = stdlib() + File.pathSeparator + upstream;

        assertEquals(List.of(false, false), build(library, upstream, user, downstream, classpath));
        assertEquals(List.of(true, true), build(library, upstream, user, downstream, classpath),
            "an unchanged rebuild keeps both modules' classes");

        Files.writeString(library, "package lib\n\nfun greet(): String = \"Hi\"\n");
        assertEquals(List.of(false, true), build(library, upstream, user, downstream, classpath),
            "a body-only edit recompiles only its own module");

        Files.writeString(library, "package lib\n\nfun greet(): String = \"Hi\"\n\nfun added(): Int = 1\n");
        assertEquals(List.of(false, false), build(library, upstream, user, downstream, classpath),
            "a new declaration recompiles the dependent");

        Files.delete(downstream.resolve("app/UserKt.class"));
        assertEquals(List.of(true, false), build(library, upstream, user, downstream, classpath),
            "a changed output is rebuilt");
        assertTrue(Files.isRegularFile(downstream.resolve("app/UserKt.class")));
    }

    /** Compiles the library, then the user module on it; whether each kept its classes. */
    private List<Boolean> build(Path library, Path upstream, Path user, Path downstream, String classpath) {
        List<Boolean> kept = new ArrayList<>();
        for (List<Object> module : List.of(List.<Object>of(library, upstream, stdlib()), List.<Object>of(user, downstream, classpath))) {
            Path destination = (Path) module.get(1);
            RecordingLogger logger = new RecordingLogger();
            CompilationResult result = compile(
                List.of((Path) module.get(0)), destination, logger, null, (String) module.get(2),
                work.resolve(destination.getFileName() + "-ic"));
            assertEquals(CompilationResult.COMPILATION_SUCCESS, result, String.join("\n", logger.errors));
            kept.add(logger.infos.stream().anyMatch(message -> message.contains("are unchanged; keeping its classes")));
        }
        return kept;
    }

    private CompilationResult compile(
        List<Path> sources,
        Path destination,
        KotlinLogger logger,
        CompilerMessageRenderer renderer
    ) {
        return compile(sources, destination, logger, renderer, stdlib(), null);
    }

    private CompilationResult compile(
        List<Path> sources,
        Path destination,
        KotlinLogger logger,
        CompilerMessageRenderer renderer,
        String classpath,
        Path incrementalWorkingDirectory
    ) {
        return compile(sources, destination, logger, renderer, classpath, incrementalWorkingDirectory,
            toolchains.createInProcessExecutionPolicy());
    }

    private CompilationResult compile(
        List<Path> sources,
        Path destination,
        KotlinLogger logger,
        CompilerMessageRenderer renderer,
        String classpath,
        Path incrementalWorkingDirectory,
        ExecutionPolicy policy
    ) {
        JvmPlatformToolchain jvm = toolchains.getToolchain(JvmPlatformToolchain.class);
        JvmCompilationOperation.Builder builder = jvm.jvmCompilationOperationBuilder(sources, destination);
        builder.getCompilerArguments().applyArgumentStrings(List.of(
            "-classpath=" + classpath,
            "-no-stdlib",
            "-module-name=box"));
        if (renderer != null) {
            builder.set(BaseCompilationOperation.COMPILER_MESSAGE_RENDERER, renderer);
        }
        if (incrementalWorkingDirectory != null) {
            builder.set(
                JvmCompilationOperation.INCREMENTAL_COMPILATION,
                builder.snapshotBasedIcConfigurationBuilder(
                    incrementalWorkingDirectory, SourcesChanges.ToBeCalculated.INSTANCE, List.of()).build());
        }
        try (KotlinToolchains.BuildSession session = toolchains.createBuildSession()) {
            return session.executeOperation(builder.build(), policy, logger);
        }
    }

    private static String stdlib() {
        for (String entry : System.getProperty("java.class.path").split(File.pathSeparator)) {
            if (Path.of(entry).getFileName().toString().startsWith("kotlin-stdlib-")) {
                return entry;
            }
        }
        throw new IllegalStateException("kotlin-stdlib is not on the test classpath");
    }

    private Path write(String name, String text) throws IOException {
        Path path = work.resolve(name);
        Files.writeString(path, text);
        return path;
    }

    private static final class RecordingLogger implements KotlinLogger {
        final List<String> errors = new ArrayList<>();
        final List<String> infos = new ArrayList<>();

        @Override
        public boolean isDebugEnabled() {
            return false;
        }

        @Override
        public void error(String message, Throwable throwable) {
            errors.add(message);
        }

        @Override
        public void warn(String message, Throwable throwable) {
        }

        @Override
        public void info(String message) {
            infos.add(message);
        }

        @Override
        public void debug(String message) {
        }

        @Override
        public void lifecycle(String message) {
        }
    }
}
