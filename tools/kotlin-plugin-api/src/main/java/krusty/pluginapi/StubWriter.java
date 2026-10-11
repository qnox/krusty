package krusty.pluginapi;

import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.signature.SignatureReader;
import org.objectweb.asm.signature.SignatureVisitor;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldNode;
import org.objectweb.asm.tree.InnerClassNode;
import org.objectweb.asm.tree.MethodNode;

/**
 * Writes one contract type of the surface: the compiler type's exact binary shape with only the
 * members {@link SurfaceModel} kept, and no compiler code.
 *
 * <ul>
 *   <li>Abstract members stay abstract, so an implementation of the surface supplies them by
 *       subclassing exactly as the compiler does.
 *   <li>Concrete members throw {@link Surface#unimplemented} naming themselves until an
 *       implementation replaces the type.
 *   <li>Constructors of abstract types and of types plugins extend succeed and keep no state, so a
 *       plugin can instantiate its own extensions. Every class also gets a public synthetic
 *       constructor taking {@link Surface}, which is how a subclass in the surface reaches its
 *       superclass without running compiler code.
 *   <li>Enum constants keep their declared order, and {@code Companion}/{@code INSTANCE} singletons
 *       exist, because plugin code reads them in static initializers.
 * </ul>
 */
final class StubWriter {
    static final String SURFACE = Type.getInternalName(Surface.class);
    static final String MARKER_DESC = "(L" + SURFACE + ";)V";
    private static final String ENUM_VALUES = "$surfaceValues";

    private final SurfaceModel model;

    StubWriter(SurfaceModel model) {
        this.model = model;
    }

    byte[] write(String name) {
        ClassNode c = model.compiler.get(name);
        Set<SurfaceModel.Member> kept = model.contracts.get(name);
        boolean isInterface = (c.access & Opcodes.ACC_INTERFACE) != 0;
        boolean isEnum = (c.access & Opcodes.ACC_ENUM) != 0;
        // An enum whose constants have bodies is abstract; its stub creates plain constants instead.
        int access = isEnum ? c.access & ~Opcodes.ACC_ABSTRACT : c.access;
        ClassWriter w = new ClassWriter(ClassWriter.COMPUTE_MAXS);
        w.visit(Math.max(c.version, Opcodes.V1_8), access, c.name, signature(c.signature), c.superName,
                c.interfaces.toArray(String[]::new));
        for (InnerClassNode inner : c.innerClasses) {
            boolean known = model.inSurface(inner.name) && (inner.outerName == null || model.inSurface(inner.outerName));
            if (known) {
                w.visitInnerClass(inner.name, inner.outerName, inner.innerName, inner.access);
            }
        }
        List<FieldNode> singletons = new ArrayList<>();
        List<FieldNode> constants = new ArrayList<>();
        for (FieldNode f : c.fields) {
            if (!kept.contains(new SurfaceModel.Member(f.name, f.desc))) {
                continue;
            }
            w.visitField(f.access, f.name, f.desc, signature(f.signature), f.value).visitEnd();
            if ((f.access & Opcodes.ACC_ENUM) != 0) {
                constants.add(f);
            } else if (isSingleton(c, f)) {
                singletons.add(f);
            }
        }
        boolean realInitializer = model.isCopied(name, "<clinit>", "()V");
        ClassNode code = model.withCode(name);
        boolean constructible = !isInterface && ((c.access & Opcodes.ACC_ABSTRACT) != 0 || model.extensible.contains(name));
        for (MethodNode m : c.methods) {
            if (!kept.contains(new SurfaceModel.Member(m.name, m.desc)) || m.name.equals("<clinit>")) {
                continue;
            }
            if (model.isCopied(name, m.name, m.desc)) {
                copy(w, code, m);
            } else if (isEnum && !realInitializer && m.name.equals("values")) {
                enumValues(w, c);
            } else if (isEnum && !realInitializer && m.name.equals("valueOf")) {
                enumValueOf(w, c);
            } else {
                method(w, c, m, constructible && m.name.equals("<init>"), isEnum);
            }
        }
        if (!isInterface) {
            markerConstructor(w, c);
        }
        if (realInitializer) {
            copy(w, code, c.methods.stream().filter(m -> m.name.equals("<clinit>")).findFirst().orElseThrow());
        } else if (isEnum) {
            w.visitField(Opcodes.ACC_PRIVATE | Opcodes.ACC_STATIC | Opcodes.ACC_FINAL | Opcodes.ACC_SYNTHETIC,
                    ENUM_VALUES, "[L" + c.name + ";", null, null).visitEnd();
            enumConstructor(w);
        }
        if (!realInitializer && (!constants.isEmpty() || !singletons.isEmpty() || isEnum)) {
            staticInitializer(w, c, constants, singletons, isEnum);
        }
        w.visitEnd();
        return w.toByteArray();
    }

