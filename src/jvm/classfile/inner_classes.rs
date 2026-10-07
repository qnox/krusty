//! The `InnerClasses` table: which nested classes a class names, and in what order.

use std::collections::HashMap;
use std::rc::Rc;

use super::{ClassWriter, InnerClassResolver, InnerClassSpec};

/// kotlinc's `fqNameWhenAvailable` of each class declared in executable code, by internal name.
pub(crate) type DeclarationPaths = Rc<HashMap<String, String>>;

/// What a referenced `InnerClasses` table knows of the file's classes declared in executable code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LocalDeclarations {
    /// The qualified names their rows sort by.
    paths: DeclarationPaths,
    /// The internal name of the class each is declared in, by its own: kotlinc's `ClassCodegen`
    /// lists every class it generates from its own code, whether or not that code names it.
    declaring: Rc<HashMap<String, String>>,
    /// Specialized suspend lambdas regenerated into a caller. The caller references them without
    /// an `InnerClasses` row; the class itself and classes it encloses still list the row.
    call_sites: Rc<std::collections::HashSet<String>>,
}

/// How a class's `InnerClasses` table is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InnerClassTable {
    /// kotlinc's `ClassCodegen`: every nested class the class references or its code declares,
    /// stably sorted by its qualified name. A class declared in executable code is named by the
    /// declarations enclosing it; any other by its outer class's name and its own.
    Referenced(LocalDeclarations),
    /// Every row as it was visited, as ASM writes a class that is copied.
    Visited,
}

impl Default for InnerClassTable {
    fn default() -> Self {
        Self::Referenced(LocalDeclarations::default())
    }
}

impl ClassWriter {
    /// Register a candidate `InnerClasses` entry (a nested class in this file). `finish` emits it only
    /// if `inner` is referenced as a class constant. Register the whole file's nest on every writer —
    /// the per-class filter then yields exactly the entries kotlinc emits for that class.
    pub fn add_inner_class(&mut self, spec: InnerClassSpec) {
        // Preserve the first registration because its order affects byte identity.
        if self
            .inner_class_candidates
            .iter()
            .any(|s| s.inner == spec.inner)
        {
            return;
        }
        self.inner_class_candidates.push(spec);
    }

    pub fn set_inner_class_resolver(&mut self, resolver: Option<InnerClassResolver>) {
        self.inner_class_resolver = resolver;
    }

    /// The qualified names the referenced table sorts the file's local classes by.
    pub(crate) fn set_declaration_paths(&mut self, paths: DeclarationPaths) {
        if let InnerClassTable::Referenced(own) = &mut self.inner_class_table {
            own.paths = paths;
        }
    }

    /// The class each of the file's local and anonymous classes is declared in. A class lists
    /// the ones its own code declares.
    pub(crate) fn set_declaring_classes(&mut self, declaring: Rc<HashMap<String, String>>) {
        if let InnerClassTable::Referenced(own) = &mut self.inner_class_table {
            own.declaring = declaring;
        }
    }

    /// Specialized suspend lambdas a caller constructs without listing them.
    pub(crate) fn set_call_site_classes(
        &mut self,
        call_sites: Rc<std::collections::HashSet<String>>,
    ) {
        if let InnerClassTable::Referenced(own) = &mut self.inner_class_table {
            own.call_sites = call_sites;
        }
    }

    /// Write every registered entry, in registration order: the table of a class copied from a
    /// compiled one, whose writer is handed the rows one by one (`ClassVisitor.visitInnerClass`).
    pub(crate) fn keep_visited_inner_classes(&mut self) {
        self.inner_class_table = InnerClassTable::Visited;
    }

    /// Whether one registered nested-class declaration belongs in this writer's final
    /// `InnerClasses` table. Keep seeding and attribute construction on this one predicate: adding
    /// constants for a rejected row changes byte identity, while omitting an annotation-only row
    /// changes the class structure.
    pub(super) fn retains_inner_class(&self, spec: &InnerClassSpec) -> bool {
        self.retains_inner_class_with_presence(spec, self.names_class(&spec.inner))
    }

