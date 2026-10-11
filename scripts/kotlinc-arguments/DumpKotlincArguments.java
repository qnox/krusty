import java.lang.annotation.Annotation;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;

/** Prints kotlinc's JVM argument surface, read from the compiler's own `@Argument` fields. */
public class DumpKotlincArguments {
    @SuppressWarnings("unchecked")
    public static void main(String[] args) throws Exception {
        String pkg = "org.jetbrains.kotlin.cli.common.arguments.";
        Class<? extends Annotation> argument = (Class<? extends Annotation>) Class.forName(pkg + "Argument");
        Class<? extends Annotation> enables = (Class<? extends Annotation>) Class.forName(pkg + "Enables");
        Class<? extends Annotation> disables = (Class<? extends Annotation>) Class.forName(pkg + "Disables");
        TreeMap<String, String> rows = new TreeMap<>();
        dump(Class.forName(pkg + "K2JVMCompilerArguments"), "jvm", argument, enables, disables, rows);
        // kotlinc still parses an argument it has removed, then warns that it has no effect.
        // Releases before the removed-argument registry have no such class.
        try {
            dump(Class.forName(pkg + "RemovedCompilerArguments"), "removed", argument, enables, disables, rows);
        } catch (ClassNotFoundException absent) {
            // Nothing to add.
        }
        System.out.println("# name\tshort\tdeprecated\ttype\tdelimiter\tdeprecatedIn\tremovedIn\tobsolete\tdeprecation\tenables\tdisables\torigin");
        rows.values().forEach(System.out::println);
    }

    private static void dump(
        Class<?> klass,
        String origin,
        Class<? extends Annotation> argument,
        Class<? extends Annotation> enables,
        Class<? extends Annotation> disables,
        TreeMap<String, String> rows
    ) throws Exception {
        for (Class<?> c = klass; c != Object.class; c = c.getSuperclass()) {
            for (Field field : c.getDeclaredFields()) {
                Annotation a = field.getAnnotation(argument);
                if (a == null) continue;
                Class<?> t = field.getType();
                String type = t == boolean.class || t == Boolean.class ? "bool"
                    : t == String.class ? "string"
                    : t == String[].class ? "array" : t.getName();
                rows.putIfAbsent(get(a, "value"), String.join("\t",
                    get(a, "value"), get(a, "shortName"), get(a, "deprecatedName"), type,
                    get(a, "delimiter"), get(a, "deprecatedVersion"), get(a, "removedVersion"),
                    get(a, "isObsolete").equals("true") ? "obsolete" : "",
                    deprecationMessage(c, field),
                    features(field.getAnnotationsByType(enables)),
                    features(field.getAnnotationsByType(disables)),
                    origin));
            }
        }
    }

    /** Kotlin puts `@Deprecated` on the property getter, whose message kotlinc appends to its warning. */
    private static String deprecationMessage(Class<?> owner, Field field) {
        String name = field.getName();
        String getter = (name.startsWith("is") && name.length() > 2 && Character.isUpperCase(name.charAt(2)))
            ? name : "get" + Character.toUpperCase(name.charAt(0)) + name.substring(1);
        try {
            for (Annotation a : owner.getMethod(getter).getAnnotations()) {
                if (a.annotationType().getName().equals("kotlin.Deprecated")) {
                    return get(a, "level") + ":" + get(a, "message");
                }
            }
        } catch (Exception absent) {
            return "";
        }
        return "";
    }

    private static String features(Annotation[] annotations) throws Exception {
        List<String> parts = new ArrayList<>();
        for (Annotation a : annotations) {
            String value = get(a, "ifValueIs");
            parts.add(get(a, "feature") + (value.isEmpty() ? "" : "=" + value));
        }
        return String.join(";", parts);
    }

    /** An attribute the running compiler's annotation lacks (older releases) reads as empty. */
    private static String get(Annotation a, String name) throws Exception {
        try {
            return String.valueOf(a.annotationType().getMethod(name).invoke(a));
        } catch (NoSuchMethodException absent) {
            return "";
        }
    }
}
