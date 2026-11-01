import java.io.File;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

/**
 * Prints every diagnostic kotlinc's distribution declares, sorted by name: the name
 * `-Xrender-internal-diagnostic-names` prints and the factory's severity. Every jar in the
 * compiler's `lib` directory is scanned, so the bundled compiler plugins' diagnostics are read the
 * same way as the compiler's own: a diagnostic is a static field or a Kotlin `object` property
 * whose type is a diagnostic factory.
 */
public class DumpDiagnosticNames {
    private static Class<?> factory;
    private static Class<?> deprecation;

    public static void main(String[] args) throws Exception {
        factory = Class.forName("org.jetbrains.kotlin.diagnostics.AbstractKtDiagnosticFactory");
        deprecation = Class.forName("org.jetbrains.kotlin.diagnostics.KtDiagnosticFactoryForDeprecation");
        File lib = new File(factory.getProtectionDomain().getCodeSource().getLocation().toURI()).getParentFile();
        File[] jars = lib.listFiles((dir, name) -> name.endsWith(".jar"));
        List<URL> urls = new ArrayList<>();
        for (File jar : jars) {
            urls.add(jar.toURI().toURL());
        }
        ClassLoader loader = new URLClassLoader(urls.toArray(new URL[0]), factory.getClassLoader());
        Map<String, String> severities = new TreeMap<>();
        for (File jar : jars) {
            try (JarFile file = new JarFile(jar)) {
                Enumeration<JarEntry> entries = file.entries();
                while (entries.hasMoreElements()) {
                    String entry = entries.nextElement().getName();
                    if (entry.endsWith(".class") && !entry.startsWith("META-INF/")) {
                        scan(loader, entry.substring(0, entry.length() - 6).replace('/', '.'), severities);
                    }
                }
            }
        }
        System.out.println("# name\tseverity");
        for (Map.Entry<String, String> row : severities.entrySet()) {
            System.out.println(row.getKey() + "\t" + row.getValue());
        }
    }

    private static void scan(ClassLoader loader, String className, Map<String, String> severities) throws Exception {
        Class<?> type;
        Field[] fields;
        Method[] methods;
        try {
            type = Class.forName(className, false, loader);
            fields = type.getDeclaredFields();
            methods = type.getDeclaredMethods();
        } catch (Throwable unlinkable) {
            // A class whose dependencies are not in the distribution cannot declare a diagnostic
            // kotlinc reports.
            return;
        }
        for (Field field : fields) {
            if (Modifier.isStatic(field.getModifiers()) && declares(field.getType())) {
                field.setAccessible(true);
                add(field.get(null), severities);
            }
        }
        // A Kotlin `object` exposes a delegated diagnostic property only through its getter.
        Object instance = null;
        for (Method method : methods) {
            if (method.getParameterCount() != 0 || !Modifier.isPublic(method.getModifiers())
                    || !declares(method.getReturnType())) {
                continue;
            }
            if (!Modifier.isStatic(method.getModifiers()) && instance == null) {
                try {
                    instance = type.getField("INSTANCE").get(null);
                } catch (NoSuchFieldException notAnObject) {
                    continue;
                }
            }
            add(method.invoke(Modifier.isStatic(method.getModifiers()) ? null : instance), severities);
        }
    }

    private static boolean declares(Class<?> type) {
        return factory.isAssignableFrom(type) || deprecation.isAssignableFrom(type);
    }

    /** A deprecation pair declares its warning and its error as two diagnostics. */
    private static void add(Object diagnostic, Map<String, String> severities) throws Exception {
        if (deprecation.isInstance(diagnostic)) {
            add(deprecation.getMethod("getWarningFactory").invoke(diagnostic), severities);
            add(deprecation.getMethod("getErrorFactory").invoke(diagnostic), severities);
            return;
        }
        String name = (String) factory.getMethod("getName").invoke(diagnostic);
        String severity = factory.getMethod("getSeverity").invoke(diagnostic).toString();
        String previous = severities.put(name, severity);
        if (previous != null && !previous.equals(severity)) {
            throw new IllegalStateException(name + " is declared as both " + previous + " and " + severity);
        }
    }
}