    /// Evaluate the shared retention predicate against an explicit view of the constant pool.
    /// Seeding uses a hypothetical presence bit while it computes the retained-set fixpoint;
    /// attribute construction passes the real pool state through `retains_inner_class`.
    fn retains_inner_class_with_presence(
        &self,
        spec: &InnerClassSpec,
        inner_present: bool,
    ) -> bool {
        if self.call_site_regenerated(&spec.inner) {
            return self.internal_name == spec.inner || self.declared_in_class(&spec.inner);
        }
        self.inner_class_table == InnerClassTable::Visited
            || spec.outer.as_deref() == Some(self.internal_name.as_str())
            || inner_present
            || self.declares(&spec.inner)
            || self.annotation_class_refs.contains(&spec.inner)
            || self.descriptor_mentions(&spec.inner)
    }

    /// Whether `inner` is a call-site suspend lambda the referencing class must not list.
    fn call_site_regenerated(&self, inner: &str) -> bool {
        match &self.inner_class_table {
            InnerClassTable::Referenced(local) => local.call_sites.contains(inner),
            InnerClassTable::Visited => false,
        }
    }

    /// Whether this class is declared inside `owner`.
    fn declared_in_class(&self, owner: &str) -> bool {
        match &self.inner_class_table {
            InnerClassTable::Referenced(local) => {
                local.declaring.get(&self.internal_name).map(String::as_str) == Some(owner)
            }
            InnerClassTable::Visited => false,
        }
    }

    /// Whether this class's code declares the local or anonymous class `inner`.
    fn declares(&self, inner: &str) -> bool {
        match &self.inner_class_table {
            InnerClassTable::Referenced(local) => {
                local.declaring.get(inner) == Some(&self.internal_name)
            }
            InnerClassTable::Visited => false,
        }
    }

    /// Seed the `InnerClasses` entries' outer-class refs and simple names at kotlinc's
    /// post-metadata pool position, in the ORDER the finished table will list them (sorted by inner
    /// internal name), and only for the entries that table will actually keep.
    ///
    /// kotlinc interns these as it visits the sorted table, so a class whose table has a sibling
    /// sorting BEFORE its own row must intern that sibling's name first: `Foo$$serializer` sorts
    /// ahead of `Foo$Companion` (`'$'` < `'C'`). Seeding only this class's own row put its name
    /// first and left the two entries transposed in the pool — the class then matched kotlinc in
    /// every other respect while still differing byte-wise.
    pub(in crate::jvm) fn seed_inner_class_names(&mut self) {
        // A referenced dependency nest may not be among the source file's registered candidates.
        // Discover those rows before sorting; resolving them later from `finish` would intern their
        // names after every source row and recreate the very order mismatch this seed prevents.
        self.resolve_inner_classes();
        // kotlinc interns PER ROW, in the attribute's own field order: the inner class, then the
        // outer class, then the simple name. Interning every row's classes first and every name
        // second matches only when no row's INNER class needs interning — true for a flat table
        // (`Foo$$serializer`/`Foo$Companion`, whose inners the class already references) and for a
        // single chain, but wrong as soon as an enclosing row's inner is not otherwise referenced:
        // `A$B$C$Companion` interns `Class(A$B)` at its own row, between two other rows' entries.
        self.intern_retained_inner_rows();
    }

    /// The registered rows the finished `InnerClasses` table keeps, in table order.
    ///
    /// Retention is a FIXPOINT, computed WITHOUT interning: a row is kept once its inner class is
    /// present, and a kept row's outer is what makes an enclosing row present. kotlinc adds every
    /// enclosing class of a referenced nested class to the table, so `A$B$C` keeps `A$B` even
    /// when that row sorts first and nothing else names `A$B`.
    fn retained_inner_rows(&self) -> Vec<InnerClassSpec> {
        let specs = &self.inner_class_candidates;
        let mut retained = vec![false; specs.len()];
        let mut seeded: std::collections::HashSet<&str> = std::collections::HashSet::new();
        loop {
            let mut grew = false;
            for (index, spec) in specs.iter().enumerate() {
                if retained[index] {
                    continue;
                }
                let present = self.names_class(&spec.inner) || seeded.contains(spec.inner.as_str());
                if self.retains_inner_class_with_presence(spec, present) {
                    retained[index] = true;
                    grew = true;
                    seeded.insert(&spec.inner);
                    if let Some(outer) = &spec.outer {
                        seeded.insert(outer);
                    }
                }
            }
            if !grew {
                break;
            }
        }
        specs
            .iter()
            .zip(retained)
            .filter(|&(_, keep)| keep)
            .map(|(spec, _)| spec.clone())
            .collect()
    }

