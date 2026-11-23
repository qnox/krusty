//! The applied supertype hierarchy of the classes a file declares.

use super::IrFile;
use crate::assignable::{applied_supertype, TypeOracle};
use crate::types::{ty_subst_supertype_template, Ty};

/// The applied hierarchy of the classes one file declares: a declared class's direct supertypes
/// with its own type arguments substituted. A classifier the file does not declare has none.
struct DeclaredHierarchy<'a>(&'a IrFile);

impl TypeOracle for DeclaredHierarchy<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(identity) = ty.kotlin_class_internal() else {
            return Vec::new();
        };
        let Some(class) = self
            .0
            .classes
            .iter()
            .find(|class| class.fq_name_id() == identity)
        else {
            return Vec::new();
        };
        let arguments = ty.type_args();
        // A supertype template names the class's own type parameters by semantic identity, which
        // the class's generic signature records beside each written name.
        let formals = self
            .0
            .class_signature_name(identity)
            .map_or(&[][..], |signature| &signature.type_params[..]);
        class
            .supertypes
            .iter()
            .map(|supertype| {
                ty_subst_supertype_template(
                    *supertype,
                    |name| {
                        formals
                            .iter()
                            .position(|formal| formal.semantic_name == name)
                            .and_then(|index| arguments.get(index).copied())
                    },
                    |_| false,
                )
            })
            .collect()
    }
}

impl IrFile {
    /// `sub` mapped through the hierarchy of the classes this file declares to the classifier `sup`
    /// names, keeping the arguments the hierarchy carries (`object S : Base<Token>()` against
    /// `KSerializer` → `KSerializer<Token>`). The walk is the shared [`applied_supertype`]; only a
    /// declared class contributes supertypes.
    pub fn declared_applied_supertype(&self, sub: Ty, sup: Ty) -> Option<Ty> {
        applied_supertype(&DeclaredHierarchy(self), sub, sup)
    }
}
