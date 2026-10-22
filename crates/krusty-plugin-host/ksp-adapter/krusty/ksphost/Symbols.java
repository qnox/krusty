package krusty.ksphost;

import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import java.io.DataInputStream;
import java.io.IOException;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

// One round's symbol model, read from what krusty sends (`KspRound`): the module's source files and
// their classifiers, and the library classifiers they name.
final class Symbols {
    final List<SourceFile> files = new ArrayList<>();
    private final Map<String, SourceClass> sourceClasses = new HashMap<>();
    private final Map<String, LibraryClass> libraryClasses = new HashMap<>();

    static Symbols read(DataInputStream in) throws IOException {
        Symbols symbols = new Symbols();
        int fileCount = in.readInt();
        for (int i = 0; i < fileCount; i++) {
            String path = Wire.readString(in);
            String packageName = Wire.readString(in);
            boolean complete = Wire.readBoolean(in);
            SourceFile file = new SourceFile(path, packageName, complete);
            symbols.readClasses(in, file, null, file.declarations);
            symbols.files.add(file);
        }
        return symbols;
    }

    private void readClasses(
            DataInputStream in,
            SourceFile file,
            SourceClass owner,
            List<KSDeclaration> into)
            throws IOException {
        int count = in.readInt();
        for (int i = 0; i < count; i++) {
            String simpleName = Wire.readString(in);
            String qualifiedName = Wire.readString(in);
            ClassKind kind = classKind(in.readByte());
            int line = in.readInt();
            List<String[]> annotations =
                    Wire.readList(in, a -> new String[] {Wire.readString(a), Wire.readString(a)});
            List<Object[]> primaryParameters = Wire.readBoolean(in)
                    ? Wire.readList(in, p -> new Object[] {Wire.readString(p), p.readByte()})
                    : null;
            boolean complete = Wire.readBoolean(in);
            SourceClass declaration =
                    new SourceClass(file, owner, simpleName, qualifiedName, kind, line, complete);
            for (String[] annotation : annotations) {
                declaration.annotations.add(
                        new SourceAnnotation(declaration, annotation[0], annotation[1], this));
            }
            if (primaryParameters != null) {
                PrimaryConstructor constructor = declaration.addPrimaryConstructor();
                for (Object[] parameter : primaryParameters) {
                    byte flags = (Byte) parameter[1];
                    constructor.parameters.add(new Parameter(
                            constructor,
                            (String) parameter[0],
                            (flags & 1) != 0,
                            (flags & 2) != 0,
                            (flags & 4) != 0,
                            (flags & 8) != 0));
                }
            }
            readClasses(in, file, declaration, declaration.declarations);
            sourceClasses.put(qualifiedName, declaration);
            into.add(declaration);
        }
    }

    private static ClassKind classKind(byte tag) throws IOException {
        switch (tag) {
            case 0: return ClassKind.INTERFACE;
            case 1: return ClassKind.CLASS;
            case 2: return ClassKind.ENUM_CLASS;
            case 3: return ClassKind.ENUM_ENTRY;
            case 4: return ClassKind.OBJECT;
            case 5: return ClassKind.ANNOTATION_CLASS;
            default: throw new IOException("unknown class kind " + tag);
        }
    }

    // The classifier named `qualifiedName`: the module's own when it declares one, else the
    // classpath's.
    KSClassDeclaration classNamed(String qualifiedName) {
        SourceClass source = sourceClasses.get(qualifiedName);
        if (source != null) {
            return source;
        }
        return libraryClasses.computeIfAbsent(qualifiedName, LibraryClass::new);
    }
}
