package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSName;

// KSP's name: a dot-separated qualified name, split at its last dot.
final class Name implements KSName {
    private final String name;

    Name(String name) {
        this.name = name;
    }

    @Override
    public String asString() {
        return name;
    }

    @Override
    public String getQualifier() {
        int dot = name.lastIndexOf('.');
        return dot < 0 ? "" : name.substring(0, dot);
    }

    @Override
    public String getShortName() {
        int dot = name.lastIndexOf('.');
        return dot < 0 ? name : name.substring(dot + 1);
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof Name && ((Name) other).name.equals(name);
    }

    @Override
    public int hashCode() {
        return name.hashCode();
    }

    @Override
    public String toString() {
        return name;
    }
}
