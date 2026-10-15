package krusty.pluginapi;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldNode;
import org.objectweb.asm.tree.MethodNode;

/**
 * Decides what the API surface jar contains, from the reference compiler and the plugins that must
 * link against it.
 *
 * <p>Every compiler type a plugin names is in the surface with its exact binary shape: name, kind
 * (interface, abstract, final, enum) and supertypes, because a plugin's bytecode fixes all of them
 * ({@code invokeinterface} versus {@code invokevirtual}, {@code extends} versus {@code implements}).
 * Within that shape a type is one of two things:
 *
 * <ul>
 *   <li><b>data</b>: a self-contained compiler class (names, ids, enums, persistent collections)
 *       whose whole dependency closure needs nothing but the JDK, the Kotlin standard library and
 *       other data classes. It is copied with its implementation, because it carries no compiler.
 *   <li><b>contract</b>: every other type. It keeps only the members plugins use, plus everything
 *       a plugin can override on the types plugins extend. Abstract members stay abstract; concrete
 *       ones are bodies an implementation of the surface provides.
 * </ul>
 */
final class SurfaceModel {
    /** A member the surface declares on {@code owner}. */
    record Member(String name, String desc) implements Comparable<Member> {
        @Override
        public int compareTo(Member o) {
            return (name + desc).compareTo(o.name + o.desc);
        }
    }

    final ClassIndex compiler;
    /** The JDK and the Kotlin standard library. */
    final Classpath platform;
    /** The reference compiler on top of the platform: where plugin references resolve. */
    private final Classpath linked;
    /** Contract types and the members each keeps. */
    final Map<String, Set<Member>> contracts = new TreeMap<>();
    /** Data classes copied whole. */
    final Set<String> data = new TreeSet<>();
    /** Contract types a plugin extends or implements: their constructors must succeed. */
    final Set<String> extensible = new TreeSet<>();
    /** References that do not resolve in the reference compiler at all, per plugin jar. */
    final Map<String, List<PluginReferences.Ref>> unresolved = new TreeMap<>();

    private final Set<String> dataCapable;
    private CodeCopies copies;
    private Map<String, Set<Member>> copied = Map.of();
    private final Deque<String> pending = new ArrayDeque<>();

    SurfaceModel(ClassIndex compiler, ClassIndex stdlib, Map<String, PluginReferences> plugins) {
        this.compiler = compiler;
        this.platform = new Classpath(List.of(stdlib));
        this.linked = new Classpath(List.of(compiler, stdlib));
        Set<String> named = new TreeSet<>();
        for (PluginReferences p : plugins.values()) {
            p.types.stream().filter(compiler::contains).forEach(named::add);
        }
        this.dataCapable = selfContained(named);
        named.forEach(this::require);
        for (var e : plugins.entrySet()) {
            for (String t : e.getValue().extended) {
                if (compiler.contains(t) && !dataCapable.contains(t)) {
                    extensible.add(t);
                    overridable(t, new HashSet<>());
                }
            }
            for (PluginReferences.Ref ref : e.getValue().refs) {
                if (!compiler.contains(ref.owner())) {
                    continue;
                }
                Classpath.Resolved declaring = ref.access().isField()
                        ? linked.resolveField(ref.owner(), ref.name(), ref.desc())
                        : linked.resolveMethod(ref.owner(), ref.name(), ref.desc());
                if (declaring == null) {
                    unresolved.computeIfAbsent(e.getKey(), k -> new ArrayList<>()).add(ref);
                } else if (compiler.contains(declaring.owner())) {
                    member(declaring.owner(), ref.name(), ref.desc());
                }
            }
        }
        drain();
        // Copying a member can name new types, whose own members may then be copyable: iterate.
        while (true) {
            CodeCopies round = new CodeCopies(this);
            Map<String, Set<Member>> copies = round.compute();
            boolean grew = false;
            for (var e : copies.entrySet()) {
                for (Member m : e.getValue()) {
                    if (!m.name().equals("<clinit>") && contracts.get(e.getKey()).add(m)) {
                        pending.add(m.desc());
                        grew = true;
                    }
                }
            }
            drain();
            if (!grew) {
                this.copies = round;
                this.copied = copies;
                break;
            }
        }
    }

    /** Whether the contract member keeps its compiler code (see {@link CodeCopies}). */
    boolean isCopied(String owner, String name, String desc) {
        return copied.getOrDefault(owner, Set.of()).contains(new Member(name, desc));
    }

    /** The compiler's own declaration of a copied member's class, with code. */
    ClassNode withCode(String owner) {
        return copies.withCode(owner);
    }

    boolean isData(String name) {
        return data.contains(name);
    }

    boolean inSurface(String name) {
        return data.contains(name) || contracts.containsKey(name);
    }

    /** Brings a compiler type into the surface: a data class with its closure, or a contract type. */
    private void require(String name) {
        if (!compiler.contains(name) || inSurface(name)) {
            return;
        }
        if (dataCapable.contains(name)) {
            data.add(name);
            for (String dep : dependencies(name)) {
                require(dep);
            }
            return;
        }
        contracts.put(name, new TreeSet<>());
        ClassNode c = compiler.get(name);
        if (c.superName != null) {
            require(c.superName);
        }
        c.interfaces.forEach(this::require);
        if ((c.access & Opcodes.ACC_ENUM) != 0) {
            // A when over an enum switches on ordinal(): every constant keeps its declared position.
            for (FieldNode f : c.fields) {
                if ((f.access & Opcodes.ACC_ENUM) != 0) {
                    member(name, f.name, f.desc);
                }
            }
            member(name, "values", "()[L" + name + ";");
            member(name, "valueOf", "(Ljava/lang/String;)L" + name + ";");
        }
    }

