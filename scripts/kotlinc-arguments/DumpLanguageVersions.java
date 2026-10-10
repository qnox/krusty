import java.lang.reflect.Method;

/**
 * Prints kotlinc's `LanguageVersion` and `ApiVersion` policy, in declaration order, read from the
 * compiler itself: which levels are stable, deprecated, or no longer supported. A cell is never
 * empty (`-` marks an unset flag), so no row ends in a tab.
 */
public class DumpLanguageVersions {
    public static void main(String[] args) throws Exception {
        Class<?> language = Class.forName("org.jetbrains.kotlin.config.LanguageVersion");
        Class<?> api = Class.forName("org.jetbrains.kotlin.config.ApiVersion");
        Method createByLanguageVersion = api.getMethod("createByLanguageVersion", language);
        System.out.println("# version\tstable\tdeprecated\tunsupported\tapiStable\tapiDeprecated\tapiUnsupported");
        for (Object entry : language.getEnumConstants()) {
            Object apiEntry = createByLanguageVersion.invoke(null, entry);
            System.out.println(String.join("\t",
                (String) language.getMethod("getVersionString").invoke(entry),
                flag(language, entry, "isStable", "stable"),
                flag(language, entry, "isDeprecated", "deprecated"),
                flag(language, entry, "isUnsupported", "unsupported"),
                flag(api, apiEntry, "isStable", "stable"),
                flag(api, apiEntry, "isDeprecated", "deprecated"),
                flag(api, apiEntry, "isUnsupported", "unsupported")));
        }
    }

    private static String flag(Class<?> type, Object entry, String getter, String name) throws Exception {
        return Boolean.TRUE.equals(type.getMethod(getter).invoke(entry)) ? name : "-";
    }
}
