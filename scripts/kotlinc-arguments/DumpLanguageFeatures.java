import java.io.DataInputStream;
import java.io.File;
import java.io.IOException;
import java.lang.reflect.Method;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

/** Prints kotlinc's `LanguageFeature` table, in declaration order, read from the compiler itself. */
public class DumpLanguageFeatures {
    public static void main(String[] args) throws Exception {
        Class<?> feature = Class.forName("org.jetbrains.kotlin.config.LanguageFeature");
        // The argument `LanguageFeatureMessageRenderer` names for enabling a feature, from the
        // compiler's own `@Enables` map; a feature without one is named `-XXLanguage:+Feature`.
        Map<?, ?> flags = featureFlags(feature.getClassLoader());
        Map<String, Set<String>> consumers = consumers(feature);
        System.out.println("# name\tsinceVersion\tsinceApiVersion\tprogressive\tforcesPreReleaseBinaries\tforcesPreReleaseBinariesBefore\ttestOnly\tbehaviorAfterSinceVersion\thintUrl\tflag\tissue\tconsumers\tpresentableName");
        for (Object entry : feature.getEnumConstants()) {
            System.out.println(String.join("\t",
                ((Enum<?>) entry).name(),
                version(get(feature, entry, "getSinceVersion")),
                version(get(feature, entry, "getSinceApiVersion")),
                flag(get(feature, entry, "getActuallyEnabledInProgressiveMode"), "progressive"),
                flag(get(feature, entry, "getForcesPreReleaseBinaries"), "prerelease"),
                version(get(feature, entry, "getForcesPreReleaseBinariesBefore")),
                flag(get(feature, entry, "getTestOnly"), "testOnly"),
                behavior(get(feature, entry, "getBehaviorAfterSinceVersion")),
                text(get(feature, entry, "getHintUrl")),
                text(flags.get(entry)),
                // `NO_ISSUE_SPECIFIED` is a placeholder kotlinc never prints; it reads as no issue.
                issue(get(feature, entry, "getIssue")),
                String.join(",", consumers.getOrDefault(((Enum<?>) entry).name(), new TreeSet<>())),
                // Last: every feature has one, so no row ends in an empty cell's tab.
                text(get(feature, entry, "getPresentableName"))));
        }
    }

    /**
     * The platforms whose compiler code names each feature, read from the compiler's own classes: a
     * class that mentions a feature's constant consumes it, and the package it lives in says for
     * which platform (`native`, `js`, `wasm`, `web` for both Kotlin/JS and Kotlin/Wasm, `jvm`; any
     * other package is `common`). The enum itself and the command-line argument classes that only
     * name features to toggle them consume nothing. A feature no class consumes has no entry.
     */
    private static Map<String, Set<String>> consumers(Class<?> feature) throws Exception {
        Set<String> names = new HashSet<>();
        for (Object entry : feature.getEnumConstants()) {
            names.add(((Enum<?>) entry).name());
        }
        File jar = new File(feature.getProtectionDomain().getCodeSource().getLocation().toURI());
        Map<String, Set<String>> consumers = new TreeMap<>();
        try (ZipFile zip = new ZipFile(jar)) {
            for (var entries = zip.entries(); entries.hasMoreElements(); ) {
                ZipEntry entry = entries.nextElement();
                String path = entry.getName();
                if (!path.endsWith(".class")
                    || path.startsWith("org/jetbrains/kotlin/config/LanguageFeature")
                    || path.startsWith("org/jetbrains/kotlin/cli/common/arguments/")) {
                    continue;
                }
                try (DataInputStream in = new DataInputStream(zip.getInputStream(entry))) {
                    for (String name : constantNames(in, names)) {
                        consumers.computeIfAbsent(name, key -> new TreeSet<>()).add(platform(path));
                    }
                }
            }
        }
        return consumers;
    }

    /** The package-to-platform rule of [consumers]. */
    private static String platform(String path) {
        if (path.contains("/native/")) return "native";
        if (path.contains("/wasm/")) return "wasm";
        if (path.contains("/web/")) return "web";
        if (path.contains("/js/")) return "js";
        if (path.contains("/jvm/") || path.contains("/fir/java/")) return "jvm";
        return "common";
    }

    /** The members of `names` among a class file's UTF-8 constants. */
    private static Set<String> constantNames(DataInputStream in, Set<String> names) throws IOException {
        Set<String> found = new HashSet<>();
        if (in.readInt() != 0xCAFEBABE) return found;
        in.skipBytes(4);
        int count = in.readUnsignedShort();
        for (int index = 1; index < count; index++) {
            int tag = in.readUnsignedByte();
            switch (tag) {
                case 1 -> {
                    String text = in.readUTF();
                    if (names.contains(text)) found.add(text);
                }
                case 3, 4, 9, 10, 11, 12, 17, 18 -> in.skipBytes(4);
                case 5, 6 -> {
                    in.skipBytes(8);
                    index++;
                }
                case 7, 8, 16, 19, 20 -> in.skipBytes(2);
                case 15 -> in.skipBytes(3);
                default -> throw new IOException("unknown constant pool tag " + tag);
            }
        }
        return found;
    }

    /**
     * The compiler's map from a feature to the argument that enables it. 2.4 reads
     * `RuntimeFeatureToFlagMapKt.buildRuntimeFeatureToFlagMap`; 2.5.0-RC publishes
     * `FeatureToFlagMapGeneratedKt.getFeatureToEnablingFlagMap` instead.
     */
    private static Map<?, ?> featureFlags(ClassLoader loader) throws Exception {
        try {
            return (Map<?, ?>) Class.forName("org.jetbrains.kotlin.diagnostics.rendering.RuntimeFeatureToFlagMapKt")
                .getMethod("buildRuntimeFeatureToFlagMap", ClassLoader.class)
                .invoke(null, loader);
        } catch (ClassNotFoundException moved) {
            return (Map<?, ?>) Class.forName("org.jetbrains.kotlin.diagnostics.rendering.FeatureToFlagMapGeneratedKt")
                .getMethod("getFeatureToEnablingFlagMap")
                .invoke(null);
        }
    }

    /** A property an older release does not declare reads as absent, which is how it behaves there. */
    private static Object get(Class<?> type, Object entry, String getter) throws Exception {
        Method method;
        try {
            method = type.getMethod(getter);
        } catch (NoSuchMethodException absent) {
            return null;
        }
        return method.invoke(entry);
    }

    /** `LanguageVersion` and `ApiVersion` both expose `getVersionString`. */
    private static String version(Object version) throws Exception {
        return version == null ? "" : (String) version.getClass().getMethod("getVersionString").invoke(version);
    }

    private static String text(Object value) {
        return value == null ? "" : (String) value;
    }

    private static String issue(Object value) {
        return value == null || "No YT issue".equals(value) ? "" : (String) value;
    }

    private static String flag(Object value, String name) {
        return Boolean.TRUE.equals(value) ? name : "";
    }

    private static String behavior(Object value) {
        return value == null ? "" : value.getClass().getSimpleName();
    }
}
