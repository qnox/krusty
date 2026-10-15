package krusty.pluginapi;

import java.util.ArrayDeque;
import java.util.Deque;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.tree.AbstractInsnNode;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldInsnNode;
import org.objectweb.asm.tree.FrameNode;
import org.objectweb.asm.tree.InvokeDynamicInsnNode;
import org.objectweb.asm.tree.LdcInsnNode;
import org.objectweb.asm.tree.MethodInsnNode;
import org.objectweb.asm.tree.MethodNode;
import org.objectweb.asm.tree.MultiANewArrayInsnNode;
import org.objectweb.asm.tree.TryCatchBlockNode;
import org.objectweb.asm.tree.TypeInsnNode;

/**
 * Finds the contract members whose compiler code can be kept as it is: code that only stores and
 * reads its own class's state, calls the platform and data classes, and chains to other kept
 * constructors and members of its own class. Such a member does no compiler work, so keeping it
 * gives plugins real names, keys, companions and constructors instead of stubs.
 *
 * <p>Code that calls into any other contract type stays a stub: that call is a service an
 * implementation of the surface provides. A method that reads its own instance state is kept only
 * when every constructor of its class is kept too, so it never observes a stub's empty state.
 */
final class CodeCopies {
    /** A method of a contract class, keyed for the fixed point. */
    record Key(String owner, String name, String desc) {
        SurfaceModel.Member member() {
            return new SurfaceModel.Member(name, desc);
        }
    }

    private record Analysis(boolean copyable, Set<Key> needs, Set<Key> fields) {
    }

    private final SurfaceModel model;
    private final Map<String, ClassNode> code = new HashMap<>();
    private final Map<Key, Analysis> analyses = new HashMap<>();
    private final Classpath declarations;

    CodeCopies(SurfaceModel model) {
        this.model = model;
        this.declarations = new Classpath(List.of(model.compiler));
    }

    /** The class with method bodies, for the members that are copied. */
    ClassNode withCode(String owner) {
        return code.computeIfAbsent(owner, o -> {
            ClassNode node = new ClassNode();
            new ClassReader(model.compiler.bytes(o)).accept(node, ClassReader.SKIP_DEBUG);
            return node;
        });
    }

    /**
     * The copyable members reachable from the kept members and static initializers, and the fields
     * they touch, per owner. The caller adds them to the surface.
     */
    Map<String, Set<SurfaceModel.Member>> compute() {
        Deque<Key> work = new ArrayDeque<>();
        for (var e : model.contracts.entrySet()) {
            for (SurfaceModel.Member m : e.getValue()) {
                work.add(new Key(e.getKey(), m.name(), m.desc()));
            }
            work.add(new Key(e.getKey(), "<clinit>", "()V"));
        }
        while (!work.isEmpty()) {
            Key k = work.poll();
            if (analyses.containsKey(k)) {
                continue;
            }
            Analysis a = analyze(k);
            analyses.put(k, a);
            work.addAll(a.needs());
        }
        Set<Key> copyable = new HashSet<>();
        analyses.forEach((k, a) -> {
            if (a.copyable()) {
                copyable.add(k);
            }
        });
        boolean changed = true;
        while (changed) {
            changed = false;
            for (var it = copyable.iterator(); it.hasNext(); ) {
                Key k = it.next();
                if (!copyable.containsAll(analyses.get(k).needs())) {
                    it.remove();
                    changed = true;
                }
            }
        }
        Map<String, Set<SurfaceModel.Member>> out = new HashMap<>();
        for (Key k : copyable) {
            out.computeIfAbsent(k.owner(), o -> new HashSet<>()).add(k.member());
            for (Key f : analyses.get(k).fields()) {
                out.computeIfAbsent(f.owner(), o -> new HashSet<>()).add(f.member());
            }
        }
        return out;
    }

