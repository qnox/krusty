//! The `InnerClasses` table: which nested classes a class names, and in what order.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use super::ClassWriter;

/// One candidate `InnerClasses` entry: the nested class, its enclosing class (`None` for an anonymous
/// local), its simple name (`None` when anonymous), and the entry's access flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerClassSpec {
    pub inner: String,
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerClassDetails {
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

pub type InnerClassResolver = Rc<dyn Fn(&str) -> Option<InnerClassDetails>>;

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
    /// The table orders the file's writers computed, shared by all of them.
    orders: Rc<TableOrders>,
}

/// Table orders already computed for one file's writers.
///
/// Every writer of a file registers the file's whole nest, and most of them order exactly the same
/// candidates. The order is a function of the candidates and the file's declaration paths and
/// declaring classes, plus the pool position of classes whose qualified names tie. An order computed
/// without a tie is therefore reused by any writer whose candidates are equal, instead of every
/// writer sorting the whole nest again.
#[derive(Default)]
pub(crate) struct TableOrders {
    known: std::cell::RefCell<Vec<TableOrder>>,
}

struct TableOrder {
    candidates: Vec<InnerClassSpec>,
    paths: DeclarationPaths,
    declaring: Rc<HashMap<String, String>>,
    /// The candidate index each table row takes, in table order.
    order: Vec<usize>,
}

/// The orders kept: the registration order a writer starts from and the table order it ends with
/// (which a writer orders again when it is written), with room for a writer's own discoveries.
const KEPT_TABLE_ORDERS: usize = 4;

impl TableOrders {
    fn find(&self, candidates: &[InnerClassSpec], local: &LocalDeclarations) -> Option<Vec<usize>> {
        self.known
            .borrow()
            .iter()
            .find(|known| {
                Rc::ptr_eq(&known.paths, &local.paths)
                    && Rc::ptr_eq(&known.declaring, &local.declaring)
                    && known.candidates == candidates
            })
            .map(|known| known.order.clone())
    }

    fn remember(
        &self,
        candidates: Vec<InnerClassSpec>,
        local: &LocalDeclarations,
        order: Vec<usize>,
    ) {
        let mut known = self.known.borrow_mut();
        if known.len() == KEPT_TABLE_ORDERS {
            known.remove(0);
        }
        known.push(TableOrder {
            candidates,
            paths: local.paths.clone(),
            declaring: local.declaring.clone(),
            order,
        });
    }
}

impl PartialEq for TableOrders {
    /// One file's shared orders are the same orders; their contents are a cache.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for TableOrders {}

impl std::fmt::Debug for TableOrders {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TableOrders")
            .field("known", &self.known.borrow().len())
            .finish()
    }
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

