package krusty.buildtools;

import java.io.File;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.HashMap;
import java.util.Map;
import org.jetbrains.kotlin.buildtools.api.ExecutionPolicy;

/**
 * The daemon execution policy. A build tool picks it to compile outside its own process, which is
 * where krusty always compiles: every operation runs one krusty child process, under this policy as
 * under the in-process one. The options configure the JVM of a Kotlin daemon (its arguments,
 * shutdown delay, run and log directories); krusty starts no daemon, so they are kept for the
 * caller and nothing else reads them. A new builder answers the defaults of the Kotlin daemon's own
 * options: its run and log directories and the log file limits, and null for the JVM arguments and
 * the shutdown delay, which are left to the daemon. An option the policy does not declare is an
 * {@link IllegalStateException}.
 */
final class DaemonPolicy implements ExecutionPolicy.WithDaemon {
    private static final String OWNER = "org.jetbrains.kotlin.buildtools.api.ExecutionPolicy.WithDaemon";

    private final OptionValues options;

    private DaemonPolicy(OptionValues options) {
        this.options = options;
    }

    @Override
    public ExecutionPolicy.WithDaemon.Builder toBuilder() {
        return new Builder(options.copy());
    }

    @Override
    public <V> V get(Option<V> option) {
        return options.get(option);
    }

    static final class Builder implements ExecutionPolicy.WithDaemon.Builder {
        private final OptionValues options;

        Builder() {
            this(OptionValues.withDefaults(OWNER, defaults()));
        }

        private Builder(OptionValues options) {
            this.options = options;
        }

        @Override
        public <V> V get(Option<V> option) {
            return options.get(option);
        }

        @Override
        public <V> void set(Option<V> option, V value) {
            options.set(option, value);
        }

        @Override
        public ExecutionPolicy.WithDaemon build() {
            return new DaemonPolicy(options.copy());
        }
    }

    private static Map<String, Object> defaults() {
        Map<String, Object> defaults = new HashMap<>();
        defaults.put(ExecutionPolicy.WithDaemon.JVM_ARGUMENTS.getId(), null);
        defaults.put(ExecutionPolicy.WithDaemon.SHUTDOWN_DELAY_MILLIS.getId(), null);
        defaults.put(ExecutionPolicy.WithDaemon.DAEMON_RUN_DIR_PATH.getId(), runFilesPath());
        defaults.put(ExecutionPolicy.WithDaemon.LOGS_PATH.getId(), Paths.get(tempPath()));
        defaults.put(ExecutionPolicy.WithDaemon.LOGS_FILE_SIZE_LIMIT.getId(), 1048576L);
        defaults.put(ExecutionPolicy.WithDaemon.LOGS_FILE_COUNT_LIMIT.getId(), 3);
        return defaults;
    }

    /**
     * Where the Kotlin daemon keeps its run files: {@code kotlin/daemon} below the platform's user
     * data directory when that exists, else {@code .kotlin/daemon} in the home directory.
     */
    private static Path runFilesPath() {
        File base = new File(dataDirectory());
        File directory = base.isDirectory()
            ? new File(new File(base, "kotlin"), "daemon")
            : new File(new File(homePath(), ".kotlin"), "daemon");
        return Paths.get(directory.getPath());
    }

    private static String dataDirectory() {
        String system = System.getProperty("os.name", "").toLowerCase(java.util.Locale.ROOT);
        if (system.startsWith("windows")) {
            return orDefault(System.getenv("LOCALAPPDATA"), tempPath());
        }
        if (system.startsWith("mac")) {
            return homePath() + "/Library/Application Support";
        }
        if (system.contains("linux") || system.contains("bsd") || system.contains("nix")) {
            return orDefault(System.getenv("XDG_DATA_HOME"), homePath() + "/.local/share");
        }
        return tempPath();
    }

    private static String orDefault(String value, String fallback) {
        return value == null || value.isBlank() ? fallback : value;
    }

    private static String homePath() {
        return System.getProperty("user.home", "");
    }

    private static String tempPath() {
        return System.getProperty("java.io.tmpdir", "");
    }
}
