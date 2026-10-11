package krusty.buildtools;

import java.io.File;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import org.jetbrains.kotlin.buildtools.api.BuildOperation;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.jvm.ClassSnapshot;
import org.jetbrains.kotlin.buildtools.api.jvm.ClasspathEntrySnapshot;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmClasspathSnapshottingOperation;

/**
 * Classpath snapshots feed kotlinc's incremental compilation. krusty always compiles a whole module
 * (the build tool decides whether a module is dirty), so a snapshot carries nothing; it exists so a
 * caller that snapshots every classpath entry before compiling keeps working.
 */
final class ClasspathSnapshotting implements JvmClasspathSnapshottingOperation, KrustyOperation<ClasspathEntrySnapshot> {
    private final Path classpathEntry;
    private final OptionValues options;

    private ClasspathSnapshotting(Path classpathEntry, OptionValues options) {
        this.classpathEntry = classpathEntry;
        this.options = options;
    }

    @Override
    public Path getClasspathEntry() {
        return classpathEntry;
    }

    @Override
    public JvmClasspathSnapshottingOperation.Builder toBuilder() {
        return new Builder(classpathEntry, options.copy());
    }

    @Override
    public <V> V get(JvmClasspathSnapshottingOperation.Option<V> option) {
        return options.get(option);
    }

    @Override
    public <V> V get(BuildOperation.Option<V> option) {
        return options.get(option);
    }

    @Override
    public ClasspathEntrySnapshot execute(KotlinLogger logger) {
        return EmptySnapshot.INSTANCE;
    }

    private enum EmptySnapshot implements ClasspathEntrySnapshot {
        INSTANCE;

        @Override
        public Map<String, ClassSnapshot> getClassSnapshots() {
            return Map.of();
        }

        @Override
        public void saveSnapshot(File file) {
            try {
                Files.write(file.toPath(), new byte[0]);
            } catch (IOException e) {
                throw new UncheckedIOException(e);
            }
        }
    }

    static final class Builder implements JvmClasspathSnapshottingOperation.Builder {
        private final Path classpathEntry;
        private final OptionValues options;

        Builder(Path classpathEntry) {
            this(classpathEntry, new OptionValues());
        }

        private Builder(Path classpathEntry, OptionValues options) {
            this.classpathEntry = classpathEntry;
            this.options = options;
        }

        @Override
        public Path getClasspathEntry() {
            return classpathEntry;
        }

        @Override
        public <V> V get(JvmClasspathSnapshottingOperation.Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(JvmClasspathSnapshottingOperation.Option<V> option, V value) {
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
        public JvmClasspathSnapshottingOperation build() {
            return new ClasspathSnapshotting(classpathEntry, options.copy());
        }
    }
}
