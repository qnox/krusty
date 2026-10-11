package krusty.pluginapi;

/**
 * Runtime support shipped inside the API surface jar. A contract member that no implementation
 * provides fails through {@link #unimplemented}, naming the exact member, instead of misbehaving.
 */
public final class Surface {
    private Surface() {
    }

    public static RuntimeException unimplemented(String member) {
        return new UnsupportedOperationException("not provided by the Kotlin plugin API surface: " + member);
    }
}
