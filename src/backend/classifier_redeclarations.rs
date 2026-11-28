//! A module's classifier that redeclares one of its dependency chain, and the earlier code that
//! already used the classifier it replaces.
//!
//! Classpath substitution lets a platform module's class replace the common module's class of the
//! same identity: kotlinc links every module, the common one included, to the one declaration.
//! A target that lowers a program one file at a time has lowered the earlier modules' files by
//! the time the redeclaration arrives, each against the classifier it declared or read then, and
//! later modules read the redeclaration (see `super::module_links`). The two agree only while no
//! earlier code builds, reads, or tests an instance of the replaced classifier. This ledger
//! records which module's code first did, so the redeclaring file is declined rather than linked
//! into a program in which that code would keep the replaced class.

use crate::fx_hash::HashMap;
use crate::ir::{EmittedDeclaration, IrExpr, IrFile};
use crate::types::TypeName;

use super::BackendClassifierSource;

/// The module whose code first used each classifier, over every file lowered so far.
#[derive(Debug, Default)]
pub struct ClassifierUses {
    first_user: HashMap<TypeName, Box<str>>,
}

/// A file's classifier redeclares one an earlier module's code already used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsedBeforeRedeclaration {
    pub classifier: TypeName,
    /// The module whose code used the classifier the redeclaration replaces.
    pub user: Box<str>,
}

impl UsedBeforeRedeclaration {
    /// The declined construct, phrased for a target's capability diagnostic.
    pub fn construct(&self) -> String {
        // Boundary conversion for a diagnostic.
        format!(
            "redeclaring the class `{}` that code compiled into the module `{}` already uses",
            self.classifier.render().replace('/', "."),
            self.user
        )
    }
}

impl ClassifierUses {
    /// Check that no classifier `ir` declares redeclares one another module's code already used,
    /// then record the classifiers `ir`'s own code uses. Call it for each file, in lowering order,
    /// before a target adds entries of its own to `ir`.
    pub fn enter_file(
        &mut self,
        module: &str,
        ir: &IrFile,
        classifiers: &dyn BackendClassifierSource,
    ) -> Result<(), UsedBeforeRedeclaration> {
        for class in &ir.classes {
            if class.is_anonymous_object || class.is_local_class {
                continue;
            }
            if classifiers
                .redeclared_dependency_module(class.fq_name)
                .is_none()
            {
                continue;
            }
            match self.first_user.get(&class.fq_name) {
                Some(user) if **user != *module => {
                    return Err(UsedBeforeRedeclaration {
                        classifier: class.fq_name,
                        user: user.clone(),
                    });
                }
                _ => {}
            }
        }
        for root in crate::ir::emitted_declaration_roots(ir) {
            // A classifier's own constructors and members run only on its instances, which only
            // another use creates or reaches.
            let owner = match root.declaration {
                EmittedDeclaration::Function(function) => {
                    ir.functions[function as usize].dispatch_receiver
                }
                EmittedDeclaration::Static(_) => None,
                EmittedDeclaration::PrimaryConstructor(class)
                | EmittedDeclaration::SecondaryConstructor { class, .. } => {
                    Some(ir.classes[class as usize].fq_name)
                }
            };
            for expression in crate::ir::value_namespace_expressions(ir, root.root) {
                let Some(classifier) = used_classifier(ir.expr(expression)) else {
                    continue;
                };
                if Some(classifier) != owner {
                    self.first_user
                        .entry(classifier)
                        .or_insert_with(|| module.into());
                }
            }
        }
        Ok(())
    }
}

/// The classifier an expression builds, reads, or tests an instance of.
fn used_classifier(expression: &IrExpr) -> Option<TypeName> {
    match expression {
        IrExpr::New { internal, .. } => Some(*internal),
        IrExpr::SingletonValue { classifier } | IrExpr::EnumEntry { classifier, .. } => {
            Some(*classifier)
        }
        IrExpr::TypeOp { type_operand, .. } => type_operand.non_null().obj_internal(),
        node => super::enums::StaticMember::of(node).map(|(classifier, _)| classifier),
    }
}