    /// Register several candidates, in order, exactly as [`Self::add_inner_class`] would one at a
    /// time. A file's whole nest is registered on each of its class writers, so the duplicate check
    /// goes through one set instead of a scan of the candidates per entry.
    pub(crate) fn add_inner_classes(&mut self, specs: &[InnerClassSpec]) {
        let mut known: HashSet<&str> = self
            .inner_class_candidates
            .iter()
            .map(|spec| spec.inner.as_str())
            .collect();
        let added: Vec<InnerClassSpec> = specs
            .iter()
            .filter(|spec| known.insert(spec.inner.as_str()))
            .cloned()
            .collect();
        self.inner_class_candidates.extend(added);
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

    /// The table orders shared by the writers of one file.
    pub(crate) fn set_table_orders(&mut self, orders: Rc<TableOrders>) {
        if let InnerClassTable::Referenced(own) = &mut self.inner_class_table {
            own.orders = orders;
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
    ///
    /// Presence only ever adds a row, so the kept rows are the least fixpoint however it is reached:
    /// each row is first judged against the pool once, and a row's seeds then revisit only the row
    /// they name.
    fn retained_inner_rows(&self) -> Vec<InnerClassSpec> {
        let specs = &self.inner_class_candidates;
        let by_inner: HashMap<&str, usize> = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (spec.inner.as_str(), index))
            .collect();
        let mut retained: Vec<bool> = specs
            .iter()
            .map(|spec| self.retains_inner_class_with_presence(spec, self.names_class(&spec.inner)))
            .collect();
        let mut pending: Vec<usize> = (0..specs.len()).filter(|&row| retained[row]).collect();
        let mut seeded: HashSet<&str> = HashSet::new();
        while let Some(row) = pending.pop() {
            let spec = &specs[row];
            for seed in std::iter::once(spec.inner.as_str()).chain(spec.outer.as_deref()) {
                if !seeded.insert(seed) {
                    continue;
                }
                let Some(&seeded_row) = by_inner.get(seed) else {
                    continue;
                };
                if !retained[seeded_row]
                    && self.retains_inner_class_with_presence(&specs[seeded_row], true)
                {
                    retained[seeded_row] = true;
                    pending.push(seeded_row);
                }
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
            let mut known: HashSet<&str> = self
                .inner_class_candidates
                .iter()
                .map(|candidate| candidate.inner.as_str())
                .collect();
            let mut discovered = Vec::new();
            for inner in &referenced {
                if known.contains(inner.as_str()) {
                    continue;
                }
                let Some(details) = resolve(inner) else {
                    continue;
                };
                known.insert(inner);
                discovered.push(InnerClassSpec {
                    inner: inner.clone(),
                    outer: details.outer,
                    name: details.name,
                    access: details.access,
                });
            }
            self.inner_class_candidates.extend(discovered);
        }
        // kotlinc writes the complete table stably sorted by qualified name (`C.Companion`,
        // `C.NestObj`, `C.Nested`, case-sensitive), classpath-discovered entries included, the
        // classes with an equal name in the order they were met.
        if let InnerClassTable::Referenced(local) = &self.inner_class_table {
            if let Some(order) = local.orders.find(&self.inner_class_candidates, local) {
                let mut rows = std::mem::take(&mut self.inner_class_candidates)
                    .into_iter()
                    .map(Some)
                    .collect::<Vec<_>>();
                self.inner_class_candidates = order
                    .into_iter()
                    .map(|row| rows[row].take().expect("each row is ordered once"))
                    .collect();
                return;
            }
            let candidates = self.inner_class_candidates.clone();
            let keys = qualified_names(&self.inner_class_candidates, &local.paths);
            let mut distinct_keys = HashSet::with_capacity(keys.len());
            let tied = !keys.iter().all(|key| distinct_keys.insert(key.as_str()));
            // Pool order follows the methods, the order kotlinc's codegen meets their classes.
            let met: HashMap<String, usize> = self
                .cp
                .class_names()
                .into_iter()
                .enumerate()
                .map(|(rank, class)| (class, rank))
                .collect();
            let rows: Vec<_> = keys
                .into_iter()
                .zip(std::mem::take(&mut self.inner_class_candidates))
                .map(|(key, spec)| {
                    let rank = met.get(spec.inner.as_str()).copied().unwrap_or(usize::MAX);
                    (key, rank, spec)
                })
                .collect();
            self.inner_class_candidates = declaration_order(rows, &local.declaring)
                .into_iter()
                .map(|(_, _, spec)| spec)
                .collect();
            if !tied {
                let index: HashMap<&str, usize> = candidates
                    .iter()
                    .enumerate()
                    .map(|(row, spec)| (spec.inner.as_str(), row))
                    .collect();
                let order = self
                    .inner_class_candidates
                    .iter()
                    .map(|spec| index[spec.inner.as_str()])
                    .collect::<Vec<_>>();
                // A table order is reusable only when each row names a class of its own.
                if index.len() == order.len() {
                    let identity = (0..order.len()).collect();
                    drop(index);
                    local.orders.remember(candidates, local, order);
                    local
                        .orders
                        .remember(self.inner_class_candidates.clone(), local, identity);
                }
            }
        }
    }
}

/// Sort by kotlinc's qualified-name/pool order while keeping an exact declaring class before every
/// class generated from its code. The latter is a partial order: mixing it directly into a pairwise
/// comparator can be non-transitive when an unrelated row sorts between a child and its parent.
/// A stable Kahn walk makes the contract a real total order, using the ordinary sorted position as
/// the priority among rows whose declaring ancestors have already been emitted.
fn declaration_order(
    mut rows: Vec<(String, usize, InnerClassSpec)>,
    declaring: &HashMap<String, String>,
) -> Vec<(String, usize, InnerClassSpec)> {
    rows.sort_by(|(key, rank, _), (other_key, other_rank, _)| {
        key.cmp(other_key).then(rank.cmp(other_rank))
    });
    let by_inner: HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, (_, _, spec))| (spec.inner.as_str(), index))
        .collect();
    let mut children = vec![Vec::new(); rows.len()];
    let mut blocked = vec![false; rows.len()];
    for (child, (_, _, spec)) in rows.iter().enumerate() {
        let mut current = declaring.get(&spec.inner).map(String::as_str);
        let mut seen = HashSet::new();
        while let Some(parent) = current {
            assert!(
                seen.insert(parent),
                "declaring-class ancestry must be acyclic"
            );
            if let Some(&parent) = by_inner.get(parent) {
                children[parent].push(child);
                blocked[child] = true;
                break;
            }
            current = declaring.get(parent).map(String::as_str);
        }
    }

    let mut ready: BTreeSet<usize> = blocked
        .iter()
        .enumerate()
        .filter_map(|(index, &blocked)| (!blocked).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(rows.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for &child in &children[index] {
            blocked[child] = false;
            ready.insert(child);
        }
    }
    assert_eq!(
        order.len(),
        rows.len(),
        "declaring-class ancestry must be acyclic"
    );

    let mut rows = rows.into_iter().map(Some).collect::<Vec<_>>();
    order
        .into_iter()
        .map(|index| rows[index].take().expect("each row is ordered once"))
        .collect()
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
    use super::{declaration_order, HashMap, InnerClassSpec, Rc, TableOrders};
    use crate::jvm::classfile::ClassWriter;

    fn row(key: &str, inner: &str) -> (String, usize, InnerClassSpec) {
        (
            key.to_string(),
            usize::MAX,
            InnerClassSpec {
                inner: inner.to_string(),
                outer: None,
                name: None,
                access: 0,
            },
        )
    }

    #[test]
    fn declaration_constraints_form_a_total_order_with_qualified_names() {
        // The old pairwise comparator formed a cycle here: Parent < Child by ancestry,
        // Child < Unrelated and Unrelated < Parent by qualified name.
        let rows = vec![row("z", "Parent"), row("a", "Child"), row("m", "Unrelated")];
        let declaring = HashMap::from([("Child".to_string(), "Parent".to_string())]);

        let ordered = declaration_order(rows, &declaring)
            .into_iter()
            .map(|(_, _, spec)| spec.inner)
            .collect::<Vec<_>>();

        assert_eq!(ordered, ["Unrelated", "Parent", "Child"]);
    }

    #[test]
    fn an_unlisted_intermediate_owner_still_orders_its_listed_ancestor_first() {
        let rows = vec![row("a", "Child"), row("z", "Root")];
        let declaring = HashMap::from([
            ("Child".to_string(), "Intermediate".to_string()),
            ("Intermediate".to_string(), "Root".to_string()),
        ]);

        let ordered = declaration_order(rows, &declaring)
            .into_iter()
            .map(|(_, _, spec)| spec.inner)
            .collect::<Vec<_>>();

        assert_eq!(ordered, ["Root", "Child"]);
    }

    fn nested(inner: &str, outer: &str, name: &str) -> InnerClassSpec {
        InnerClassSpec {
            inner: inner.to_string(),
            outer: Some(outer.to_string()),
            name: Some(name.to_string()),
            access: 0x0019,
        }
    }

    /// What one file shares between its writers, as `InnerClasses::register` hands it over.
    #[derive(Default)]
    struct FileNest {
        paths: super::DeclarationPaths,
        declaring: Rc<HashMap<String, String>>,
        orders: Rc<TableOrders>,
    }

    fn table_order(nest: &[InnerClassSpec], file: Option<&FileNest>) -> Vec<String> {
        let mut writer = ClassWriter::new("app/Outer", "java/lang/Object");
        writer.add_inner_classes(nest);
        if let Some(file) = file {
            writer.set_declaration_paths(file.paths.clone());
            writer.set_declaring_classes(file.declaring.clone());
            writer.set_table_orders(file.orders.clone());
        }
        writer.resolve_inner_classes();
        // A writer orders its table again when it is written.
        writer.resolve_inner_classes();
        writer
            .inner_class_candidates
            .iter()
            .map(|spec| spec.inner.clone())
            .collect()
    }

    #[test]
    fn writers_of_one_file_share_the_table_order_they_compute() {
        let nest = [
            nested("app/Outer$Zeta", "app/Outer", "Zeta"),
            nested("app/Outer$Alpha", "app/Outer", "Alpha"),
            nested("app/Outer$Alpha$Inner", "app/Outer$Alpha", "Inner"),
        ];
        let alone = table_order(&nest, None);
        assert_eq!(
            alone,
            ["app/Outer$Alpha", "app/Outer$Alpha$Inner", "app/Outer$Zeta"]
        );
        let file = FileNest::default();
        assert_eq!(table_order(&nest, Some(&file)), alone);
        assert_eq!(file.orders.known.borrow().len(), 2);
        // The next writer of the file reuses both orders and keeps no new one.
        assert_eq!(table_order(&nest, Some(&file)), alone);
        assert_eq!(file.orders.known.borrow().len(), 2);
        // A writer of another file, with paths of its own, does not take this file's order.
        let other = FileNest {
            orders: file.orders.clone(),
            ..FileNest::default()
        };
        assert_eq!(table_order(&nest, Some(&other)), alone);
        assert_eq!(file.orders.known.borrow().len(), 4);
    }

    #[test]
    fn a_table_whose_qualified_names_tie_is_not_shared() {
        // `app/B$C` nested in `app/B` and a top-level `app/B.C` row both read `app.B.C`.
        let nest = [
            nested("app/B$C", "app/B", "C"),
            InnerClassSpec {
                inner: "app/B.C".to_string(),
                outer: None,
                name: None,
                access: 0x0019,
            },
        ];
        let file = FileNest::default();
        assert_eq!(table_order(&nest, Some(&file)), table_order(&nest, None));
        assert!(file.orders.known.borrow().is_empty());
    }
}
