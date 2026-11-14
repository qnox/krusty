//! The classes earlier files of one source module laid out, as a later file extending one reads
//! them.
//!
//! A class's struct repeats its superclass's fields and its table type its superclass's slot
//! types; its table holds the superclass's functions wherever it overrides nothing; constructing it
//! runs the superclass's constructor. Same-source-module records retain only that module's live
//! declaration identities. Source classes exported to another module publish a second form under
//! their exact constructor KLIB identities; it drops source-local CallableIds, types, and interface
//! numbers, then is activated only by a matching current-provider constructor contract. Dependency
//! support classes have their own program-wide records, keyed by the selected provider identity.

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::LaidOutSuperclass;
use krusty::backend::CheckedBackendCallables;
use krusty::fir::CallableId;
use krusty::ir::{ClassId, CtorDelegateTarget, IrFile};
use krusty::libraries::{ExternalCallableKind, KlibDependencyCallable};
use krusty::types::SemanticCallableOwner;
use krusty::types::{Ty, TypeName};

use super::super::boxes::BoxAdapter;
use super::super::encode::FieldType;
use super::super::interface_dispatch::MemberRef;
use super::classes::OwnedBase;
use super::interface_members::Signature;

/// A function and the signature it is carried as.
pub(super) type Placed = (u32, Signature);

/// Program-owned operations that adapt one physical signature to another, without provider types.
#[derive(Clone, Debug)]
pub(super) struct SignatureAdapters {
    /// Indexed by physical parameter, including an empty receiver entry at index zero.
    pub(super) parameters: Vec<Option<BoxAdapter>>,
    pub(super) result: Option<ResultAdapter>,
}

/// Result adaptation selected from the checked logical result while its type is live.
#[derive(Clone, Copy, Debug)]
pub(super) enum ResultAdapter {
    Box(BoxAdapter),
    /// The existing runtime singleton, for a Unit body with no physical result.
    Unit(u32),
}

impl SignatureAdapters {
    pub(super) fn none(parameter_count: usize) -> Self {
        Self {
            parameters: vec![None; parameter_count],
            result: None,
        }
    }
}

/// One class an earlier file of the source module laid out.
#[derive(Clone, Debug)]
pub(super) struct LaidOutClass {
    pub(super) structure: u32,
    pub(super) table_type: u32,
    /// Its struct's fields after the dispatch table, which a subclass's struct begins with.
    pub(super) fields: Vec<FieldType>,
    /// The base no file declares that its struct extends, whose fields lead `fields`.
    pub(super) base: OwnedBase,
    /// The function type of each slot of its table; `None` for an untyped one.
    pub(super) slot_types: Vec<Option<u32>>,
    /// The function each slot of its table holds; `None` for an untyped one.
    pub(super) entries: Vec<Option<u32>>,
    /// Its struct, then every struct it is declared a subtype of, nearest first.
    pub(super) ancestors: Vec<u32>,
    /// Each constructor by ordinal, the primary first: its function and the parameters it takes.
    pub(super) constructors: Vec<Option<(u32, Vec<Ty>)>>,
    /// Constructor entries by ordinal, retained without provider-local types across modules.
    pub(super) constructor_entries: Vec<Option<u32>>,
    /// Exact KLIB identities published for each constructor ordinal, when one is visible.
    pub(super) constructor_identities: Vec<Option<KlibDependencyCallable>>,
    /// Every method of it and its superclasses that has a body, by identity: its function and that
    /// function's type, which a `super` call of a subclass calls directly.
    pub(super) methods: HashMap<CallableId, (u32, u32)>,
    /// The getters of the companions constructing an instance initializes first, root first.
    pub(super) activation: Vec<u32>,
    /// What the shared class tables read of it.
    pub(super) table: LaidOutSuperclass,
    /// The function its interface table holds at each number that no class slot of it fills.
    pub(super) interface_bodies: HashMap<u32, Placed>,
    /// How it implements each member of a dependency's interface, most derived first.
    pub(super) dependency_implementations: Vec<(MemberRef, DependencyImplementation)>,
    /// Checked callable membership, retained even when no invocation adapter could be emitted.
    pub(super) function_arities: Vec<usize>,
    pub(super) suspend_function_arities: Vec<usize>,
    /// Why a subclass cannot extend it, where it has no instances of its own and so its file
    /// needed none of what a subclass would inherit.
    pub(super) refusal: Option<String>,
}

/// How a class implements a member of a dependency's interface, as a subclass inherits it.
#[derive(Clone, Debug)]
pub(super) enum DependencyImplementation {
    /// The member at this class slot, carried from `implemented` to `declared` — what a subclass
    /// overriding the member at that slot implements it with in turn.
    Slot {
        slot: u32,
        declared: Signature,
        implemented: Signature,
        adapters: SignatureAdapters,
    },
    /// A function no class slot holds: a default.
    Function(u32),
}

/// The classes laid out so far in the current source module, and the dependency classes support
/// units materialized for the whole program, by identity.
#[derive(Default)]
pub(super) struct LaidOutClasses {
    source_module: Option<String>,
    classes: HashMap<TypeName, LaidOutClass>,
    /// Source classes only cross a module boundary under the exact constructor identity their
    /// producer exported. Their live CallableIds and type handles are stripped before storage.
    published_source: HashMap<KlibDependencyCallable, (TypeName, LaidOutClass)>,
    /// Published source layouts authenticated by this file's exact selected super constructors.
    active_source: HashMap<TypeName, LaidOutClass>,
    /// A dependency class is materialized once for the program, by provider identities, so its
    /// record outlives the source module whose file selected its support unit.
    dependencies: HashMap<TypeName, LaidOutClass>,
    /// Whether the file being laid out is a dependency support unit.
    support: bool,
}

