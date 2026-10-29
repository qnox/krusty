package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSDeclaration;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Supplier;

// The members `getAllProperties` or `getAllFunctions` lists (`put_members` in
// `src/ksp/protocol.rs`). A declaration of another source classifier is looked up when asked for,
// as a later file may declare it.
final class Members {
    private final List<Supplier<KSDeclaration>> entries;

    Members(List<Supplier<KSDeclaration>> entries) {
        this.entries = entries;
    }

    // The member at `position`.
    KSDeclaration get(int position) {
        return entries.get(position).get();
    }

    // The members, each cast to the kind the query lists.
    <T> List<T> as(Class<T> kind) {
        List<T> members = new ArrayList<>(entries.size());
        for (Supplier<KSDeclaration> entry : entries) {
            members.add(kind.cast(entry.get()));
        }
        return members;
    }
}