    private Analysis analyze(Key k) {
        Analysis no = new Analysis(false, Set.of(), Set.of());
        if (!model.contracts.containsKey(k.owner())) {
            return no;
        }
        ClassNode c = withCode(k.owner());
        MethodNode m = method(c, k.name(), k.desc());
        if (m == null || m.instructions.size() == 0 || (m.access & Opcodes.ACC_NATIVE) != 0) {
            return no;
        }
        Set<Key> needs = new HashSet<>();
        Set<Key> fields = new HashSet<>();
        for (TryCatchBlockNode t : m.tryCatchBlocks) {
            if (t.type != null && !known(t.type)) {
                return no;
            }
        }
        if (!knownDescriptor(m.desc)) {
            return no;
        }
        for (AbstractInsnNode insn : m.instructions) {
            switch (insn) {
                case MethodInsnNode mi -> {
                    if (!call(c, mi, needs)) {
                        return no;
                    }
                }
                case FieldInsnNode fi -> {
                    if (!field(c, fi, needs, fields)) {
                        return no;
                    }
                }
                case TypeInsnNode ti -> {
                    if (!known(ti.desc)) {
                        return no;
                    }
                }
                case MultiANewArrayInsnNode ma -> {
                    if (!knownDescriptor(ma.desc)) {
                        return no;
                    }
                }
                case LdcInsnNode ldc -> {
                    if (ldc.cst instanceof Type t && !knownDescriptor(t.getDescriptor())) {
                        return no;
                    }
                }
                case InvokeDynamicInsnNode ignored -> {
                    return no;
                }
                case FrameNode f -> {
                    if (!knownFrame(f.local) || !knownFrame(f.stack)) {
                        return no;
                    }
                }
                default -> { }
            }
        }
        return new Analysis(true, needs, fields);
    }

    private boolean call(ClassNode c, MethodInsnNode mi, Set<Key> needs) {
        if (mi.owner.startsWith("[")) {
            return knownDescriptor(mi.owner) && knownDescriptor(mi.desc);
        }
        if (!knownDescriptor(mi.desc)) {
            return false;
        }
        if (model.isPlatform(mi.owner) || model.isData(mi.owner)) {
            return true;
        }
        if (!model.contracts.containsKey(mi.owner)) {
            return false;
        }
        boolean constructor = mi.name.equals("<init>");
        if (!constructor && !mi.owner.equals(c.name)) {
            return false; // a service of another contract type
        }
        Classpath.Resolved target = declarations.resolveMethod(mi.owner, mi.name, mi.desc);
        if (target == null || !target.owner().equals(mi.owner)) {
            return false;
        }
        if ((target.access() & Opcodes.ACC_ABSTRACT) == 0) {
            needs.add(new Key(mi.owner, mi.name, mi.desc));
        }
        return true;
    }

    private boolean field(ClassNode c, FieldInsnNode fi, Set<Key> needs, Set<Key> fields) {
        if (!knownDescriptor(fi.desc)) {
            return false;
        }
        if (model.isPlatform(fi.owner) || model.isData(fi.owner)) {
            return true;
        }
        if (!fi.owner.equals(c.name)) {
            // Another contract type's static state is real only when its initializer is copied.
            if (fi.getOpcode() != Opcodes.GETSTATIC || !model.contracts.containsKey(fi.owner)
                    || withCode(fi.owner).fields.stream().noneMatch(f -> f.name.equals(fi.name) && f.desc.equals(fi.desc))) {
                return false;
            }
            fields.add(new Key(fi.owner, fi.name, fi.desc));
            needs.add(new Key(fi.owner, "<clinit>", "()V"));
            return true;
        }
        if (c.fields.stream().noneMatch(f -> f.name.equals(fi.name) && f.desc.equals(fi.desc))) {
            return false;
        }
        fields.add(new Key(c.name, fi.name, fi.desc));
        boolean readsInstanceState = fi.getOpcode() == Opcodes.GETFIELD;
        if (readsInstanceState) {
            for (MethodNode ctor : c.methods) {
                if (ctor.name.equals("<init>")) {
                    needs.add(new Key(c.name, ctor.name, ctor.desc));
                }
            }
        }
        return true;
    }

    private boolean known(String internalNameOrArray) {
        if (internalNameOrArray.startsWith("[")) {
            return knownDescriptor(internalNameOrArray);
        }
        return model.isPlatform(internalNameOrArray) || model.inSurface(internalNameOrArray);
    }

    private boolean knownDescriptor(String desc) {
        Type t = Type.getType(desc);
        return switch (t.getSort()) {
            case Type.METHOD -> {
                boolean ok = knownDescriptor(t.getReturnType().getDescriptor());
                for (Type a : t.getArgumentTypes()) {
                    ok &= knownDescriptor(a.getDescriptor());
                }
                yield ok;
            }
            case Type.ARRAY -> knownDescriptor(t.getElementType().getDescriptor());
            case Type.OBJECT -> known(t.getInternalName());
            default -> true;
        };
    }

    private boolean knownFrame(List<Object> entries) {
        if (entries == null) {
            return true;
        }
        for (Object o : entries) {
            if (o instanceof String s && !known(s)) {
                return false;
            }
        }
        return true;
    }

    private static MethodNode method(ClassNode c, String name, String desc) {
        for (MethodNode m : c.methods) {
            if (m.name.equals(name) && m.desc.equals(desc)) {
                return m;
            }
        }
        return null;
    }
}
