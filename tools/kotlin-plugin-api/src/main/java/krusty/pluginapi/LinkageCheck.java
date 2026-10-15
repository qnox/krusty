package krusty.pluginapi;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.tree.ClassNode;

/**
 * Applies the JVM's link-time rules to every reference a plugin makes into the compiler, against a
 * surface jar instead of the compiler: the class exists, its kind matches the instruction
 * ({@code invokeinterface} needs an interface, {@code extends} a non-final class), and the member
 * resolves with the expected static-ness. A reference to a class the reference compiler does not
 * have either is outside the surface's job and is counted, not checked.
 */
final class LinkageCheck {
    /**
     * {@code problems} are links the reference compiler satisfies and the surface does not.
     * {@code compilerLacks} are links the reference compiler does not satisfy either: code a plugin
     * keeps for other compiler versions, which never runs against this one.
     */
    record Result(List<String> problems, int checked, int compilerLacks, int outsideCompiler) {
    }

    private final ClassIndex surface;
    private final ClassIndex compiler;
    private final Classpath platform;
    /** The surface on top of the platform, as a plugin sees it at run time. */
    private final Classpath linked;
    /** The reference compiler on top of the platform. */
    private final Classpath reference;

    LinkageCheck(ClassIndex surface, ClassIndex compiler, ClassIndex stdlib) {
        this.surface = surface;
        this.compiler = compiler;
        this.platform = new Classpath(List.of(stdlib));
        this.linked = new Classpath(List.of(surface, stdlib));
        this.reference = new Classpath(List.of(compiler, stdlib));
    }

    Result check(PluginReferences plugin, ClassIndex pluginJar) {
        List<String> problems = new ArrayList<>();
        int checked = 0;
        int compilerLacks = 0;
        Set<String> outside = new HashSet<>();
        for (String name : plugin.own) {
            ClassNode c = pluginJar.get(name);
            if (c.superName != null && relevant(c.superName, plugin, outside)) {
                checked++;
                ClassNode sup = surface.get(c.superName);
                if (sup == null) {
                    problems.add(name + " extends missing " + c.superName);
                } else if ((sup.access & (Opcodes.ACC_INTERFACE | Opcodes.ACC_FINAL)) != 0) {
                    problems.add(name + " extends " + c.superName + ", which is an interface or final");
                }
            }
            for (String i : c.interfaces) {
                if (relevant(i, plugin, outside)) {
                    checked++;
                    ClassNode in = surface.get(i);
                    if (in == null || (in.access & Opcodes.ACC_INTERFACE) == 0) {
                        problems.add(name + " implements " + i + (in == null ? ", which is missing" : ", which is not an interface"));
                    }
                }
            }
            pluginJar.forget(name);
        }
        for (PluginReferences.Ref ref : plugin.refs) {
            if (!relevant(ref.owner(), plugin, outside)) {
                continue;
            }
            checked++;
            String problem = member(ref, linked, surface);
            if (problem != null && member(ref, reference, compiler) != null) {
                compilerLacks++;
            } else if (problem != null) {
                problems.add(ref.user() + " -> " + ref.member() + ": " + problem);
            }
        }
        for (String type : plugin.types) {
            if (relevant(type, plugin, outside) && !surface.contains(type)) {
                problems.add("missing class " + type);
            }
        }
        return new Result(problems, checked, compilerLacks, outside.size());
    }

    /** A compiler type the surface must provide. */
    private boolean relevant(String type, PluginReferences plugin, Set<String> outside) {
        if (plugin.own.contains(type)) {
            return false;
        }
        if (!compiler.contains(type)) {
            if (platform.find(type) != null) {
                return false;
            }
            outside.add(type);
            return false;
        }
        return true;
    }

    private static String member(PluginReferences.Ref ref, Classpath linked, ClassIndex declarations) {
        ClassNode owner = declarations.get(ref.owner());
        if (owner == null) {
            return "class missing";
        }
        boolean ownerIsInterface = (owner.access & Opcodes.ACC_INTERFACE) != 0;
        if (!ref.access().isField() && ref.itf() != ownerIsInterface) {
            return ownerIsInterface ? "owner is an interface" : "owner is not an interface";
        }
        boolean wantsStatic = ref.access() == PluginReferences.Access.STATIC_FIELD
                || ref.access() == PluginReferences.Access.STATIC_METHOD;
        Classpath.Resolved resolved = ref.access().isField()
                ? linked.resolveField(ref.owner(), ref.name(), ref.desc())
                : linked.resolveMethod(ref.owner(), ref.name(), ref.desc());
        if (resolved == null) {
            return "does not resolve";
        }
        if (((resolved.access() & Opcodes.ACC_STATIC) != 0) != wantsStatic) {
            return wantsStatic ? "member is not static" : "member is static";
        }
        return null;
    }

    /** {@code LinkageCheck --surface <jar> --compiler <jar> --stdlib <jar> <plugin.jar>...} */
    public static void main(String[] args) throws java.io.IOException {
        java.nio.file.Path surfaceJar = null;
        java.nio.file.Path compilerJar = null;
        java.nio.file.Path stdlibJar = null;
        List<java.nio.file.Path> plugins = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--surface" -> surfaceJar = java.nio.file.Path.of(args[++i]);
                case "--compiler" -> compilerJar = java.nio.file.Path.of(args[++i]);
                case "--stdlib" -> stdlibJar = java.nio.file.Path.of(args[++i]);
                default -> plugins.add(java.nio.file.Path.of(args[i]));
            }
        }
        int failures = 0;
        try (ClassIndex surface = new ClassIndex(surfaceJar, false);
             ClassIndex compiler = new ClassIndex(compilerJar, false);
             ClassIndex stdlib = new ClassIndex(stdlibJar, false)) {
            LinkageCheck check = new LinkageCheck(surface, compiler, stdlib);
            for (java.nio.file.Path p : plugins) {
                try (ClassIndex jar = new ClassIndex(p, true)) {
                    Result r = check.check(PluginReferences.scan(jar), jar);
                    System.out.printf("%-52s %5d links, %3d broken, %3d absent from the compiler too, %2d non-compiler types%n",
                            p.getFileName(), r.checked(), r.problems().size(), r.compilerLacks(), r.outsideCompiler());
                    r.problems().stream().limit(20).forEach(x -> System.out.println("    " + x));
                    failures += r.problems().size();
                }
            }
        }
        if (failures > 0) {
            System.exit(1);
        }
    }
}
