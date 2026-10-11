package krusty.buildtools;

import java.nio.file.Path;
import java.util.List;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmPlatformToolchain;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.DiscoverScriptExtensionsOperation;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmClasspathSnapshottingOperation;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmCompilationOperation;

final class KrustyJvmToolchain implements JvmPlatformToolchain {
    private final KrustyConfiguration configuration;

    KrustyJvmToolchain(KrustyConfiguration configuration) {
        this.configuration = configuration;
    }

    @Override
    public JvmCompilationOperation.Builder jvmCompilationOperationBuilder(
        List<? extends Path> sources,
        Path destinationDirectory
    ) {
        return new JvmCompilation.Builder(configuration, List.copyOf(sources), destinationDirectory);
    }

    @Override
    public JvmClasspathSnapshottingOperation.Builder classpathSnapshottingOperationBuilder(Path classpathEntry) {
        return new ClasspathSnapshotting.Builder(classpathEntry);
    }

    @Override
    public DiscoverScriptExtensionsOperation.Builder discoverScriptExtensionsOperationBuilder(
        List<? extends Path> classpath
    ) {
        throw new UnsupportedOperationException("krusty does not compile Kotlin scripts");
    }
}
