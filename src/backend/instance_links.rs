//! Links from one file to an instance of a classifier another file realizes: an object's single
//! instance, one constant of an enum, or a new instance built by one of its constructors; and to
//! an enum's `values()`, `valueOf` and `entries`, which answer its constants.
//!
//! Only the declaring file holds such an instance's layout, storage and initialization, so it
//! realizes an entry, an ordinary function answering the instance: an object's and a
//! constructor's in `super::construction_entries`, an enum constant's, `values()`, `valueOf` and
//! `entries` in `super::enums`. Every other file reads the instance by calling that entry. These
//! are the same problem, so they share one identity ([`InstanceEntry`]), one Pass-1 fact
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
    /// The `values()`, `valueOf` or `entries` of an enum, which read the constants the declaring
    /// file holds; the entry takes the member's own parameters.
    EnumMember {
        classifier: TypeName,
        member: super::enums::StaticMember,
    },
    /// A new instance, built by the checked constructor declaration of the named source module;
    /// the entry takes the constructor's declared parameters. A declaration identity belongs to
    /// one module's analysis, so the module qualifies it.
    Constructor {
        module: Box<str>,
        declaration: DeclarationId,
    },
    /// A new instance, built by a construction that leaves arguments of the checked constructor
    /// declaration to their defaults; the entry takes an inner class's enclosing instance, the
    /// constructor's whole parameter frame and the omission mask words (see
    /// `super::default_arguments`). The module qualifies the declaration as for
    /// [`Self::Constructor`].
    ConstructorDefaults {
        module: Box<str>,
        declaration: DeclarationId,
    },
    /// Whether a reference is an instance of the named class another file of the module
    /// declares: only the declaring file knows the class's object layout, so it answers with a
    /// function of one reference that returns the test's result.
    TypeTest(TypeName),
}

impl InstanceEntry {
    /// The enum constant this entry reads, or `None` for any other kind of instance.
    pub fn enum_constant(&self) -> Option<&EnumConstant> {
        match self {
            Self::EnumConstant(constant) => Some(constant),
            Self::Object(_)
            | Self::EnumMember { .. }
            | Self::Constructor { .. }
            | Self::ConstructorDefaults { .. }
            | Self::TypeTest(_) => None,
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

/// The link of each instance of another file's classifier that `ir` emits a read of, and of each
/// `values()`, `valueOf` and `entries` of another file's enum it uses: the declaring
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
            node => match super::enums::StaticMember::of(node) {
                Some((classifier, member)) => InstanceEntry::EnumMember { classifier, member },
                None => continue,
            },
        };
        let declared_here = match &instance {
            InstanceEntry::Object(classifier) | InstanceEntry::EnumMember { classifier, .. } => {
                ir.class_id_by_name(*classifier).is_some()
            }
            InstanceEntry::EnumConstant(constant) => {
                ir.class_id_by_name(constant.classifier).is_some()
            }
            InstanceEntry::Constructor { .. }
            | InstanceEntry::ConstructorDefaults { .. }
            | InstanceEntry::TypeTest(_) => continue,
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
/// reserved or found like [`link_reads`] does for the instances it reads: a construction the
/// checker selected a module constructor for whose class `ir` does not declare, and a call of a
/// constructor's default-argument entry. One whose constructor `module` does not certify has no
/// link, and the target declines it.
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
        let instance = match ir.expr(expression) {
            IrExpr::New { internal, .. } => {
                let Some(&constructor) = ir.module_constructions.selected.get(&expression) else {
                    continue;
                };
                if ir.class_id_by_name(*internal).is_some() {
                    continue;
                }
                InstanceEntry::Constructor {
                    module: module_name.into(),
                    declaration: constructor,
                }
            }
            IrExpr::Call { callee, .. } => {
                let crate::ir::Callee::ModuleConstructorDefaultEntry { constructor, .. } =
                    &**callee
                else {
                    continue;
                };
                InstanceEntry::ConstructorDefaults {
                    module: module_name.into(),
                    declaration: *constructor,
                }
            }
            _ => continue,
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

/// The link of each type test against another file's class that `ir` emits: an `is` or `!is`
/// whose class `ir` does not declare and `module` certifies a file declares. One `module` does
/// not certify has no link, and the target declines it.
pub fn link_type_test_reads<V: Clone>(
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
        let IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::InstanceOf | crate::ir::IrTypeOp::NotInstanceOf,
            type_operand,
            ..
        } = ir.expr(expression)
        else {
            continue;
        };
        let Some(classifier) = type_operand.non_null().obj_internal() else {
            continue;
        };
        if ir.class_id_by_name(classifier).is_some() {
            continue;
        }
        let instance = InstanceEntry::TypeTest(classifier);
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
