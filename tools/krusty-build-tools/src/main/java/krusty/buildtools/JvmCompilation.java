package krusty.buildtools;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.stream.Stream;
import org.jetbrains.kotlin.buildtools.api.BaseCompilationOperation;
import org.jetbrains.kotlin.buildtools.api.BaseIncrementalCompilationConfiguration;
import org.jetbrains.kotlin.buildtools.api.BuildOperation;
import org.jetbrains.kotlin.buildtools.api.CompilationResult;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.SourcesChanges;
import org.jetbrains.kotlin.buildtools.api.arguments.JvmCompilerArguments;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmSnapshotBasedIncrementalCompilationConfiguration;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmCompilationOperation;
import org.jetbrains.kotlin.buildtools.api.trackers.BuildMetricsCollector;

/**
 * One Kotlin/JVM compilation, run as one krusty process over the whole source set.
 *
 * krusty does not compile incrementally. An incremental-compilation configuration is accepted
 * because callers attach one whenever their own up-to-date check found the module dirty, but the
 * destination is always cleared and rebuilt, so classes of deleted sources never survive.
 */
final class JvmCompilation implements JvmCompilationOperation, KrustyOperation<CompilationResult> {
    private final KrustyConfiguration configuration;
    private final List<Path> sources;
    private final Path destinationDirectory;
    private final JvmCompilerArguments arguments;
    private final OptionValues options;
    private volatile Process process;
    private volatile boolean cancelled;

    private JvmCompilation(
        KrustyConfiguration configuration,
        List<Path> sources,
        Path destinationDirectory,
        JvmCompilerArguments arguments,
        OptionValues options
    ) {
        this.configuration = configuration;
        this.sources = sources;
        this.destinationDirectory = destinationDirectory;
        this.arguments = arguments;
        this.options = options;
    }

    @Override
    public List<Path> getSources() {
        return sources;
    }

    @Override
    public Path getDestinationDirectory() {
        return destinationDirectory;
    }

    @Override
    public JvmCompilerArguments getCompilerArguments() {
        return arguments;
    }

    @Override
    public JvmCompilationOperation.Builder toBuilder() {
        Builder builder = new Builder(configuration, sources, destinationDirectory, options.copy());
        builder.getCompilerArguments().applyArgumentStrings(arguments.toArgumentStrings());
        return builder;
    }

    @Override
    public <V> V get(JvmCompilationOperation.Option<V> option) {
        return options.get(option);
    }

    @Override
    public <V> V get(BaseCompilationOperation.Option<V> option) {
        return options.get(option);
    }

    @Override
    public <V> V get(BuildOperation.Option<V> option) {
        return options.get(option);
    }

    @Override
    public void cancel() {
        cancelled = true;
        Process running = process;
        if (running != null) {
            running.destroyForcibly();
        }
    }

    @Override
    public CompilationResult execute(KotlinLogger logger) {
        clearDestination();
        if (sources.isEmpty()) {
            return CompilationResult.COMPILATION_SUCCESS;
        }
        List<String> command = new ArrayList<>();
        command.add(configuration.binary().toString());
        command.addAll(arguments.toArgumentStrings());
        command.add("-Xkotlin-reference-version=" + configuration.kotlinVersion());
        command.add("-d");
        command.add(destinationDirectory.toString());
        for (Path source : sources) {
            command.add(source.toString());
        }
        if (logger.isDebugEnabled()) {
            logger.debug("krusty command: " + String.join(" ", command));
        }

        CompilerOutput output = new CompilerOutput(options.get(BaseCompilationOperation.COMPILER_MESSAGE_RENDERER));
        int exitCode;
        try {
            Process started = new ProcessBuilder(command).redirectErrorStream(true).start();
            process = started;
            if (cancelled) {
                started.destroyForcibly();
            }
            started.getOutputStream().close();
            output.read(started.getInputStream());
            exitCode = started.waitFor();
        } catch (IOException e) {
            logger.error("cannot run krusty: " + e.getMessage(), e);
            return CompilationResult.COMPILER_INTERNAL_ERROR;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            process.destroyForcibly();
            logger.error("interrupted while krusty was compiling", e);
            return CompilationResult.COMPILER_INTERNAL_ERROR;
        } finally {
            process = null;
        }
        output.report(logger);

        BuildMetricsCollector metrics = options.get(BuildOperation.METRICS_COLLECTOR);
        if (metrics != null) {
            metrics.collectMetric("Total compiler iteration", BuildMetricsCollector.ValueType.NUMBER, 1);
        }
        if (exitCode == 0) {
            return CompilationResult.COMPILATION_SUCCESS;
        }
        if (output.hasErrors()) {
            return CompilationResult.COMPILATION_ERROR;
        }
        logger.error("krusty exited with " + exitCode + " without reporting an error", null);
        return CompilationResult.COMPILER_INTERNAL_ERROR;
    }

