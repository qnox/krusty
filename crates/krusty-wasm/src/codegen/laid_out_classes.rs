//! The classes earlier files of one source module laid out, as a later file extending one reads
//! them.
//!
//! A class's struct repeats its superclass's fields and its table type its superclass's slot
//! types; its table holds the superclass's functions wherever it overrides nothing; constructing it
//! runs the superclass's constructor. Only the file declaring the superclass has its IR, so when
//! that file lays the class out it records all of this here, by the class's identity, and a later
//! file of the same source module extends the class from the record
//! (`krusty::backend::class_tables::LaidOutSuperclass`). A record does not outlive its source
//! module: the module plan numbering the table is that module's, and so are the callable
//! identities its implementations were chosen by. A dependency class a support unit materializes
//! is the exception: the plan numbers it from its provider's declarations alike in every module,
//! by provider identities, so its record serves the rest of the program.

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::LaidOutSuperclass;
use krusty::fir::CallableId;
use krusty::types::{Ty, TypeName};

use super::super::encode::FieldType;
use super::super::interface_dispatch::MemberRef;
use super::classes::OwnedBase;
use super::interface_members::Signature;

/// A function and the signature it is carried as.
pub(super) type Placed = (u32, Signature);

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
        declared: (Vec<Ty>, Ty),
        implemented: (Vec<Ty>, Ty),
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
    /// A dependency class is materialized once for the program, by provider identities, so its
    /// record outlives the source module whose file selected its support unit.
    dependencies: HashMap<TypeName, LaidOutClass>,
    /// Whether the file being laid out is a dependency support unit.
    support: bool,
}

impl LaidOutClasses {
    /// Begin (or continue) one source module, forgetting another module's classes.
    pub(super) fn enter_source_module(&mut self, module_name: &str) {
        self.support = false;
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.classes.clear();
        }
    }

    /// Begin a dependency support unit, whose classes every later file of the program extends.
    pub(super) fn enter_dependency_support(&mut self) {
        self.support = true;
    }

    pub(super) fn get(&self, class: TypeName) -> Option<&LaidOutClass> {
        self.classes
            .get(&class)
            .or_else(|| self.dependencies.get(&class))
    }

    /// Record `class` as a file laid it out. A class an inline function's body declares is laid
    /// out again by every file its body is inlined into, each copy a type of its own, so no single
    /// record stands for it and a subclass of it declines.
    pub(super) fn insert(&mut self, class: TypeName, laid_out: LaidOutClass) {
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
}
