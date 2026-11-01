import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Consumer;

import org.jetbrains.org.objectweb.asm.AnnotationVisitor;
import org.jetbrains.org.objectweb.asm.ClassVisitor;
import org.jetbrains.org.objectweb.asm.FieldVisitor;
import org.jetbrains.org.objectweb.asm.MethodVisitor;
import org.jetbrains.org.objectweb.asm.Opcodes;
import org.jetbrains.org.objectweb.asm.Type;

/**
 * A Java rendering of JetBrains' {@code BytecodeListingTextCollectingVisitor}
 * (compiler/tests-compiler-utils, Apache 2.0), the visitor behind the {@code codegen/bytecodeListing}
 * dumps, with the {@code ForCodegenTests} filter. It must print exactly what the Kotlin original
 * prints: the dumps are compared as text.
 */
final class BytecodeListing extends ClassVisitor {
    private static final int CLASS = 1, FIELD = 2, METHOD = 4;

    private static final class Modifier {
        final String text;
        final int mask;
        final int targets;
        final int excludedMask;

        Modifier(String text, int mask, int targets, int excludedMask) {
            this.text = text;
            this.mask = mask;
            this.targets = targets;
            this.excludedMask = excludedMask;
        }

        boolean has(int access, int target) {
            return (access & mask) != 0 && (access & excludedMask) == 0 && (targets & target) != 0;
        }
    }

    private static final Modifier[] MODIFIERS = {
        new Modifier("public", Opcodes.ACC_PUBLIC, CLASS | FIELD | METHOD, 0),
        new Modifier("protected", Opcodes.ACC_PROTECTED, CLASS | FIELD | METHOD, 0),
        new Modifier("private", Opcodes.ACC_PRIVATE, CLASS | FIELD | METHOD, 0),
        new Modifier("synthetic", Opcodes.ACC_SYNTHETIC, CLASS | FIELD | METHOD, 0),
        new Modifier("bridge", Opcodes.ACC_BRIDGE, METHOD, 0),
        new Modifier("volatile", Opcodes.ACC_VOLATILE, FIELD, 0),
        new Modifier("synchronized", Opcodes.ACC_SYNCHRONIZED, METHOD, 0),
        new Modifier("varargs", Opcodes.ACC_VARARGS, METHOD, 0),
        new Modifier("transient", Opcodes.ACC_TRANSIENT, FIELD, 0),
        new Modifier("native", Opcodes.ACC_NATIVE, METHOD, 0),
        new Modifier("deprecated", Opcodes.ACC_DEPRECATED, CLASS | FIELD | METHOD, 0),
        new Modifier("final", Opcodes.ACC_FINAL, CLASS | FIELD | METHOD, 0),
        new Modifier("strict", Opcodes.ACC_STRICT, METHOD, 0),
        new Modifier("enum", Opcodes.ACC_ENUM, FIELD, 0),
        new Modifier("abstract", Opcodes.ACC_ABSTRACT, CLASS | METHOD, Opcodes.ACC_INTERFACE),
        new Modifier("static", Opcodes.ACC_STATIC, FIELD | METHOD, 0),
    };

    /** {@code Filter.ForCodegenTests}: the generated coroutine helpers are not listed. */
    static boolean shouldWriteClass(String name) {
        return !name.startsWith("helpers/");
    }

    private static final class Declaration {
        final String text;
        final List<String> annotations = new ArrayList<>();

        Declaration(String text) {
            this.text = text;
        }
    }

    private final boolean withSignatures;
    private final boolean withAnnotations;
    private final boolean sortDeclarations;
    private final List<Declaration> declarations = new ArrayList<>();
    private final List<String> classAnnotations = new ArrayList<>();
    private String className = "";
    private int classAccess = 0;
    private String classSignature = "";

    BytecodeListing(boolean withSignatures, boolean withAnnotations, boolean sortDeclarations) {
        super(Opcodes.API_VERSION);
        this.withSignatures = withSignatures;
        this.withAnnotations = withAnnotations;
        this.sortDeclarations = sortDeclarations;
    }

    private Declaration last() {
        return declarations.get(declarations.size() - 1);
    }

    private static void handleModifiers(int target, int access, List<String> list) {
        for (Modifier modifier : MODIFIERS) {
            if (modifier.has(access, target)) list.add(modifier.text + " ");
        }
    }

    private static String getModifiers(int target, int access) {
        StringBuilder text = new StringBuilder();
        for (Modifier modifier : MODIFIERS) {
            if (!modifier.has(access, target)) continue;
            if (text.length() > 0) text.append(' ');
            text.append(modifier.text);
        }
        return text.toString();
    }

