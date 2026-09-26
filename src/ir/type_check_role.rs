//! The semantic role of a type-operation target that a plain instance test or cast cannot decide.
//!
//! A mutable Kotlin collection shares its platform interface with its read-only face, and a
//! function type erases to a class every lambda of any arity may implement. Both roles are
//! declaration facts: a provider publishes a classifier's [`ClassifierRole`] on its record, this IR
//! carries it for every classifier the file references, and a `Ty::Fun` signature carries its own
//! arity. A backend maps a role to its own runtime checks and never recovers one from a name.

use super::referenced_classifiers::{collect_classifier_names, referenced_classifier_names};
use super::{Callee, IrExpr, IrFile, IrIntrinsic};
use crate::types::{ClassifierFactSource, ClassifierRole, CollectionKind, MappedCollection};
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeCheckRole {
    /// The mutable face of a mapped collection classifier.
    MutableCollection(CollectionKind),
    /// A non-suspend function type of this arity (receiver and context parameters included).
    FunctionOfArity(u8),
}

impl IrFile {
    /// Record the checked role of every classifier this file references, from the one normalized
    /// classifier-fact boundary. Runs once the file's types are final.
    pub fn publish_classifier_roles(&mut self, classifiers: &dyn ClassifierFactSource) {
        let mut referenced = referenced_classifier_names(self);
        for substitutions in self.reified_call_subst.values() {
            for (_, ty) in substitutions {
                collect_classifier_names(*ty, &mut referenced);
            }
        }
        for expression in &self.exprs {
            if let IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: IrIntrinsic::TypeOf { ty },
                        ..
                    },
                ..
            } = expression
            {
                collect_classifier_names(*ty, &mut referenced);
            }
        }
        for classifier in referenced {
            if self.classifier_roles.contains_key(&classifier) {
                continue;
            }
            if let Some(role) = classifiers.classifier_role(classifier) {
                self.classifier_roles.insert(classifier, role);
            }
        }
    }

    /// The published role of a referenced classifier.
    pub fn classifier_role(&self, classifier: TypeName) -> Option<ClassifierRole> {
        self.classifier_roles.get(&classifier).copied()
    }

    /// The mapped collection face a referenced classifier is, if any.
    pub fn mapped_collection(&self, classifier: TypeName) -> Option<MappedCollection> {
        match self.classifier_role(classifier)? {
            ClassifierRole::MappedCollection(collection) => Some(collection),
            ClassifierRole::FunctionOfArity(_) => None,
        }
    }

    /// The role of the target `ty` of an `is` or `as` (nullability aside).
    pub fn type_check_role(&self, ty: Ty) -> Option<TypeCheckRole> {
        match ty.non_null() {
            Ty::Obj(name, _) => match self.classifier_role(name)? {
                ClassifierRole::MappedCollection(collection) => collection
                    .mutable
                    .then_some(TypeCheckRole::MutableCollection(collection.kind)),
                ClassifierRole::FunctionOfArity(arity) => {
                    Some(TypeCheckRole::FunctionOfArity(arity))
                }
            },
            Ty::Fun(signature) if !signature.suspend => u8::try_from(signature.params.len())
                .ok()
                .map(TypeCheckRole::FunctionOfArity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrConst, IrTypeOp};
    use crate::types::{intern_fnsig, type_name, FnSig};

    /// Publishes roles only for repository-owned classifiers, so any role the IR reports for a
    /// Kotlin-looking name would have come from reading that name.
    struct PublishedRoles;

    impl ClassifierFactSource for PublishedRoles {
        fn classifier_annotations(
            &self,
            _classifier: TypeName,
        ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
            None
        }

        fn classifier_role(&self, classifier: TypeName) -> Option<ClassifierRole> {
            if classifier == type_name("test/roles/Editable") {
                Some(ClassifierRole::MappedCollection(MappedCollection {
                    kind: CollectionKind::List,
                    mutable: true,
                }))
            } else if classifier == type_name("test/roles/Readable") {
                Some(ClassifierRole::MappedCollection(MappedCollection {
                    kind: CollectionKind::List,
                    mutable: false,
                }))
            } else if classifier == type_name("test/roles/Binary") {
                Some(ClassifierRole::FunctionOfArity(2))
            } else {
                None
            }
        }
    }

    fn test_of(ir: &mut IrFile, classifier: &str) -> Ty {
        let target = Ty::Obj(type_name(classifier), &[]);
        let arg = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg,
            type_operand: target,
        });
        target
    }

    #[test]
    fn published_roles_reach_type_operations() {
        let mut ir = IrFile::default();
        let editable = test_of(&mut ir, "test/roles/Editable");
        let readable = test_of(&mut ir, "test/roles/Readable");
        let binary = test_of(&mut ir, "test/roles/Binary");
        let mutable_list = test_of(&mut ir, "kotlin/collections/MutableList");
        let function = test_of(&mut ir, "kotlin/Function2");
        ir.publish_classifier_roles(&PublishedRoles);

        let roles = [editable, readable, binary, mutable_list, function]
            .map(|target| ir.type_check_role(Ty::nullable(target)));
        assert_eq!(
            roles,
            [
                Some(TypeCheckRole::MutableCollection(CollectionKind::List)),
                None,
                Some(TypeCheckRole::FunctionOfArity(2)),
                None,
                None,
            ]
        );
        assert_eq!(
            ir.mapped_collection(type_name("test/roles/Readable")),
            Some(MappedCollection {
                kind: CollectionKind::List,
                mutable: false,
            })
        );
    }

    /// A reified call's type argument is tested inside the specialized body, so its classifier's
    /// role is carried too.
    #[test]
    fn reified_type_arguments_carry_their_roles() {
        let mut ir = IrFile::default();
        let editable = Ty::Obj(type_name("test/roles/Editable"), &[]);
        ir.reified_call_subst
            .insert(0, vec![("T".to_string(), editable)]);
        ir.publish_classifier_roles(&PublishedRoles);
        assert_eq!(
            ir.type_check_role(editable),
            Some(TypeCheckRole::MutableCollection(CollectionKind::List))
        );
    }

    #[test]
    fn a_function_type_takes_its_signature_arity() {
        let ir = IrFile::default();
        let function = |suspend| {
            Ty::Fun(intern_fnsig(FnSig {
                params: vec![Ty::Int, Ty::String],
                ret: Ty::Int,
                context_count: 0,
                has_receiver: true,
                suspend,
            }))
        };
        assert_eq!(
            ir.type_check_role(function(false)),
            Some(TypeCheckRole::FunctionOfArity(2))
        );
        assert_eq!(ir.type_check_role(function(true)), None);
    }
}