    private void member(String owner, String name, String desc) {
        require(owner);
        if (data.contains(owner)) {
            return;
        }
        if (contracts.get(owner).add(new Member(name, desc))) {
            pending.add(desc);
        }
    }

    /** Every member a plugin may override or call through a type it extends, along the whole chain. */
    private void overridable(String type, Set<String> seen) {
        if (!compiler.contains(type) || !seen.add(type) || dataCapable.contains(type)) {
            return;
        }
        ClassNode c = compiler.get(type);
        for (MethodNode m : c.methods) {
            boolean visible = (m.access & Opcodes.ACC_PRIVATE) == 0;
            if (visible && !m.name.equals("<clinit>") && (m.access & Opcodes.ACC_STATIC) == 0) {
                member(type, m.name, m.desc);
            }
        }
        if (c.superName != null) {
            overridable(c.superName, seen);
        }
        c.interfaces.forEach(i -> overridable(i, seen));
    }

    private void drain() {
        while (!pending.isEmpty()) {
            descriptorTypes(Type.getType(pending.poll()));
        }
    }

    private void descriptorTypes(Type t) {
        switch (t.getSort()) {
            case Type.METHOD -> {
                descriptorTypes(t.getReturnType());
                for (Type a : t.getArgumentTypes()) {
                    descriptorTypes(a);
                }
            }
            case Type.ARRAY -> descriptorTypes(t.getElementType());
            case Type.OBJECT -> require(t.getInternalName());
            default -> { }
        }
    }

    /** Whether the JVM running a plugin provides {@code name}: the JDK or the Kotlin standard library. */
    boolean isPlatform(String name) {
        return !compiler.contains(name) && platform.find(name) != null;
    }

    /**
     * The compiler classes that can be copied whole: the greatest set, among everything reachable
     * from {@code roots}, in which every class depends only on the platform and on other members.
     */
    private Set<String> selfContained(Set<String> roots) {
        Map<String, Set<String>> deps = new HashMap<>();
        Deque<String> work = new ArrayDeque<>(roots);
        while (!work.isEmpty()) {
            String c = work.poll();
            if (deps.containsKey(c)) {
                continue;
            }
            Set<String> d = dependencies(c);
            deps.put(c, d);
            for (String n : d) {
                if (compiler.contains(n) && !deps.containsKey(n)) {
                    work.add(n);
                }
            }
        }
        Set<String> capable = new HashSet<>(deps.keySet());
        boolean changed = true;
        while (changed) {
            changed = false;
            for (var it = capable.iterator(); it.hasNext(); ) {
                String c = it.next();
                for (String d : deps.get(c)) {
                    if (!isPlatform(d) && !capable.contains(d)) {
                        it.remove();
                        changed = true;
                        break;
                    }
                }
            }
        }
        return capable;
    }

    private final Map<String, Set<String>> dependencyCache = new HashMap<>();

    /** The classes a compiler class's constant pool names, itself excluded. */
    Set<String> dependencies(String name) {
        return dependencyCache.computeIfAbsent(name, n -> {
            Set<String> out = new HashSet<>();
            ClassReader r = new ClassReader(compiler.bytes(n));
            char[] buf = new char[r.getMaxStringLength()];
            for (int i = 1; i < r.getItemCount(); i++) {
                int offset = r.getItem(i);
                if (offset == 0) {
                    continue;
                }
                int tag = r.readByte(offset - 1);
                if (tag == 7) { // CONSTANT_Class
                    addTypeName(out, r.readUTF8(offset, buf));
                } else if (tag == 12) { // CONSTANT_NameAndType: the descriptor
                    addDescriptor(out, r.readUTF8(offset + 2, buf));
                } else if (tag == 16) { // CONSTANT_MethodType
                    addDescriptor(out, r.readUTF8(offset, buf));
                }
            }
            // A class's own field and method descriptors are plain UTF-8 entries, not references.
            ClassNode node = compiler.get(n);
            node.fields.forEach(f -> addDescriptor(out, f.desc));
            node.methods.forEach(m -> addDescriptor(out, m.desc));
            out.remove(n);
            return out;
        });
    }

    private static void addTypeName(Set<String> out, String name) {
        if (name.startsWith("[")) {
            addDescriptor(out, name);
        } else {
            out.add(name);
        }
    }

    private static void addDescriptor(Set<String> out, String desc) {
        Type t;
        try {
            t = Type.getType(desc);
        } catch (RuntimeException e) {
            return;
        }
        switch (t.getSort()) {
            case Type.METHOD -> {
                addDescriptor(out, t.getReturnType().getDescriptor());
                for (Type a : t.getArgumentTypes()) {
                    addDescriptor(out, a.getDescriptor());
                }
            }
            case Type.ARRAY -> addDescriptor(out, t.getElementType().getDescriptor());
            case Type.OBJECT -> out.add(t.getInternalName());
            default -> { }
        }
    }
}