impl LaidOutClasses {
    /// Begin (or continue) one source module, forgetting another module's classes.
    pub(super) fn enter_source_module(
        &mut self,
        module_name: &str,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
    ) {
        self.support = false;
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.classes.clear();
        }
        self.active_source.clear();

        for (class_id, selected_ordinal, target) in super_constructor_edges(ir) {
            let selected_ordinal = selected_ordinal as usize;
            let parent = ir.classes[class_id as usize].superclass;
            let Some(fact) = callables.callable(target) else {
                continue;
            };
            let Some(identity) = fact.dependency_callable.as_ref() else {
                continue;
            };
            if fact.kind != ExternalCallableKind::Constructor
                || fact.declaration_owner != Some(SemanticCallableOwner::Classifier(parent))
                || !fact
                    .klib_body_class_contract
                    .as_ref()
                    .is_some_and(|contract| contract.matches_owner(&identity.module, parent))
            {
                continue;
            }
            let Some((known_class, published)) = self.published_source.get(identity) else {
                continue;
            };
            if *known_class != parent {
                continue;
            }
            let Some(parent_ordinal) = published
                .constructor_identities
                .iter()
                .position(|candidate| candidate.as_ref() == Some(identity))
            else {
                continue;
            };
            let Some(function) = published
                .constructor_entries
                .get(parent_ordinal)
                .copied()
                .flatten()
            else {
                continue;
            };
            let current = self.active_source.entry(parent).or_insert_with(|| {
                let mut current = published.clone();
                current.methods.clear();
                current.constructors.fill(None);
                current
            });
            if !current
                .constructor_identities
                .iter()
                .any(|candidate| candidate.as_ref() == Some(identity))
                || current.structure != published.structure
                || current.table_type != published.table_type
            {
                current.refusal = Some("source classes with conflicting KLIB producers".into());
                continue;
            }
            if current.constructors.len() <= selected_ordinal {
                current.constructors.resize(selected_ordinal + 1, None);
            }
            let Some(slot) = current.constructors.get_mut(selected_ordinal) else {
                continue;
            };
            *slot = Some((function, fact.params.clone()));
        }
    }

    /// Begin a dependency support unit, whose classes every later file of the program extends.
    pub(super) fn enter_dependency_support(&mut self) {
        self.support = true;
        self.active_source.clear();
    }

    pub(super) fn get(&self, class: TypeName) -> Option<&LaidOutClass> {
        if self.support {
            return self.dependencies.get(&class);
        }
        self.classes
            .get(&class)
            .or_else(|| self.active_source.get(&class))
            .or_else(|| self.dependencies.get(&class))
    }

    /// Record `class` as a file laid it out. A class an inline function's body declares is laid
    /// out again by every file its body is inlined into, each copy a type of its own, so no single
    /// record stands for it and a subclass of it declines.
    pub(super) fn insert(&mut self, class: TypeName, laid_out: LaidOutClass) {
        if !self.support {
            self.publish_source(class, laid_out.clone());
        }
        let records = if self.support {
            &mut self.dependencies
        } else {
            &mut self.classes
        };
        match records.get_mut(&class) {
            Some(known) => {
                known.refusal = Some("a class more than one file lays out".to_string());
            }
            None => {
                records.insert(class, laid_out);
            }
        }
    }

    /// Record the exported form under its exact provider identities, never by a rendered class
    /// name alone. CallableIds and Ty values are local to the producing analysis.
    fn publish_source(&mut self, class: TypeName, laid_out: LaidOutClass) {
        for identity in laid_out.constructor_identities.iter().flatten() {
            let mut portable = laid_out.clone();
            portable.methods.clear();
            portable.constructors.fill(None);
            portable.interface_bodies.clear();
            portable.table.interface_entries.clear();
            match self.published_source.get_mut(identity) {
                Some((_, known)) => {
                    known.refusal = Some("a source class identity with conflicting layouts".into());
                }
                None => {
                    self.published_source
                        .insert(identity.clone(), (class, portable));
                }
            }
        }
    }
}

/// The actual checked super-constructor edges, with their current-provider parent ordinals.
fn super_constructor_edges(ir: &IrFile) -> Vec<(ClassId, u32, krusty::fir::ExternalCallableId)> {
    let mut selected = Vec::new();
    selected.extend(
        ir.external_super_constructors
            .iter()
            .filter_map(|(&child, target)| {
                let class = ir.class_id_by_name(child)?;
                Some((
                    class,
                    ir.classes[class as usize].super_ctor.ordinal,
                    target.declaration,
                ))
            }),
    );
    selected.extend(ir.external_secondary_super_constructors.iter().filter_map(
        |(&(child, child_ordinal), target)| {
            let class = ir.class_id_by_name(child)?;
            let secondary = ir.classes[class as usize]
                .secondary_ctors
                .get(child_ordinal as usize)?;
            let CtorDelegateTarget::Super {
                target: selected, ..
            } = &secondary.delegate
            else {
                return None;
            };
            Some((class, selected.ordinal, target.declaration))
        },
    ));
    selected.sort_unstable_by_key(|(class, ordinal, _)| (*class, *ordinal));
    selected
}
