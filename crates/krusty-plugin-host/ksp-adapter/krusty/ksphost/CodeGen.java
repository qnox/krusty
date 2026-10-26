package krusty.ksphost;

import com.google.devtools.ksp.processing.CodeGenerator;
import com.google.devtools.ksp.processing.Dependencies;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.file.FileAlreadyExistsException;
import java.util.ArrayList;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

// KSP's code generator: a file goes below the output directory its extension selects, at its
// package's path, and no path may be generated twice in a run.
final class CodeGen implements CodeGenerator {
    static final byte KOTLIN = 0;
    static final byte JAVA = 1;
    static final byte CLASS = 2;
    static final byte RESOURCE = 3;

    private final File kotlinDir;
    private final File javaDir;
    private final File classDir;
    private final File resourceDir;
    private final Map<String, File> files = new LinkedHashMap<>();
    private final List<Generated> created = new ArrayList<>();

    static final class Generated {
        final byte kind;
        final String path;

        Generated(byte kind, String path) {
            this.kind = kind;
            this.path = path;
        }
    }

    CodeGen(File kotlinDir, File javaDir, File classDir, File resourceDir) {
        this.kotlinDir = kotlinDir;
        this.javaDir = javaDir;
        this.classDir = classDir;
        this.resourceDir = resourceDir;
    }

    // The files created since the last call.
    List<Generated> drain() {
        List<Generated> drained = new ArrayList<>(created);
        created.clear();
        return drained;
    }

    private static byte kindOf(String extensionName) {
        switch (extensionName) {
            case "kt": return KOTLIN;
            case "java": return JAVA;
            case "class": return CLASS;
            default: return RESOURCE;
        }
    }

    private File baseDir(byte kind) {
        switch (kind) {
            case KOTLIN: return kotlinDir;
            case JAVA: return javaDir;
            case CLASS: return classDir;
            default: return resourceDir;
        }
    }

    private static String extension(String extensionName) {
        return extensionName.isEmpty() ? "" : "." + extensionName;
    }

    @Override
    public OutputStream createNewFile(
            Dependencies dependencies, String packageName, String fileName, String extensionName) {
        // KSP2 turns the package's dots into directories without checking that it is a Kotlin
        // package, since a resource such as a service descriptor lives under `META-INF`.
        String packageDirs =
                packageName.isEmpty()
                        ? ""
                        : String.join(File.separator, packageName.split("\\.")) + File.separator;
        return create(kindOf(extensionName), packageDirs + fileName + extension(extensionName));
    }

    @Override
    public OutputStream createNewFileByPath(
            Dependencies dependencies, String path, String extensionName) {
        return create(kindOf(extensionName), path + extension(extensionName));
    }

    private OutputStream create(byte kind, String path) {
        File base = baseDir(kind);
        File file = new File(base, path);
        String key = file.getPath();
        try {
            if (!file.getCanonicalPath().startsWith(base.getCanonicalPath() + File.separator)) {
                throw new IllegalStateException("requested path is outside the bounds of the required directory");
            }
            if (files.containsKey(key)) {
                throw new FileAlreadyExistsException(file.getPath());
            }
            File parent = file.getParentFile();
            if (!parent.exists() && !parent.mkdirs()) {
                throw new IllegalStateException("failed to make parent directories.");
            }
            OutputStream out = new FileOutputStream(file);
            files.put(key, file);
            created.add(new Generated(kind, file.getPath()));
            return out;
        } catch (IOException error) {
            // KSP is Kotlin and throws these unchecked, so a processor sees the same exception.
            throw CodeGen.<RuntimeException>unchecked(error);
        }
    }

    private static <E extends Throwable> E unchecked(Throwable error) throws E {
        throw (E) error;
    }

    @Override
    public void associate(List<? extends KSFile> sources, String packageName, String fileName, String extensionName) {
        // Associations feed KSP's incremental processing, which a krusty run does not do.
    }

    @Override
    public void associateByPath(List<? extends KSFile> sources, String path, String extensionName) {
        // As associate.
    }

    @Override
    public void associateWithClasses(
            List<? extends KSClassDeclaration> classes, String packageName, String fileName, String extensionName) {
        // As associate.
    }

    @Override
    public Collection<File> getGeneratedFile() {
        return new ArrayList<>(files.values());
    }
}