    /// Intern each kept row's inner class, outer class and simple name, row by row. Both the
    /// post-metadata seeding and the class write go through here, so they keep the same rows.
    pub(super) fn intern_retained_inner_rows(&mut self) {
        for spec in self.retained_inner_rows() {
            self.cp.class(&spec.inner);
            if let Some(outer) = &spec.outer {
                self.cp.class(outer);
            }
            if let Some(name) = &spec.name {
                self.cp.utf8(name);
            }
        }
    }

    pub(super) fn resolve_inner_classes(&mut self) {
        if let Some(resolve) = self.inner_class_resolver.clone() {
            // Class constants first, then annotation types (an applied annotation is a reference
            // even though only its descriptor string reaches the pool), then the classes that
            // descriptors and generic signatures name. kotlinc's writer sorts the final table by
            // inner name, so collection order does not leak into the attribute.
            let mut referenced = self.cp.class_names();
            let mut annotation_refs: Vec<String> =
                self.annotation_class_refs.iter().cloned().collect();
            annotation_refs.sort();
            referenced.extend(annotation_refs);
            referenced.extend(self.signature_classes());
            for inner in referenced {
                if self
                    .inner_class_candidates
                    .iter()
                    .any(|candidate| candidate.inner == inner)
                {
                    continue;
                }
                let Some(details) = resolve(&inner) else {
                    continue;
                };
                self.add_inner_class(InnerClassSpec {
                    inner,
                    outer: details.outer,
                    name: details.name,
                    access: details.access,
                });
            }
        }
        // kotlinc writes the complete table stably sorted by qualified name (`C.Companion`,
        // `C.NestObj`, `C.Nested`, case-sensitive), classpath-discovered entries included, the
        // classes with an equal name in the order they were met.
        if let InnerClassTable::Referenced(local) = &self.inner_class_table {
            let keys = qualified_names(&self.inner_class_candidates, &local.paths);
            // Pool order follows the methods, the order kotlinc's codegen meets their classes.
            let met: HashMap<String, usize> = self
                .cp
                .class_names()
                .into_iter()
                .enumerate()
                .map(|(rank, class)| (class, rank))
                .collect();
            let mut rows: Vec<_> = keys
                .into_iter()
                .zip(std::mem::take(&mut self.inner_class_candidates))
                .map(|(key, spec)| {
                    let rank = met.get(spec.inner.as_str()).copied().unwrap_or(usize::MAX);
                    (key, rank, spec)
                })
                .collect();
            order_by_enclosure(&mut rows, &local.declaring);
            self.inner_class_candidates = rows.into_iter().map(|(_, _, spec)| spec).collect();
        }
    }
}

/// Apply the declaration graph as precedence constraints over the ordinary qualified-name order.
///
/// An ancestry relation is only a PARTIAL order. Putting it directly in a `sort_by` comparator and
/// using qualified names for unrelated rows is not transitive: an unrelated row can close a cycle
/// between an ancestor and its descendant. Start with the normal stable order, then perform a
/// stable topological selection so every declaring row precedes the rows generated from its code.
fn order_by_enclosure(
    rows: &mut Vec<(String, usize, InnerClassSpec)>,
    declaring: &HashMap<String, String>,
) {
    rows.sort_by(|(key, rank, _), (other_key, other_rank, _)| {
        key.cmp(other_key).then(rank.cmp(other_rank))
    });
    let mut remaining = std::mem::take(rows);
    rows.reserve(remaining.len());
    while !remaining.is_empty() {
        let root = remaining
            .iter()
            .position(|(_, _, candidate)| {
                !remaining.iter().any(|(_, _, possible_ancestor)| {
                    candidate.inner != possible_ancestor.inner
                        && declared_within(declaring, &candidate.inner, &possible_ancestor.inner)
                })
            })
            // A cyclic declaration graph cannot impose ancestry. Preserve the already-established
            // qualified-name order for that component rather than looping or giving `sort_by` an
            // invalid comparator.
            .unwrap_or(0);
        rows.push(remaining.remove(root));
    }
}

