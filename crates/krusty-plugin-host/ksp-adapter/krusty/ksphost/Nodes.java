package krusty.ksphost;

import com.google.devtools.ksp.symbol.FileLocation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import java.util.Collections;
import java.util.List;
import kotlin.sequences.Sequence;

// What every node of the host's symbol model shares.
final class Nodes {
    private Nodes() {}

    // A view of `list` that takes a fresh iterator for each traversal, as KSP's collection-backed
    // sequences do: a processor may keep a returned sequence and read it more than once.
    static <T> Sequence<T> sequence(List<T> list) {
        return list::iterator;
    }

    static <T> Sequence<T> empty() {
        return sequence(Collections.<T>emptyList());
    }

    static NotImplemented notImplemented(String member) {
        return new NotImplemented(member);
    }

    static FileLocation location(SourceFile file, int line) {
        return new FileLocation(file.getFilePath(), line);
    }

    static Sequence<KSDeclaration> declarations(
            String owner, boolean complete, List<KSDeclaration> declarations) {
        if (!complete) {
            throw notImplemented(owner + ".declarations of a body with functions or properties");
        }
        return sequence(declarations);
    }
}
