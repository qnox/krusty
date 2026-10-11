package krusty.buildtools;

import java.util.UUID;
import org.jetbrains.kotlin.buildtools.api.BuildOperation;
import org.jetbrains.kotlin.buildtools.api.ExecutionPolicy;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.KotlinToolchains;
import org.jetbrains.kotlin.buildtools.api.ProjectId;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmPlatformToolchain;

/**
 * The Kotlin Build Tools API entry point, found by {@code KotlinToolchains.loadImplementation}
 * through {@code META-INF/services}. A build tool that loads the Build Tools API implementation from
 * a classpath compiles Kotlin/JVM with the krusty binary when this jar is that classpath.
 *
 * Only in-process execution and the JVM toolchain exist. In-process means the operation runs on the
 * caller's thread; the compiler itself is always a krusty child process.
 */
public final class KrustyToolchains implements KotlinToolchains {
    private final KrustyConfiguration configuration = KrustyConfiguration.load();
    private final KrustyJvmToolchain jvm = new KrustyJvmToolchain(configuration);

    @Override
    public <T extends Toolchain> T getToolchain(Class<T> type) {
        if (type == JvmPlatformToolchain.class) {
            return type.cast(jvm);
        }
        throw new IllegalStateException("krusty provides only the JVM toolchain, not " + type.getName());
    }

    @Override
    public ExecutionPolicy.InProcess createInProcessExecutionPolicy() {
        return InProcess.INSTANCE;
    }

    @Override
    public ExecutionPolicy.WithDaemon.Builder daemonExecutionPolicyBuilder() {
        throw new UnsupportedOperationException("krusty has no compile daemon; use the in-process policy");
    }

    @Override
    public String getCompilerVersion() {
        return configuration.kotlinVersion();
    }

    @Override
    public BuildSession createBuildSession() {
        return new Session(this, new ProjectId.ProjectUUID(UUID.randomUUID()));
    }

    private enum InProcess implements ExecutionPolicy.InProcess {
        INSTANCE
    }

    private record Session(KotlinToolchains toolchains, ProjectId projectId) implements BuildSession {
        @Override
        public KotlinToolchains getKotlinToolchains() {
            return toolchains;
        }

        @Override
        public ProjectId getProjectId() {
            return projectId;
        }

        @Override
        public <R> R executeOperation(BuildOperation<R> operation) {
            return executeOperation(operation, InProcess.INSTANCE, NoLogger.INSTANCE);
        }

        @Override
        public <R> R executeOperation(BuildOperation<R> operation, ExecutionPolicy policy, KotlinLogger logger) {
            if (!(policy instanceof InProcess)) {
                throw new IllegalArgumentException("krusty runs only with its own in-process policy, not " + policy);
            }
            if (operation instanceof KrustyOperation<R> krustyOperation) {
                return krustyOperation.execute(logger == null ? NoLogger.INSTANCE : logger);
            }
            throw new IllegalArgumentException("operation was not created by the krusty toolchain: " + operation);
        }

        @Override
        public void close() {
        }
    }

    private enum NoLogger implements KotlinLogger {
        INSTANCE;

        @Override
        public boolean isDebugEnabled() {
            return false;
        }

        @Override
        public void error(String message, Throwable throwable) {
        }

        @Override
        public void warn(String message, Throwable throwable) {
        }

        @Override
        public void info(String message) {
        }

        @Override
        public void debug(String message) {
        }

        @Override
        public void lifecycle(String message) {
        }
    }
}