/// Whether `class` is generated from code lexically owned by `ancestor`, directly or through
/// other generated classes. The relation is recorded from common-IR enclosure identities; JVM
/// spelling is payload only and is never parsed to reconstruct it.
fn declared_within(declaring: &HashMap<String, String>, class: &str, ancestor: &str) -> bool {
    let mut class = class;
    for _ in 0..=declaring.len() {
        let Some(owner) = declaring.get(class) else {
            return false;
        };
        if owner == ancestor {
            return true;
        }
        class = owner;
    }
    false
}

/// kotlinc's `fqNameWhenAvailable` of each row's class: from `paths` for a class declared in
/// executable code, otherwise its outer class's name and its own simple name, or the dotted
/// internal name of a top-level class.
fn qualified_names(rows: &[InnerClassSpec], paths: &HashMap<String, String>) -> Vec<String> {
    fn name(
        inner: &str,
        rows: &HashMap<&str, &InnerClassSpec>,
        paths: &HashMap<String, String>,
        depth: usize,
    ) -> String {
        if let Some(path) = paths.get(inner) {
            return path.clone();
        }
        match rows.get(inner) {
            Some(InnerClassSpec {
                outer: Some(outer),
                name: Some(simple),
                ..
            }) if depth < rows.len() => {
                format!("{}.{simple}", name(outer, rows, paths, depth + 1))
            }
            _ => inner.replace('/', "."),
        }
    }
    let by_inner: HashMap<&str, &InnerClassSpec> =
        rows.iter().map(|row| (row.inner.as_str(), row)).collect();
    rows.iter()
        .map(|row| name(&row.inner, &by_inner, paths, 0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, rank: usize, inner: &str) -> (String, usize, InnerClassSpec) {
        (
            key.to_string(),
            rank,
            InnerClassSpec {
                inner: inner.to_string(),
                outer: None,
                name: None,
                access: 0,
            },
        )
    }

    #[test]
    fn enclosure_is_a_precedence_constraint_not_a_sort_comparator() {
        let declaring = HashMap::from([
            ("Outer$lambda$inner".to_string(), "Outer$lambda".to_string()),
            ("Outer$lambda".to_string(), "Outer".to_string()),
        ]);
        // Qualified-name order puts the descendant first and an unrelated row between the two.
        // Combining those two orders in a pairwise comparator forms a comparison cycle.
        let mut rows = vec![
            row("a", 0, "Outer$lambda$inner"),
            row("b", 0, "Unrelated"),
            row("c", 0, "Outer$lambda"),
        ];
        order_by_enclosure(&mut rows, &declaring);
        let inners: Vec<_> = rows.iter().map(|(_, _, row)| row.inner.as_str()).collect();
        assert_eq!(inners, ["Unrelated", "Outer$lambda", "Outer$lambda$inner"]);
    }

    #[test]
    fn equal_qualified_names_keep_ancestor_before_descendant() {
        let declaring =
            HashMap::from([("Facade$call$1$1".to_string(), "Facade$call$1".to_string())]);
        let mut rows = vec![
            row("call.<anonymous>", 0, "Facade$call$1$1"),
            row("call.<anonymous>", 1, "Facade$call$1"),
        ];
        order_by_enclosure(&mut rows, &declaring);
        let inners: Vec<_> = rows.iter().map(|(_, _, row)| row.inner.as_str()).collect();
        assert_eq!(inners, ["Facade$call$1", "Facade$call$1$1"]);
    }
}
