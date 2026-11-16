//! The applied hierarchies of the classes a lowering materializes. A target answers a type test
//! and lays out a class's interface tables from the hierarchy, as it does for a source class.

use crate::fx_hash::HashMap;
use crate::ir::{IrAppliedClassifier, IrClass, IrFile};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind};

use super::decline::KlibBodyDeclineReason;

/// Publish the applied hierarchy of each class lowering declared from `first` on. A class of the
/// file is above its written supertypes, at the application the walk reaches it with; any other
/// classifier is above the supertypes `declarations` gives it.
///
/// A value class records no supertypes on its class, so it publishes no hierarchy.
pub(super) fn publish(
    ir: &mut IrFile,
    first: usize,
    declarations: &dyn SymbolSource,
) -> Result<(), KlibBodyDeclineReason> {
    for class in first..ir.classes.len() {
        let name = ir.classes[class].fq_name_id();
        if ir.classes[class].is_value || ir.classifier_hierarchies.contains_key(&name) {
            continue;
        }
        let root = match ir.declared_class_self_type(name) {
            Some(root) => root,
            None if ir.classes[class].type_params.is_empty() => Ty::obj_name(name),
            None => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a generic materialized class without its self application".to_owned(),
                ))
            }
        };
        let mut malformed = false;
        let hierarchy =
            crate::symbol_resolver::applied_hierarchy_through(root, |owner, applied| {
                match ir.class_id_by_name(owner) {
                    Some(declared) => declared_parents(&ir.classes[declared as usize], applied)
                        .unwrap_or_else(|| {
                            malformed = true;
                            Vec::new()
                        }),
                    None => crate::symbol_resolver::direct_supertypes(declarations, applied),
                }
            });
        if malformed {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a materialized class applied with arguments its parameters do not take".to_owned(),
            ));
        }
        let hierarchy = hierarchy
            .into_iter()
            .map(|(classifier, applied, depth)| IrAppliedClassifier {
                classifier,
                applied,
                depth: u32::try_from(depth).expect("a hierarchy depth fits u32"),
            })
            .collect();
        ir.classifier_hierarchies.insert(name, hierarchy);
    }
    Ok(())
}

/// The supertypes `class` declares, at the arguments `applied` gives its type parameters.
fn declared_parents(class: &IrClass, applied: Ty) -> Option<Vec<Ty>> {
    let TyKind::Obj(_, arguments) = applied.non_null().kind() else {
        return None;
    };
    if arguments.len() != class.type_params.len() {
        return None;
    }
    let bindings = class
        .type_params
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .collect::<HashMap<_, _>>();
    Some(
        class
            .supertypes
            .iter()
            .map(|declared| crate::types::ty_subst_applied_arguments(*declared, &bindings))
            .collect(),
    )
}
