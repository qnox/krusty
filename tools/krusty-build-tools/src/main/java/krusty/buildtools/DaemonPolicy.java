package krusty.buildtools;

import org.jetbrains.kotlin.buildtools.api.ExecutionPolicy;

/**
 * The daemon execution policy. A build tool picks it to compile outside its own process, which is
 * where krusty always compiles: every operation runs one krusty child process, under this policy as
 * under the in-process one. The options configure the JVM of a Kotlin daemon (its arguments,
 * shutdown delay, run and log directories); krusty starts no daemon, so they are kept for the
 * caller and nothing else reads them.
 */
final class DaemonPolicy implements ExecutionPolicy.WithDaemon {
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
            this(new OptionValues());
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
}