    private void clearDestination() {
        try {
            if (Files.exists(destinationDirectory)) {
                try (Stream<Path> walk = Files.walk(destinationDirectory)) {
                    for (Path path : walk.sorted(Comparator.reverseOrder()).toList()) {
                        if (!path.equals(destinationDirectory)) {
                            Files.delete(path);
                        }
                    }
                }
            }
            Files.createDirectories(destinationDirectory);
        } catch (IOException e) {
            throw new UncheckedIOException("cannot clear " + destinationDirectory, e);
        }
    }

    static final class Builder implements JvmCompilationOperation.Builder {
        private final KrustyConfiguration configuration;
        private final List<Path> sources;
        private final Path destinationDirectory;
        private final OptionValues options;
        private final CompilerArguments.Builder arguments = new CompilerArguments.Builder();

        Builder(KrustyConfiguration configuration, List<Path> sources, Path destinationDirectory) {
            this(configuration, sources, destinationDirectory, new OptionValues());
        }

        private Builder(
            KrustyConfiguration configuration,
            List<Path> sources,
            Path destinationDirectory,
            OptionValues options
        ) {
            this.configuration = configuration;
            this.sources = sources;
            this.destinationDirectory = destinationDirectory;
            this.options = options;
        }

        @Override
        public List<Path> getSources() {
            return sources;
        }

        @Override
        public Path getDestinationDirectory() {
            return destinationDirectory;
        }

        @Override
        public JvmCompilerArguments.Builder getCompilerArguments() {
            return arguments;
        }

        @Override
        public <V> V get(JvmCompilationOperation.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(JvmCompilationOperation.Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public <V> V get(BaseCompilationOperation.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(BaseCompilationOperation.Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public <V> V get(BuildOperation.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(BuildOperation.Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public JvmCompilationOperation build() {
            return new JvmCompilation(configuration, sources, destinationDirectory, arguments.build(), options.copy());
        }

        @Override
        public JvmSnapshotBasedIncrementalCompilationConfiguration.Builder snapshotBasedIcConfigurationBuilder(
            Path workingDirectory,
            SourcesChanges sourcesChanges,
            List<? extends Path> dependenciesSnapshotFiles
        ) {
            return new IncrementalConfiguration(workingDirectory, sourcesChanges, List.copyOf(dependenciesSnapshotFiles), null);
        }

        @Override
        public JvmSnapshotBasedIncrementalCompilationConfiguration.Builder snapshotBasedIcConfigurationBuilder(
            Path workingDirectory,
            SourcesChanges sourcesChanges,
            List<? extends Path> dependenciesSnapshotFiles,
            Path shrunkClasspathSnapshot
        ) {
            return new IncrementalConfiguration(
                workingDirectory, sourcesChanges, List.copyOf(dependenciesSnapshotFiles), shrunkClasspathSnapshot);
        }
    }

    /** Recorded so the caller's builder code runs unchanged; krusty does not read it. */
    private static final class IncrementalConfiguration implements JvmSnapshotBasedIncrementalCompilationConfiguration.Builder {
        private final Path workingDirectory;
        private final SourcesChanges sourcesChanges;
        private final List<Path> dependenciesSnapshotFiles;
        private final Path shrunkClasspathSnapshot;
        private final OptionValues options = new OptionValues();

        IncrementalConfiguration(
            Path workingDirectory,
            SourcesChanges sourcesChanges,
            List<Path> dependenciesSnapshotFiles,
            Path shrunkClasspathSnapshot
        ) {
            this.workingDirectory = workingDirectory;
            this.sourcesChanges = sourcesChanges;
            this.dependenciesSnapshotFiles = dependenciesSnapshotFiles;
            this.shrunkClasspathSnapshot = shrunkClasspathSnapshot;
        }

        @Override
        public Path getWorkingDirectory() {
            return workingDirectory;
        }

        @Override
        public SourcesChanges getSourcesChanges() {
            return sourcesChanges;
        }

        @Override
        public List<Path> getDependenciesSnapshotFiles() {
            return dependenciesSnapshotFiles;
        }

        @Override
        public Path getShrunkClasspathSnapshot() {
            return shrunkClasspathSnapshot;
        }

        @Override
        public <V> V get(JvmSnapshotBasedIncrementalCompilationConfiguration.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(JvmSnapshotBasedIncrementalCompilationConfiguration.Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public <V> V get(BaseIncrementalCompilationConfiguration.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(BaseIncrementalCompilationConfiguration.Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public JvmSnapshotBasedIncrementalCompilationConfiguration build() {
            return shrunkClasspathSnapshot == null
                ? new JvmSnapshotBasedIncrementalCompilationConfiguration(
                    workingDirectory, sourcesChanges, dependenciesSnapshotFiles)
                : new JvmSnapshotBasedIncrementalCompilationConfiguration(
                    workingDirectory, sourcesChanges, dependenciesSnapshotFiles, shrunkClasspathSnapshot);
        }
    }
}