    private static String classOrInterface(int access) {
        if ((access & Opcodes.ACC_ANNOTATION) != 0) return "annotation class";
        if ((access & Opcodes.ACC_ENUM) != 0) return "enum class";
        if ((access & Opcodes.ACC_INTERFACE) != 0) return "interface";
        return "class";
    }

    String text() {
        StringBuilder text = new StringBuilder();
        if (!classAnnotations.isEmpty()) {
            text.append(String.join("\n", classAnnotations)).append("\n");
        }
        List<String> modifiers = new ArrayList<>();
        handleModifiers(CLASS, classAccess, modifiers);
        for (String modifier : modifiers) text.append(modifier);
        text.append(classOrInterface(classAccess));
        if (withSignatures) text.append("<").append(classSignature).append("> ");
        text.append(" ");
        text.append(className);
        if (!declarations.isEmpty()) {
            text.append(" {\n");
            List<Declaration> ordered = new ArrayList<>(declarations);
            if (sortDeclarations) ordered.sort(Comparator.comparing(declaration -> declaration.text));
            for (Declaration declaration : ordered) {
                text.append("    ")
                    .append(String.join("", declaration.annotations))
                    .append(declaration.text)
                    .append("\n");
            }
            text.append("}");
        }
        return text.toString();
    }

    @Override
    public void visitSource(String source, String debug) {
        declarations.add(new Declaration(source != null ? "// source: '" + source + "'" : "// source: null"));
    }

    @Override
    public void visitPermittedSubclass(String permittedSubclass) {
        if (permittedSubclass != null) {
            declarations.add(new Declaration("permittedSubclass: " + permittedSubclass));
        }
    }

    @Override
    public MethodVisitor visitMethod(
        int access, String name, String desc, String signature, String[] exceptions
    ) {
        String returnType = Type.getReturnType(desc).getClassName();
        Type[] arguments = Type.getArgumentTypes(desc);
        List<String> methodAnnotations = new ArrayList<>();
        Map<Integer, List<String>> parameterAnnotations = new HashMap<>();
        handleModifiers(METHOD, access, methodAnnotations);
        int methodParamCount = arguments.length;

        return new MethodVisitor(Opcodes.API_VERSION) {
            private int visibleAnnotableParameterCount = methodParamCount;
            private int invisibleAnnotableParameterCount = methodParamCount;

            @Override
            public AnnotationVisitor visitAnnotation(String desc, boolean visible) {
                return visitAnnotationImpl(args -> methodAnnotations.add("@" + renderAnnotation(desc, args) + " "));
            }

            @Override
            public AnnotationVisitor visitParameterAnnotation(int parameter, String desc, boolean visible) {
                return visitAnnotationImpl(args -> {
                    int index = parameter + methodParamCount
                        - (visible ? visibleAnnotableParameterCount : invisibleAnnotableParameterCount);
                    parameterAnnotations.computeIfAbsent(index, key -> new ArrayList<>())
                        .add("@" + renderAnnotation(desc, args) + " ");
                });
            }

            @Override
            public void visitAnnotableParameterCount(int parameterCount, boolean visible) {
                if (visible) visibleAnnotableParameterCount = parameterCount;
                else invisibleAnnotableParameterCount = parameterCount;
            }

            @Override
            public void visitEnd() {
                StringBuilder parameters = new StringBuilder();
                for (int index = 0; index < arguments.length; index++) {
                    if (index > 0) parameters.append(", ");
                    parameters.append(String.join("", parameterAnnotations.getOrDefault(index, List.of())))
                        .append("p").append(index).append(": ").append(arguments[index].getClassName());
                }
                String signatureIfRequired = withSignatures ? "<" + signature + "> " : "";
                Declaration declaration = new Declaration(
                    signatureIfRequired + "method " + name + "(" + parameters + "): " + returnType);
                declaration.annotations.addAll(methodAnnotations);
                declarations.add(declaration);
                super.visitEnd();
            }
        };
    }

    @Override
    public FieldVisitor visitField(int access, String name, String desc, String signature, Object value) {
        String type = Type.getType(desc).getClassName();
        String fieldSignature = withSignatures ? "<" + signature + "> " : "";
        declarations.add(new Declaration("field " + fieldSignature + name + ": " + type));
        handleModifiers(FIELD, access, last().annotations);
        return new FieldVisitor(Opcodes.API_VERSION) {
            @Override
            public AnnotationVisitor visitAnnotation(String desc, boolean visible) {
                // The original appends to the declaration listed last, which is this field's.
                return visitAnnotationImpl(args -> last().annotations.add("@" + renderAnnotation(desc, args) + " "));
            }
        };
    }

