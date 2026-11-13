//! Links from one file to an instance of a classifier another file realizes: an object's single
//! instance, one constant of an enum, or a new instance built by one of its constructors.
//!
//! Only the declaring file holds such an instance's layout, storage and initialization, so it
//! realizes an entry, an ordinary function answering the instance: an object's and a
//! constructor's in `super::construction_entries`, an enum constant's in `super::enums`. Every
//! other file reads the instance by calling that entry. These are the same problem, so they
//! share one identity ([`InstanceEntry`]), one Pass-1 fact
//! (`super::BackendModuleFacts::declares_instance`) and one table of links
//! ([`super::module_links::ModuleLinks`]): whichever of the reading and the declaring file is
//! lowered first allocates the link, and the other finds it. The table serves every module of a
//! program, so a library's instance links the same way.

use crate::fir::DeclarationId;
use crate::fx_hash::HashMap;
use crate::ir::{FunId, IrExpr, IrFile};
use crate::types::TypeName;

use super::module_links::ModuleLinks;
use super::{BackendModuleFacts, DependencyExportSignatures, EnumConstant};

/// An instance another file reads through its declaring file's entry.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InstanceEntry {
    /// The single instance of an `object` or companion object.
    Object(TypeName),
    EnumConstant(EnumConstant),
    /// A new instance, built by the checked constructor declaration of the named source module;
    /// the entry takes the constructor's declared parameters. A declaration identity belongs to
    /// one module's analysis, so the module qualifies it.
    Constructor {
        module: Box<str>,
        declaration: DeclarationId,
    },
}

impl InstanceEntry {
    /// The enum constant this entry reads, or `None` for any other kind of instance.
    pub fn enum_constant(&self) -> Option<&EnumConstant> {
        match self {
            Self::EnumConstant(constant) => Some(constant),
            Self::Object(_) | Self::Constructor { .. } => None,
        }
    }
}

/// Define the link of every instance entry this file realized, each by its instance: the link a
/// read in an earlier file reserved, or a fresh one from `allocate`.
pub fn link_entries<V: Clone>(
    exports: &DependencyExportSignatures,
    links: &mut ModuleLinks<InstanceEntry, V>,
    mut allocate: impl FnMut() -> V,
) -> Result<Vec<(FunId, V)>, &'static str> {
    exports
        .instance_entries()
        .iter()
        .map(|(instance, entry)| {
            links
                .define(instance.clone(), &mut allocate)
                .map(|link| (*entry, link))
                .map_err(|_| "two files realize one instance entry")
        })
        .collect()
}

/// The link of each instance of another file's classifier that `ir` emits a read of: the declaring
/// file's defined link, or, for an instance `module` certifies but whose file comes later, a link
/// reserved from `allocate` for that file to define. An instance of neither kind has no link, and
/// the target declines its read.
pub fn link_reads<V: Clone>(
    ir: &IrFile,
    module: &BackendModuleFacts,
    links: &mut ModuleLinks<InstanceEntry, V>,
    mut allocate: impl FnMut() -> V,
) -> HashMap<InstanceEntry, V> {
    let mut reads = HashMap::default();
    for expression in crate::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
    {
        let instance = match ir.expr(expression) {
            IrExpr::EnumEntry { classifier, name } => InstanceEntry::EnumConstant(EnumConstant {
                classifier: *classifier,
                name: name.clone(),
            }),
            IrExpr::SingletonValue { classifier } => InstanceEntry::Object(*classifier),
            _ => continue,
        };
        let declared_here = match &instance {
            InstanceEntry::Object(classifier) => ir.class_id_by_name(*classifier).is_some(),
            InstanceEntry::EnumConstant(constant) => {
                ir.class_id_by_name(constant.classifier).is_some()
            }
            InstanceEntry::Constructor { .. } => continue,
        };
        if declared_here || reads.contains_key(&instance) {
            continue;
        }
        let declared = module.declares_instance(&instance);
        if let Some(link) = links.read(instance.clone(), declared, &mut allocate) {
            reads.insert(instance, link);
        }
    }
    reads
}

/// The link of each constructor of another file's class that `ir` builds an instance through,
/// reserved or found like [`link_reads`] does for the instances it reads. A construction the
/// checker selected a module constructor for whose class `ir` does not declare, and that `module`
/// does not certify, has no link, and the target declines it.
///
/// Native still builds a cross-file construction through its own module ABI
/// (`module_abi/constructors.rs`), which also covers inner, value and generic classes whose
/// entries `construction_entries` does not build yet, and does not call this.
pub fn link_constructor_reads<V: Clone>(
    ir: &IrFile,
    module_name: &str,
    module: &BackendModuleFacts,
    links: &mut ModuleLinks<InstanceEntry, V>,
    mut allocate: impl FnMut() -> V,
) -> HashMap<InstanceEntry, V> {
    let mut reads = HashMap::default();
    for expression in crate::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
    {
        let IrExpr::New { internal, .. } = ir.expr(expression) else {
            continue;
        };
        let Some(&constructor) = ir.module_constructions.selected.get(&expression) else {
            continue;
        };
        if ir.class_id_by_name(*internal).is_some() {
            continue;
        }
        let instance = InstanceEntry::Constructor {
            module: module_name.into(),
            declaration: constructor,
        };
        if reads.contains_key(&instance) {
            continue;
        }
        let declared = module.declares_instance(&instance);
        if let Some(link) = links.read(instance.clone(), declared, &mut allocate) {
            reads.insert(instance, link);
        }
    }
    reads
}
