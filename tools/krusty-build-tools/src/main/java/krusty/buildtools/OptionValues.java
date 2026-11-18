package krusty.buildtools;

import java.util.HashMap;
import java.util.Map;
import org.jetbrains.kotlin.buildtools.api.internal.BaseOption;

/** The values set on a builder's typed options, keyed by option id. Copied when a builder builds. */
final class OptionValues {
    private final Map<String, Object> values;
    private final String owner;

    OptionValues() {
        this(new HashMap<>(), null);
    }

    private OptionValues(Map<String, Object> values, String owner) {
        this.values = values;
        this.owner = owner;
    }

    /**
     * The values of a builder whose options all answer: those given by {@code defaults} until set,
     * and for an option that is not among them an {@link IllegalStateException}, as the Build Tools
     * API's own implementation answers for an option its builder does not declare. A default may be
     * null, which is then what the option reads.
     */
    static OptionValues withDefaults(String owner, Map<String, Object> defaults) {
        return new OptionValues(new HashMap<>(defaults), owner);
    }

    OptionValues copy() {
        return new OptionValues(new HashMap<>(values), owner);
    }

    <V> V get(BaseOption<V> option) {
        if (owner != null && !values.containsKey(option.getId())) {
            throw new IllegalStateException(option.getId() + " was not set in " + owner);
        }
        return (V) values.get(option.getId());
    }

    <V> void set(BaseOption<V> option, V value) {
        values.put(option.getId(), value);
    }
}