    @Override
    public AnnotationVisitor visitAnnotation(String desc, boolean visible) {
        return visitAnnotationImpl(args -> {
            String rendered = renderClassAnnotation(desc, args);
            if (rendered != null) classAnnotations.add("@" + rendered);
        });
    }

    private AnnotationVisitor visitAnnotationImpl(Consumer<List<String>> end) {
        if (!withAnnotations) return null;
        return new AnnotationVisitor(Opcodes.API_VERSION) {
            private final List<String> arguments = new ArrayList<>();

            @Override
            public void visit(String name, Object value) {
                String rendered;
                if (value instanceof String) rendered = "\"" + value + "\"";
                else if (value instanceof Type) rendered = ((Type) value).getClassName() + "::class";
                else if (value instanceof boolean[]) rendered = Arrays.toString((boolean[]) value);
                else if (value instanceof char[]) rendered = Arrays.toString((char[]) value);
                else if (value instanceof byte[]) rendered = Arrays.toString((byte[]) value);
                else if (value instanceof short[]) rendered = Arrays.toString((short[]) value);
                else if (value instanceof int[]) rendered = Arrays.toString((int[]) value);
                else if (value instanceof float[]) rendered = Arrays.toString((float[]) value);
                else if (value instanceof long[]) rendered = Arrays.toString((long[]) value);
                else if (value instanceof double[]) rendered = Arrays.toString((double[]) value);
                else rendered = value.toString();
                arguments.add(name != null ? name + "=" + rendered : rendered);
            }

            @Override
            public void visitEnum(String name, String descriptor, String value) {
                arguments.add(name != null ? name + "=" + value : value);
            }

            @Override
            public AnnotationVisitor visitAnnotation(String name, String descriptor) {
                return visitAnnotationImpl(args -> {
                    String rendered = renderAnnotation(descriptor, args);
                    arguments.add(name != null ? name + "=" + rendered : rendered);
                });
            }

            @Override
            public AnnotationVisitor visitArray(String name) {
                return visitAnnotationImpl(args -> arguments.add(name + "=[" + String.join(", ", args) + "]"));
            }

            @Override
            public void visitEnd() {
                end.accept(arguments);
            }
        };
    }

    private static String renderAnnotation(String desc, List<String> args) {
        return renderAnnotationArguments(args, Type.getType(desc).getClassName());
    }

    private static String renderClassAnnotation(String desc, List<String> args) {
        if (desc.equals("Lkotlin/jvm/internal/SourceDebugExtension;")) return null;
        String name = Type.getType(desc).getClassName();
        if (desc.equals("Lkotlin/Metadata;") || desc.equals("Lkotlin/coroutines/jvm/internal/DebugMetadata;")) {
            return name;
        }
        return renderAnnotationArguments(args, name);
    }

    private static String renderAnnotationArguments(List<String> args, String name) {
        return args.isEmpty() ? name : name + "(" + String.join(", ", args) + ")";
    }

    @Override
    public void visit(int version, int access, String name, String signature, String superName, String[] interfaces) {
        className = name;
        classAccess = access;
        classSignature = signature;
    }

    @Override
    public void visitOuterClass(String owner, String name, String descriptor) {
        if (name == null) {
            if (descriptor != null) throw new AssertionError("enclosing class with a descriptor");
            declarations.add(new Declaration("enclosing class " + owner));
        } else {
            if (descriptor == null) throw new AssertionError("enclosing method without a descriptor");
            declarations.add(new Declaration("enclosing method " + owner + "." + name + descriptor));
        }
    }

    @Override
    public void visitInnerClass(String name, String outerName, String innerName, int access) {
        if (!shouldWriteClass(name)) return;
        if (innerName == null) {
            if (outerName != null) {
                throw new AssertionError("Anonymous classes should have neither innerName nor outerName. Name="
                    + name + ", outerName=" + outerName);
            }
            declarations.add(new Declaration("inner (anonymous) class " + name));
        } else if (outerName == null) {
            declarations.add(new Declaration("inner (local) class " + name + " " + innerName));
        } else if (name.equals(outerName + "$" + innerName)) {
            declarations.add(new Declaration(getModifiers(CLASS, access) + " inner class " + name));
        } else {
            declarations.add(new Declaration("inner (unrecognized) class " + name + " " + outerName + " " + innerName));
        }
    }
}