    /** Writes the compiler's own code for a member {@link CodeCopies} found self-contained. */
    private static void copy(ClassWriter w, ClassNode code, MethodNode declaration) {
        for (MethodNode m : code.methods) {
            if (m.name.equals(declaration.name) && m.desc.equals(declaration.desc)) {
                m.accept(w);
                return;
            }
        }
        throw new IllegalStateException("no code for " + code.name + "." + declaration.name + declaration.desc);
    }

    /** A {@code Companion} or {@code INSTANCE} field holding an instance the surface can create. */
    private boolean isSingleton(ClassNode owner, FieldNode f) {
        if ((f.access & Opcodes.ACC_STATIC) == 0 || f.desc.charAt(0) != 'L') {
            return false;
        }
        String type = Type.getType(f.desc).getInternalName();
        boolean named = f.name.equals("INSTANCE") && type.equals(owner.name)
                || f.name.equals("Companion") && type.equals(owner.name + "$Companion");
        if (!named || !model.contracts.containsKey(type)) {
            return false;
        }
        ClassNode t = model.compiler.get(type);
        return (t.access & (Opcodes.ACC_ABSTRACT | Opcodes.ACC_INTERFACE)) == 0;
    }

    private void method(ClassWriter w, ClassNode c, MethodNode m, boolean succeeds, boolean concreteOnly) {
        int access = m.access & ~Opcodes.ACC_NATIVE;
        if (concreteOnly) {
            access &= ~Opcodes.ACC_ABSTRACT;
        }
        MethodVisitor mv = w.visitMethod(access, m.name, m.desc, signature(m.signature),
                m.exceptions.toArray(String[]::new));
        if ((access & Opcodes.ACC_ABSTRACT) != 0) {
            mv.visitEnd();
            return;
        }
        mv.visitCode();
        if (succeeds) {
            superFromSurface(mv, c);
            mv.visitInsn(Opcodes.RETURN);
        } else {
            mv.visitLdcInsn(c.name + "." + m.name + m.desc);
            mv.visitMethodInsn(Opcodes.INVOKESTATIC, SURFACE, "unimplemented",
                    "(Ljava/lang/String;)Ljava/lang/RuntimeException;", false);
            mv.visitInsn(Opcodes.ATHROW);
        }
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    private void markerConstructor(ClassWriter w, ClassNode c) {
        MethodVisitor mv = w.visitMethod(Opcodes.ACC_PUBLIC | Opcodes.ACC_SYNTHETIC, "<init>", MARKER_DESC, null, null);
        mv.visitCode();
        superFromSurface(mv, c);
        mv.visitInsn(Opcodes.RETURN);
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    /** Runs the superclass constructor without compiler code: {@code this} is initialized after it. */
    private void superFromSurface(MethodVisitor mv, ClassNode c) {
        mv.visitVarInsn(Opcodes.ALOAD, 0);
        String sup = c.superName;
        if (model.contracts.containsKey(sup)) {
            mv.visitInsn(Opcodes.ACONST_NULL);
            mv.visitMethodInsn(Opcodes.INVOKESPECIAL, sup, "<init>", MARKER_DESC, false);
        } else if (sup.equals("java/lang/Enum")) {
            mv.visitInsn(Opcodes.ACONST_NULL);
            mv.visitInsn(Opcodes.ICONST_0);
            mv.visitMethodInsn(Opcodes.INVOKESPECIAL, sup, "<init>", "(Ljava/lang/String;I)V", false);
        } else {
            String desc = cheapestConstructor(sup);
            for (Type a : Type.getArgumentTypes(desc)) {
                pushDefault(mv, a);
            }
            mv.visitMethodInsn(Opcodes.INVOKESPECIAL, sup, "<init>", desc, false);
        }
    }

    /** The visible constructor of a platform or data superclass with the fewest parameters. */
    private String cheapestConstructor(String type) {
        ClassNode node = model.compiler.contains(type) ? model.compiler.get(type) : model.platform.find(type);
        if (node == null) {
            throw new IllegalStateException("superclass " + type + " is neither in the surface nor on the platform");
        }
        String best = null;
        for (MethodNode m : node.methods) {
            if (m.name.equals("<init>") && (m.access & Opcodes.ACC_PRIVATE) == 0
                    && (best == null || Type.getArgumentTypes(m.desc).length < Type.getArgumentTypes(best).length)) {
                best = m.desc;
            }
        }
        if (best == null) {
            throw new IllegalStateException("no visible constructor on superclass " + type);
        }
        return best;
    }

    private static void pushDefault(MethodVisitor mv, Type t) {
        switch (t.getSort()) {
            case Type.LONG -> mv.visitInsn(Opcodes.LCONST_0);
            case Type.FLOAT -> mv.visitInsn(Opcodes.FCONST_0);
            case Type.DOUBLE -> mv.visitInsn(Opcodes.DCONST_0);
            case Type.OBJECT, Type.ARRAY -> mv.visitInsn(Opcodes.ACONST_NULL);
            default -> mv.visitInsn(Opcodes.ICONST_0);
        }
    }

    private static void enumConstructor(ClassWriter w) {
        MethodVisitor mv = w.visitMethod(Opcodes.ACC_PRIVATE | Opcodes.ACC_SYNTHETIC, "<init>",
                "(Ljava/lang/String;ILjava/lang/Void;)V", null, null);
        mv.visitCode();
        mv.visitVarInsn(Opcodes.ALOAD, 0);
        mv.visitVarInsn(Opcodes.ALOAD, 1);
        mv.visitVarInsn(Opcodes.ILOAD, 2);
        mv.visitMethodInsn(Opcodes.INVOKESPECIAL, "java/lang/Enum", "<init>", "(Ljava/lang/String;I)V", false);
        mv.visitInsn(Opcodes.RETURN);
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    private static void enumValues(ClassWriter w, ClassNode c) {
        String array = "[L" + c.name + ";";
        MethodVisitor mv = w.visitMethod(Opcodes.ACC_PUBLIC | Opcodes.ACC_STATIC, "values", "()" + array, null, null);
        mv.visitCode();
        mv.visitFieldInsn(Opcodes.GETSTATIC, c.name, ENUM_VALUES, array);
        mv.visitMethodInsn(Opcodes.INVOKEVIRTUAL, array, "clone", "()Ljava/lang/Object;", false);
        mv.visitTypeInsn(Opcodes.CHECKCAST, array);
        mv.visitInsn(Opcodes.ARETURN);
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    private static void enumValueOf(ClassWriter w, ClassNode c) {
        MethodVisitor mv = w.visitMethod(Opcodes.ACC_PUBLIC | Opcodes.ACC_STATIC, "valueOf",
                "(Ljava/lang/String;)L" + c.name + ";", null, null);
        mv.visitCode();
        mv.visitLdcInsn(Type.getObjectType(c.name));
        mv.visitVarInsn(Opcodes.ALOAD, 0);
        mv.visitMethodInsn(Opcodes.INVOKESTATIC, "java/lang/Enum", "valueOf",
                "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;", false);
        mv.visitTypeInsn(Opcodes.CHECKCAST, c.name);
        mv.visitInsn(Opcodes.ARETURN);
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    private static void staticInitializer(ClassWriter w, ClassNode c, List<FieldNode> constants,
                                          List<FieldNode> singletons, boolean isEnum) {
        MethodVisitor mv = w.visitMethod(Opcodes.ACC_STATIC, "<clinit>", "()V", null, null);
        mv.visitCode();
        if (isEnum) {
            String array = "[L" + c.name + ";";
            pushInt(mv, constants.size());
            mv.visitTypeInsn(Opcodes.ANEWARRAY, c.name);
            for (int i = 0; i < constants.size(); i++) {
                FieldNode f = constants.get(i);
                mv.visitInsn(Opcodes.DUP);
                pushInt(mv, i);
                mv.visitTypeInsn(Opcodes.NEW, c.name);
                mv.visitInsn(Opcodes.DUP);
                mv.visitLdcInsn(f.name);
                pushInt(mv, i);
                mv.visitInsn(Opcodes.ACONST_NULL);
                mv.visitMethodInsn(Opcodes.INVOKESPECIAL, c.name, "<init>", "(Ljava/lang/String;ILjava/lang/Void;)V", false);
                mv.visitInsn(Opcodes.DUP);
                mv.visitFieldInsn(Opcodes.PUTSTATIC, c.name, f.name, f.desc);
                mv.visitInsn(Opcodes.AASTORE);
            }
            mv.visitFieldInsn(Opcodes.PUTSTATIC, c.name, ENUM_VALUES, array);
        }
        for (FieldNode f : singletons) {
            String type = Type.getType(f.desc).getInternalName();
            mv.visitTypeInsn(Opcodes.NEW, type);
            mv.visitInsn(Opcodes.DUP);
            mv.visitInsn(Opcodes.ACONST_NULL);
            mv.visitMethodInsn(Opcodes.INVOKESPECIAL, type, "<init>", MARKER_DESC, false);
            mv.visitFieldInsn(Opcodes.PUTSTATIC, c.name, f.name, f.desc);
        }
        mv.visitInsn(Opcodes.RETURN);
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    private static void pushInt(MethodVisitor mv, int value) {
        if (value <= 5) {
            mv.visitInsn(Opcodes.ICONST_0 + value);
        } else if (value <= Byte.MAX_VALUE) {
            mv.visitIntInsn(Opcodes.BIPUSH, value);
        } else {
            mv.visitIntInsn(Opcodes.SIPUSH, value);
        }
    }

    /** Keeps a generic signature only when every class it names exists next to the plugin. */
    private String signature(String sig) {
        if (sig == null) {
            return null;
        }
        boolean[] resolvable = {true};
        new SignatureReader(sig).accept(new SignatureVisitor(Opcodes.ASM9) {
            @Override
            public void visitClassType(String name) {
                if (!model.isPlatform(name) && !model.inSurface(name)) {
                    resolvable[0] = false;
                }
            }
        });
        return resolvable[0] ? sig : null;
    }
}
