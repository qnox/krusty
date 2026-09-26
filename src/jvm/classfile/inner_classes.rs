//! The `InnerClasses` table: which nested classes a class names, and in what order.

use super::{ClassWriter, InnerClassResolver, InnerClassSpec};

/// How a class's `InnerClasses` table is built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum InnerClassTable {
    /// kotlinc's `ClassCodegen`: every nested class the class references, sorted by name.
    #[default]
    Referenced,
    /// Every row as it was visited, as ASM writes a class that is copied.
    Visited,
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
        self.retains_inner_class_with_presence(spec, self.cp.has_class(&spec.inner))
    }

    /// Evaluate the shared retention predicate against an explicit view of the constant pool.
    /// Seeding uses a hypothetical presence bit while it computes the retained-set fixpoint;
    /// attribute construction passes the real pool state through `retains_inner_class`.
    fn retains_inner_class_with_presence(
        &self,
        spec: &InnerClassSpec,
        inner_present: bool,
    ) -> bool {
        self.inner_class_table == InnerClassTable::Visited
            || spec.outer.as_deref() == Some(self.internal_name.as_str())
            || inner_present
            || self.annotation_class_refs.contains(&spec.inner)
            || self.descriptor_mentions(&spec.inner)
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
                let present =
                    self.cp.has_class(&spec.inner) || seeded.contains(spec.inner.as_str());
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
            .filter_map(|(spec, keep)| keep.then(|| spec.clone()))
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
            // even though only its descriptor string reaches the pool). kotlinc's writer sorts the
            // final table by inner name, so collection order does not leak into the attribute.
            let mut referenced = self.cp.class_names();
            let mut annotation_refs: Vec<String> =
                self.annotation_class_refs.iter().cloned().collect();
            annotation_refs.sort();
            referenced.extend(annotation_refs);
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
        // kotlinc writes the complete table sorted by inner internal name (`C$Companion`,
        // `C$NestObj`, `C$Nested` — case-sensitive), including classpath-discovered entries.
        if self.inner_class_table == InnerClassTable::Referenced {
            self.inner_class_candidates
                .sort_by(|a, b| a.inner.cmp(&b.inner));
        }
    }
}
