package krusty.ksphost;

import com.google.devtools.ksp.processing.KSBuiltIns;
import com.google.devtools.ksp.processing.Resolver;
import com.google.devtools.ksp.symbol.KSAnnotated;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSDeclarationContainer;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSFunctionDeclaration;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSPropertyAccessor;
import com.google.devtools.ksp.symbol.KSPropertyDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Variance;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// What a processor's `process` reads in one round.
final class HostResolver implements Resolver {
    private final Symbols symbols;
    private final List<KSFile> allFiles;
    private final List<KSFile> newFiles;
    private final Name moduleName;

    HostResolver(Symbols symbols, Set<String> previousPaths, String moduleName) {
        this.symbols = symbols;
        this.allFiles = new ArrayList<>(symbols.files);
        this.newFiles = new ArrayList<>();
        for (SourceFile file : symbols.files) {
            if (!previousPaths.contains(file.getFilePath())) {
                newFiles.add(file);
            }
        }
        this.moduleName = new Name(moduleName);
    }

    @Override
    public Sequence<KSFile> getNewFiles() {
        return Nodes.sequence(newFiles);
    }

    @Override
    public Sequence<KSFile> getAllFiles() {
        return Nodes.sequence(allFiles);
    }

    @Override
    public Sequence<KSAnnotated> getSymbolsWithAnnotation(String annotationName, boolean inDepth) {
        throw Nodes.notImplemented("Resolver.getSymbolsWithAnnotation");
    }

    @Override
    public KSClassDeclaration getClassDeclarationByName(KSName name) {
        return symbols.classNamed(name.asString());
    }

    @Override
    public Sequence<KSFunctionDeclaration> getFunctionDeclarationsByName(
            KSName name, boolean includeTopLevel) {
        throw Nodes.notImplemented("Resolver.getFunctionDeclarationsByName");
    }

    @Override
    public KSPropertyDeclaration getPropertyDeclarationByName(KSName name, boolean includeTopLevel) {
        throw Nodes.notImplemented("Resolver.getPropertyDeclarationByName");
    }

    @Override
    public KSTypeArgument getTypeArgument(KSTypeReference typeRef, Variance variance) {
        throw Nodes.notImplemented("Resolver.getTypeArgument");
    }

    @Override
    public KSName getKSNameFromString(String name) {
        return new Name(name);
    }

    @Override
    public KSTypeReference createKSTypeReferenceFromKSType(KSType type) {
        throw Nodes.notImplemented("Resolver.createKSTypeReferenceFromKSType");
    }

    @Override
    public KSBuiltIns getBuiltIns() {
        throw Nodes.notImplemented("Resolver.builtIns");
    }

    @Override
    public String mapToJvmSignature(KSDeclaration declaration) {
        // KSP2 describes no typealias.
        if (declaration instanceof SourceTypeAlias) {
            return null;
        }
        String signature = null;
        if (declaration instanceof SourceClass) {
            signature = ((SourceClass) declaration).jvmSignature;
        } else if (declaration instanceof SourceFunction) {
            signature = ((SourceFunction) declaration).jvmSignature;
        } else if (declaration instanceof SourceProperty) {
            signature = ((SourceProperty) declaration).jvmSignature;
        }
        // A declaration krusty could not describe is refused.
        if (signature == null) {
            throw Nodes.notImplemented("Resolver.mapToJvmSignature");
        }
        return signature;
    }

    @Override
    public boolean overrides(KSDeclaration overrider, KSDeclaration overridee) {
        throw Nodes.notImplemented("Resolver.overrides");
    }

    @Override
    public boolean overrides(
            KSDeclaration overrider, KSDeclaration overridee, KSClassDeclaration containingClass) {
        throw Nodes.notImplemented("Resolver.overrides");
    }

    @Override
    public String getJvmName(KSFunctionDeclaration declaration) {
        if (declaration instanceof SourceFunction && ((SourceFunction) declaration).jvmName != null) {
            return ((SourceFunction) declaration).jvmName;
        }
        throw Nodes.notImplemented("Resolver.getJvmName");
    }

    @Override
    public String getJvmName(KSPropertyAccessor accessor) {
        if (accessor instanceof SourceAccessor && ((SourceAccessor) accessor).jvmName != null) {
            return ((SourceAccessor) accessor).jvmName;
        }
        throw Nodes.notImplemented("Resolver.getJvmName");
    }


    @Override
    public String getOwnerJvmClassName(KSPropertyDeclaration declaration) {
        throw Nodes.notImplemented("Resolver.getOwnerJvmClassName");
    }

    @Override
    public String getOwnerJvmClassName(KSFunctionDeclaration declaration) {
        throw Nodes.notImplemented("Resolver.getOwnerJvmClassName");
    }

    @Override
    public Sequence<KSType> getJvmCheckedException(KSFunctionDeclaration function) {
        throw Nodes.notImplemented("Resolver.getJvmCheckedException");
    }

    @Override
    public Sequence<KSType> getJvmCheckedException(KSPropertyAccessor accessor) {
        throw Nodes.notImplemented("Resolver.getJvmCheckedException");
    }

    @Override
    public Sequence<KSDeclaration> getDeclarationsFromPackage(String packageName) {
        throw Nodes.notImplemented("Resolver.getDeclarationsFromPackage");
    }

    @Override
    public KSName mapJavaNameToKotlin(KSName javaName) {
        String mapped = symbols.classpath().kotlinName(javaName.asString());
        return mapped == null ? null : new Name(mapped);
    }

    @Override
    public KSName mapKotlinNameToJava(KSName kotlinName) {
        String mapped = symbols.classpath().javaName(kotlinName.asString());
        return mapped == null ? null : new Name(mapped);
    }

    @Override
    public Sequence<KSDeclaration> getDeclarationsInSourceOrder(KSDeclarationContainer container) {
        return container.getDeclarations();
    }

    @Override
    public Set<Modifier> effectiveJavaModifiers(KSDeclaration declaration) {
        throw Nodes.notImplemented("Resolver.effectiveJavaModifiers");
    }

    @Override
    public KSTypeReference getJavaWildcard(KSTypeReference reference) {
        throw Nodes.notImplemented("Resolver.getJavaWildcard");
    }

    @Override
    public boolean isJavaRawType(KSType type) {
        throw Nodes.notImplemented("Resolver.isJavaRawType");
    }

    @Override
    public Sequence<KSAnnotation> getPackageAnnotations(String packageName) {
        throw Nodes.notImplemented("Resolver.getPackageAnnotations");
    }

    @Override
    public Sequence<String> getPackagesWithAnnotation(String annotationName) {
        throw Nodes.notImplemented("Resolver.getPackagesWithAnnotation");
    }

    @Override
    public KSName getModuleName() {
        return moduleName;
    }
}
