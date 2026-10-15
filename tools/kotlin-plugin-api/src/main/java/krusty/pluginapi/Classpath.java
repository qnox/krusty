package krusty.pluginapi;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldNode;
import org.objectweb.asm.tree.MethodNode;

/**
 * Declarations visible to a plugin at run time: the given jars in order, then the running JDK. Member
 * resolution follows the JVM specification (5.4.3.3 and 5.4.3.4) over these declarations, so a
 * method inherited from {@code java.lang.Object} or declared by a standard-library interface
 * resolves exactly where the JVM would find it, and nowhere else.
 */
final class Classpath {
    /** Where a member resolved: its declaring class and access flags. */
    record Resolved(String owner, int access) {
    }

    private final List<ClassIndex> jars;
    private final Map<String, ClassNode> jdk = new HashMap<>();

    Classpath(List<ClassIndex> jars) {
        this.jars = jars;
    }

    ClassNode find(String name) {
        for (ClassIndex jar : jars) {
            ClassNode c = jar.get(name);
            if (c != null) {
                return c;
            }
        }
        return jdk.computeIfAbsent(name, Classpath::jdkClass);
    }

    private static ClassNode jdkClass(String name) {
        // A resource ending in .class is never encapsulated by the module system.
        try (InputStream in = ClassLoader.getSystemResourceAsStream(name + ".class")) {
            if (in == null || !(name.startsWith("java/") || name.startsWith("javax/") || name.startsWith("jdk/"))) {
                return null;
            }
            ClassNode node = new ClassNode();
            new ClassReader(in.readAllBytes()).accept(node, ClassReader.SKIP_CODE | ClassReader.SKIP_DEBUG);
            return node;
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    Resolved resolveMethod(String owner, String name, String desc) {
        for (String c = owner; c != null; ) {
            ClassNode node = find(c);
            if (node == null) {
                return null;
            }
            MethodNode m = declared(node, name, desc);
            if (m != null) {
                return new Resolved(c, m.access);
            }
            c = node.superName;
        }
        return resolveInSuperinterfaces(owner, name, desc, new HashSet<>());
    }

    private Resolved resolveInSuperinterfaces(String type, String name, String desc, Set<String> seen) {
        ClassNode node = find(type);
        if (node == null || !seen.add(type)) {
            return null;
        }
        for (String i : node.interfaces) {
            ClassNode in = find(i);
            if (in == null) {
                continue;
            }
            MethodNode m = declared(in, name, desc);
            if (m != null && (m.access & (Opcodes.ACC_PRIVATE | Opcodes.ACC_STATIC)) == 0) {
                return new Resolved(i, m.access);
            }
            Resolved deeper = resolveInSuperinterfaces(i, name, desc, seen);
            if (deeper != null) {
                return deeper;
            }
        }
        return node.superName == null ? null : resolveInSuperinterfaces(node.superName, name, desc, seen);
    }

    Resolved resolveField(String owner, String name, String desc) {
        return resolveField(owner, name, desc, new HashSet<>());
    }

    private Resolved resolveField(String owner, String name, String desc, Set<String> seen) {
        ClassNode node = find(owner);
        if (node == null || !seen.add(owner)) {
            return null;
        }
        for (FieldNode f : node.fields) {
            if (f.name.equals(name) && f.desc.equals(desc)) {
                return new Resolved(owner, f.access);
            }
        }
        for (String i : node.interfaces) {
            Resolved found = resolveField(i, name, desc, seen);
            if (found != null) {
                return found;
            }
        }
        return node.superName == null ? null : resolveField(node.superName, name, desc, seen);
    }

    private static MethodNode declared(ClassNode c, String name, String desc) {
        for (MethodNode m : c.methods) {
            if (m.name.equals(name) && m.desc.equals(desc)) {
                return m;
            }
        }
        return null;
    }
}
