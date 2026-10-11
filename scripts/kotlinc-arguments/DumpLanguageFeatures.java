import java.lang.reflect.Method;
import java.util.Map;

/** Prints kotlinc's `LanguageFeature` table, in declaration order, read from the compiler itself. */
public class DumpLanguageFeatures {
    public static void main(String[] args) throws Exception {
        Class<?> feature = Class.forName("org.jetbrains.kotlin.config.LanguageFeature");
        // The argument `LanguageFeatureMessageRenderer` names for enabling a feature, from the
        // compiler's own `@Enables` map; a feature without one is named `-XXLanguage:+Feature`.
        Map<?, ?> flags = (Map<?, ?>) Class.forName("org.jetbrains.kotlin.diagnostics.rendering.RuntimeFeatureToFlagMapKt")
            .getMethod("buildRuntimeFeatureToFlagMap", ClassLoader.class)
            .invoke(null, feature.getClassLoader());
        System.out.println("# name\tsinceVersion\tsinceApiVersion\tprogressive\tforcesPreReleaseBinaries\tforcesPreReleaseBinariesBefore\ttestOnly\tbehaviorAfterSinceVersion\thintUrl\tflag\tpresentableName");
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
                // Last: every feature has one, so no row ends in an empty cell's tab.
                text(get(feature, entry, "getPresentableName"))));
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

    private static String flag(Object value, String name) {
        return Boolean.TRUE.equals(value) ? name : "";
    }

    private static String behavior(Object value) {
        return value == null ? "" : value.getClass().getSimpleName();
    }
}
