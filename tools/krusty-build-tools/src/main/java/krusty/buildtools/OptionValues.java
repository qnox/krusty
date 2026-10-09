package krusty.buildtools;

import java.util.HashMap;
import java.util.Map;
import org.jetbrains.kotlin.buildtools.api.internal.BaseOption;

/** The values set on a builder's typed options, keyed by option id. Copied when a builder builds. */
final class OptionValues {
    private final Map<String, Object> values;

    OptionValues() {
        this(new HashMap<>());
    }

    private OptionValues(Map<String, Object> values) {
        this.values = values;
    }

    OptionValues copy() {
        return new OptionValues(new HashMap<>(values));
    }

    <V> V get(BaseOption<V> option) {
        return (V) values.get(option.getId());
    }

    <V> void set(BaseOption<V> option, V value) {
        values.put(option.getId(), value);
    }
}
