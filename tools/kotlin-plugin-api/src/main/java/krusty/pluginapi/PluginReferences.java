package krusty.pluginapi;

import java.util.Set;
import java.util.TreeSet;
import org.objectweb.asm.Handle;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.tree.AbstractInsnNode;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldInsnNode;
import org.objectweb.asm.tree.FieldNode;
import org.objectweb.asm.tree.InvokeDynamicInsnNode;
import org.objectweb.asm.tree.LdcInsnNode;
import org.objectweb.asm.tree.MethodInsnNode;
import org.objectweb.asm.tree.MethodNode;
import org.objectweb.asm.tree.MultiANewArrayInsnNode;
import org.objectweb.asm.tree.TryCatchBlockNode;
import org.objectweb.asm.tree.TypeInsnNode;

/**
 * Everything one plugin jar links against outside itself: the types it names, the members its code
 * accesses (with the access instruction, so a check can apply the JVM's resolution rules), and the
 * types its classes extend or implement.
 */
final class PluginReferences {
    /** How an instruction reaches a member; decides the JVM resolution and kind checks. */
    enum Access {
        STATIC_FIELD, INSTANCE_FIELD, STATIC_METHOD, VIRTUAL_METHOD, SPECIAL_METHOD, INTERFACE_METHOD;

        boolean isField() {
            return this == STATIC_FIELD || this == INSTANCE_FIELD;
        }
    }

    /** One member access in plugin code. {@code itf} is the instruction's interface-owner flag. */
    record Ref(String user, String owner, String name, String desc, Access access, boolean itf)
            implements Comparable<Ref> {
        @Override
        public int compareTo(Ref o) {
            return toString().compareTo(o.toString());
        }

        String member() {
            return owner + "." + name + desc;
        }
    }

    final Set<String> own = new TreeSet<>();
    final Set<String> types = new TreeSet<>();
    final Set<Ref> refs = new TreeSet<>();
    /** Supertypes of plugin classes that the plugin does not declare itself. */
    final Set<String> extended = new TreeSet<>();

    static PluginReferences scan(ClassIndex jar) {
        PluginReferences r = new PluginReferences();
        r.own.addAll(jar.names());
        for (String name : jar.names()) {
            ClassNode c = jar.get(name);
            r.visit(c);
            jar.forget(name);
        }
        r.types.removeAll(r.own);
        r.extended.removeAll(r.own);
        r.refs.removeIf(ref -> r.own.contains(ref.owner()));
        return r;
    }

    private void visit(ClassNode c) {
        if (c.superName != null) {
            extended.add(c.superName);
            types.add(c.superName);
        }
        extended.addAll(c.interfaces);
        types.addAll(c.interfaces);
        for (FieldNode f : c.fields) {
            descriptorTypes(f.desc);
        }
        for (MethodNode m : c.methods) {
            descriptorTypes(m.desc);
            for (TryCatchBlockNode t : m.tryCatchBlocks) {
                if (t.type != null) {
                    types.add(t.type);
                }
            }
            for (AbstractInsnNode insn : m.instructions) {
                instruction(c.name, insn);
            }
        }
    }

    private void instruction(String user, AbstractInsnNode insn) {
        switch (insn) {
            case MethodInsnNode mi -> {
                Access access = switch (mi.getOpcode()) {
                    case Opcodes.INVOKESTATIC -> Access.STATIC_METHOD;
                    case Opcodes.INVOKESPECIAL -> Access.SPECIAL_METHOD;
                    case Opcodes.INVOKEINTERFACE -> Access.INTERFACE_METHOD;
                    default -> Access.VIRTUAL_METHOD;
                };
                member(user, mi.owner, mi.name, mi.desc, access, mi.itf);
            }
            case FieldInsnNode fi -> {
                boolean isStatic = fi.getOpcode() == Opcodes.GETSTATIC || fi.getOpcode() == Opcodes.PUTSTATIC;
                member(user, fi.owner, fi.name, fi.desc, isStatic ? Access.STATIC_FIELD : Access.INSTANCE_FIELD, false);
            }
            case TypeInsnNode ti -> typeName(ti.desc);
            case MultiANewArrayInsnNode ma -> descriptorTypes(ma.desc);
            case LdcInsnNode ldc -> constant(user, ldc.cst);
            case InvokeDynamicInsnNode indy -> {
                descriptorTypes(indy.desc);
                handle(user, indy.bsm);
                for (Object arg : indy.bsmArgs) {
                    constant(user, arg);
                }
            }
            default -> { }
        }
    }

    private void constant(String user, Object cst) {
        if (cst instanceof Type t) {
            descriptorTypes(t.getDescriptor());
        } else if (cst instanceof Handle h) {
            handle(user, h);
        }
    }

    private void handle(String user, Handle h) {
        Access access = switch (h.getTag()) {
            case Opcodes.H_GETSTATIC, Opcodes.H_PUTSTATIC -> Access.STATIC_FIELD;
            case Opcodes.H_GETFIELD, Opcodes.H_PUTFIELD -> Access.INSTANCE_FIELD;
            case Opcodes.H_INVOKESTATIC -> Access.STATIC_METHOD;
            case Opcodes.H_INVOKESPECIAL, Opcodes.H_NEWINVOKESPECIAL -> Access.SPECIAL_METHOD;
            case Opcodes.H_INVOKEINTERFACE -> Access.INTERFACE_METHOD;
            default -> Access.VIRTUAL_METHOD;
        };
        member(user, h.getOwner(), h.getName(), h.getDesc(), access, h.isInterface());
    }

    private void member(String user, String owner, String name, String desc, Access access, boolean itf) {
        if (owner.startsWith("[")) {
            descriptorTypes(owner);
            return;
        }
        types.add(owner);
        descriptorTypes(desc);
        refs.add(new Ref(user, owner, name, desc, access, itf));
    }

    private void typeName(String internalNameOrArray) {
        if (internalNameOrArray.startsWith("[")) {
            descriptorTypes(internalNameOrArray);
        } else {
            types.add(internalNameOrArray);
        }
    }

    private void descriptorTypes(String desc) {
        Type t = Type.getType(desc);
        switch (t.getSort()) {
            case Type.METHOD -> {
                descriptorTypes(t.getReturnType().getDescriptor());
                for (Type a : t.getArgumentTypes()) {
                    descriptorTypes(a.getDescriptor());
                }
            }
            case Type.ARRAY -> descriptorTypes(t.getElementType().getDescriptor());
            case Type.OBJECT -> types.add(t.getInternalName());
            default -> { }
        }
    }
}
