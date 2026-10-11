package krusty.pluginapi;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.tree.ClassNode;

/** The classes of one jar, parsed on demand and keyed by internal name. */
final class ClassIndex implements AutoCloseable {
    private final ZipFile zip;
    private final Set<String> names = new TreeSet<>();
    private final Map<String, ClassNode> parsed = new HashMap<>();
    private final int parsing;

    /** {@code withCode} keeps method bodies; declarations alone are enough for the compiler jar. */
    ClassIndex(Path jar, boolean withCode) {
        this.parsing = withCode ? 0 : ClassReader.SKIP_CODE | ClassReader.SKIP_DEBUG | ClassReader.SKIP_FRAMES;
        try {
            zip = new ZipFile(jar.toFile());
        } catch (IOException e) {
            throw new UncheckedIOException("cannot open " + jar, e);
        }
        zip.stream()
                .map(ZipEntry::getName)
                .filter(n -> n.endsWith(".class") && !n.startsWith("META-INF/") && !n.endsWith("module-info.class"))
                .forEach(n -> names.add(n.substring(0, n.length() - ".class".length())));
    }

    Set<String> names() {
        return names;
    }

    boolean contains(String internalName) {
        return names.contains(internalName);
    }

    /** The class with its code, or {@code null} when this jar does not declare it. */
    ClassNode get(String internalName) {
        if (!names.contains(internalName)) {
            return null;
        }
        return parsed.computeIfAbsent(internalName, n -> {
            ClassNode node = new ClassNode();
            new ClassReader(bytes(n)).accept(node, parsing);
            return node;
        });
    }

    byte[] bytes(String internalName) {
        try (var in = zip.getInputStream(zip.getEntry(internalName + ".class"))) {
            return in.readAllBytes();
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    /** Releases a parsed class that will not be visited again. */
    void forget(String internalName) {
        parsed.remove(internalName);
    }

    @Override
    public void close() throws IOException {
        zip.close();
    }
}
