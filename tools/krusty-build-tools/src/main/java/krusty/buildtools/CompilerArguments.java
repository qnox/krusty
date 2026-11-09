package krusty.buildtools;

import java.util.ArrayList;
import java.util.List;
import org.jetbrains.kotlin.buildtools.api.arguments.CommonCompilerArguments.CommonCompilerArgument;
import org.jetbrains.kotlin.buildtools.api.arguments.CommonToolArguments.CommonToolArgument;
import org.jetbrains.kotlin.buildtools.api.arguments.JvmCompilerArguments;
import org.jetbrains.kotlin.buildtools.api.arguments.JvmCompilerArguments.JvmCompilerArgument;

/**
 * Compiler arguments in kotlinc's command-line form. krusty parses that form itself, so the strings
 * a build tool applies are forwarded verbatim; there is no second, typed model of them here to
 * drift from krusty's own parser. Typed access is rejected rather than approximated.
 */
final class CompilerArguments implements JvmCompilerArguments {
    private final List<String> strings;

    private CompilerArguments(List<String> strings) {
        this.strings = List.copyOf(strings);
    }

    @Override
    public List<String> toArgumentStrings() {
        return strings;
    }

    @Override
    public <V> V get(JvmCompilerArgument<V> argument) {
        throw typed(argument.getId());
    }

    @Override
    public boolean contains(JvmCompilerArgument<?> argument) {
        throw typed(argument.getId());
    }

    @Override
    public <V> V get(CommonCompilerArgument<V> argument) {
        throw typed(argument.getId());
    }

    @Override
    public boolean contains(CommonCompilerArgument<?> argument) {
        throw typed(argument.getId());
    }

    @Override
    public <V> V get(CommonToolArgument<V> argument) {
        throw typed(argument.getId());
    }

    @Override
    public boolean contains(CommonToolArgument<?> argument) {
        throw typed(argument.getId());
    }

    static UnsupportedOperationException typed(String id) {
        return new UnsupportedOperationException(
            "krusty takes compiler arguments as command-line strings (applyArgumentStrings), not typed argument " + id);
    }

    static final class Builder implements JvmCompilerArguments.Builder {
        private final List<String> strings = new ArrayList<>();

        @Override
        public void applyArgumentStrings(List<String> arguments) {
            strings.addAll(arguments);
        }

        /**
         * The Build Tools API's replacement for {@link #applyArgumentStrings} from Kotlin 2.5 on. It
         * replaces the value of each argument it names while keeping the others, which takes an
         * argument model to tell which strings an earlier application contributed to the same
         * argument. krusty keeps no such model here, so a builder that already holds arguments
         * refuses rather than guessing; the first application is forwarded verbatim. It is not an
         * {@code @Override} because the API of the Kotlin releases before 2.5 does not declare it.
         */
        public void applyCommandLineArguments(List<String> arguments) {
            if (!strings.isEmpty()) {
                throw new UnsupportedOperationException(
                    "krusty forwards compiler arguments as one command line and cannot replace individual"
                        + " arguments applied earlier; apply every argument in one applyCommandLineArguments call");
            }
            strings.addAll(arguments);
        }

        @Override
        public JvmCompilerArguments build() {
            return new CompilerArguments(strings);
        }

        @Override
        public <V> V get(JvmCompilerArgument<V> argument) {
            throw typed(argument.getId());
        }

        @Override
        public <V> void set(JvmCompilerArgument<V> argument, V value) {
            throw typed(argument.getId());
        }

        @Override
        public boolean contains(JvmCompilerArgument<?> argument) {
            throw typed(argument.getId());
        }

        @Override
        public <V> V get(CommonCompilerArgument<V> argument) {
            throw typed(argument.getId());
        }

        @Override
        public <V> void set(CommonCompilerArgument<V> argument, V value) {
            throw typed(argument.getId());
        }

        @Override
        public boolean contains(CommonCompilerArgument<?> argument) {
            throw typed(argument.getId());
        }

        @Override
        public <V> V get(CommonToolArgument<V> argument) {
            throw typed(argument.getId());
        }

        @Override
        public <V> void set(CommonToolArgument<V> argument, V value) {
            throw typed(argument.getId());
        }

        @Override
        public boolean contains(CommonToolArgument<?> argument) {
            throw typed(argument.getId());
        }
    }
}
