package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import java.io.File;
import java.util.ArrayList;
import java.util.List;
import kotlin.sequences.Sequence;

final class SourceFile implements KSFile {
    private final String path;
    private final Name packageName;
    private final boolean declarationsComplete;
    final List<KSDeclaration> declarations = new ArrayList<>();

    SourceFile(String path, String packageName, boolean declarationsComplete) {
        this.path = path;
        this.packageName = new Name(packageName);
        this.declarationsComplete = declarationsComplete;
    }

    @Override
    public KSName getPackageName() {
        return packageName;
    }

    @Override
    public String getFileName() {
        return new File(path).getName();
    }

    @Override
    public String getFilePath() {
        return path;
    }

    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        return Nodes.declarations("KSFile", declarationsComplete, declarations);
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSFile.annotations");
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        return Nodes.location(this, 1);
    }

    @Override
    public KSNode getParent() {
        return null;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitFile(this, data);
    }

    @Override
    public String toString() {
        return "File: " + getFileName();
    }
}
