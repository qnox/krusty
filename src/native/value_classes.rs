//! Which classifiers a native program carries as value classes, and how it carries them.
//!
//! What a value class IS comes from the checked declarations the frontend hands a backend — the
//! classifier facts of the module being compiled and of its dependencies — never from a class's
//! shape or from how its name is spelled. So there is one inventory per file: every classifier the
//! IR references, every one their declared underlying types reach in turn, and the underlying type
//! each declares. The traversal over that inventory is the one every backend shares
//! (`crate::value_classes`); what belongs to this target is the REPRESENTATION POLICY, which says
//! when an occurrence travels as its underlying value and when as a box.
//!
//! A non-null `V` always travels as its underlying value — that is what the shared traversal
//! means by projecting it. A `V?` does too when the underlying value is a reference that cannot
//! itself be `null`: the reference's own `null` is then free to mean the outer one. Otherwise —
//! an underlying scalar, which has no `null` to spare, or an underlying that is already nullable —
//! `V?` is a BOX, one object of `V`'s own type holding the value. So is `V` wherever a position
//! does not know it holds a `V` at all: `Any`, a type parameter, an interface.

use std::collections::{HashMap, HashSet};

use crate::ir::IrFile;
use crate::types::{ClassifierFactSource, Ty, TypeName};
use crate::value_classes::{
    declarations_are_acyclic, project_underlying, underlying_chain_accepts_null,
    RepresentationPolicy, UnderlyingTypes,
};

/// Every value class one file reaches, each with the underlying type its declaration states.
#[derive(Debug, Default)]
pub(super) struct NativeValueClasses {
    declarations: UnderlyingTypes,
    /// The sole property holding the value, for each declaration that names it.
    properties: HashMap<TypeName, String>,
}

/// This target's answer to the one question the shared traversal leaves to a backend: whether a
/// NULLABLE occurrence of a value class can travel as its underlying value.
struct NativeRepresentation;

impl RepresentationPolicy for NativeRepresentation {
    fn project_nullable(
        &self,
        _classifier: TypeName,
        underlying: Ty,
        declarations: &UnderlyingTypes,
    ) -> bool {
        // The underlying value's own `null` must be free to mean the outer one, which asks two
        // things of it: that it be a reference, and that nothing below it be nullable already.
        if underlying_chain_accepts_null(underlying, declarations) {
            return false;
        }
        let terminal = project_underlying(underlying, declarations, self);
        !matches!(terminal, Ty::TyParam(..)) && terminal.scalar_value_repr().is_none()
    }
}

impl NativeValueClasses {
    /// The inventory of `ir`, read from the checked declarations in `facts`.
    ///
    /// Two publications of one declaration that disagree, a class the IR marks as a value class
    /// with no checked declaration behind it, and a declaration graph that reaches itself are each
    /// refused rather than resolved by picking one answer.
    pub(super) fn inventory(ir: &IrFile, facts: &dyn ClassifierFactSource) -> Result<Self, String> {
        let mut pending = ir.referenced_classifiers();
        // A dependency member is named by its checked signature, and the IR walk above covers
        // the call shapes a JVM needs; a native program also realizes the dependency members its
        // runtime answers, so their signatures are part of what it references.
        for expression in &ir.exprs {
            if let crate::ir::IrExpr::Call {
                callee: crate::ir::Callee::External { params, ret, .. },
                ..
            } = expression
            {
                for ty in params.iter().chain([ret]) {
                    crate::ir::collect_classifiers(*ty, &mut pending);
                }
            }
        }
        pending.extend(
            ir.classes
                .iter()
                .filter(|class| class.is_value)
                .map(|class| class.fq_name),
        );
        let mut inventory = Self::default();
        let mut probed = HashSet::new();
        while let Some(classifier) = pending.pop() {
            if !probed.insert(classifier) || is_builtin(classifier) {
                continue;
            }
            let mut declared: Option<Ty> = None;
            for candidate in [
                ir.external_value_class_name(classifier).copied(),
                facts.classifier_value_underlying(classifier),
            ]
            .into_iter()
            .flatten()
            {
                let candidate = candidate.canonical_semantic();
                match declared {
                    Some(existing) if existing != candidate => {
                        return Err(format!(
                            "a value class declared with two underlying types (`{}`)",
                            classifier.render().replace('/', ".")
                        ));
                    }
                    _ => declared = Some(candidate),
                }
            }
            let Some(underlying) = declared else {
                if ir
                    .classes
                    .iter()
                    .any(|class| class.fq_name == classifier && class.is_value)
                {
                    return Err(format!(
                        "a value class with no checked declaration (`{}`)",
                        classifier.render().replace('/', ".")
                    ));
                }
                continue;
            };
            if let Some(property) = facts.classifier_value_property(classifier) {
                inventory.properties.insert(classifier, property);
            }
            inventory.declarations.insert(classifier, underlying);
            crate::ir::collect_classifiers(underlying, &mut pending);
        }
        if !declarations_are_acyclic(&inventory.declarations) {
            return Err("a value class whose underlying type reaches itself".to_string());
        }
        Ok(inventory)
    }

    /// `ty` as this target carries it: a value class projected to its underlying value wherever
    /// the policy says so, anything else unchanged. Still a SEMANTIC type — the carrier is read
    /// from it as from any other.
    pub(super) fn project(&self, ty: Ty) -> Ty {
        if self.declarations.is_empty() {
            return ty;
        }
        project_underlying(ty, &self.declarations, &NativeRepresentation)
    }

    /// The value class `ty` is an UNBOXED occurrence of, or `None` when `ty` travels as itself —
    /// which a boxed `V?` does, being an object like any other.
    pub(super) fn unboxed(&self, ty: Ty) -> Option<TypeName> {
        let classifier = ty.non_null().obj_internal()?;
        (self.declarations.contains_key(&classifier) && self.project(ty) != ty)
            .then_some(classifier)
    }

    /// Whether `classifier` is a value class.
    pub(super) fn is_value_class(&self, classifier: TypeName) -> bool {
        self.declarations.contains_key(&classifier)
    }

    /// The underlying type `classifier` declares, when it is a value class.
    pub(super) fn underlying(&self, classifier: TypeName) -> Option<Ty> {
        self.declarations.get(&classifier).copied()
    }

    /// The property a value class holds its value in, when its declaration names it.
    pub(super) fn property(&self, classifier: TypeName) -> Option<&str> {
        self.properties.get(&classifier).map(String::as_str)
    }
}

/// The unsigned integers and their specialized arrays: value classes to Kotlin, and built-ins to
/// every backend, with carriers, boxes and descriptors of their own — the JVM's inventory skips the
/// same set. They are never projected.
fn is_builtin(classifier: TypeName) -> bool {
    Ty::obj_name(classifier).is_unsigned() || crate::types::prim_array_element(classifier).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{type_name, ResolvedAnnotation};

    /// Checked facts naming a fixed set of value classes.
    struct Facts(Vec<(&'static str, Ty, &'static str)>);

    impl ClassifierFactSource for Facts {
        fn classifier_annotations(&self, _classifier: TypeName) -> Option<Vec<ResolvedAnnotation>> {
            None
        }

        fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
            self.0
                .iter()
                .find(|(name, _, _)| classifier.matches(name))
                .map(|(_, underlying, _)| *underlying)
        }

        fn classifier_value_property(&self, classifier: TypeName) -> Option<String> {
            self.0
                .iter()
                .find(|(name, _, _)| classifier.matches(name))
                .map(|(_, _, property)| property.to_string())
        }
    }

    /// A file whose one function takes each of `types`.
    fn referencing(types: &[Ty]) -> IrFile {
        let mut ir = IrFile::default();
        ir.functions.push(crate::ir::IrFunction {
            name: "f".to_string(),
            params: types.to_vec(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir
    }

    #[test]
    fn the_inventory_is_what_the_checked_declarations_say() {
        let facts = Facts(vec![("app/Name", Ty::String, "value")]);
        let ir = referencing(&[Ty::obj("app/Name"), Ty::obj("app/Other")]);
        let inventory = NativeValueClasses::inventory(&ir, &facts).expect("an inventory");
        assert_eq!(inventory.project(Ty::obj("app/Name")), Ty::String);
        assert_eq!(inventory.property(type_name("app/Name")), Some("value"));
        assert!(!inventory.is_value_class(type_name("app/Other")));
    }

    #[test]
    fn an_underlying_value_class_is_followed() {
        let facts = Facts(vec![
            ("app/Outer", Ty::obj("app/Inner"), "inner"),
            ("app/Inner", Ty::Int, "n"),
        ]);
        let ir = referencing(&[Ty::obj("app/Outer")]);
        let inventory = NativeValueClasses::inventory(&ir, &facts).expect("an inventory");
        assert!(inventory.is_value_class(type_name("app/Inner")));
        assert_eq!(inventory.project(Ty::obj("app/Outer")), Ty::Int);
    }

    #[test]
    fn the_unsigned_integers_stay_built_in_scalars() {
        let facts = Facts(vec![("kotlin/UInt", Ty::Int, "data")]);
        let ir = referencing(&[Ty::obj("kotlin/UInt")]);
        let inventory = NativeValueClasses::inventory(&ir, &facts).expect("an inventory");
        assert!(!inventory.is_value_class(type_name("kotlin/UInt")));
    }

    #[test]
    fn the_unsigned_arrays_stay_built_in_arrays() {
        let facts = Facts(vec![(
            "kotlin/UIntArray",
            Ty::obj("kotlin/IntArray"),
            "storage",
        )]);
        let ir = referencing(&[Ty::obj("kotlin/UIntArray")]);
        let inventory = NativeValueClasses::inventory(&ir, &facts).expect("an inventory");
        assert!(!inventory.is_value_class(type_name("kotlin/UIntArray")));
    }

    #[test]
    fn a_declaration_graph_that_reaches_itself_is_refused() {
        let facts = Facts(vec![
            ("app/First", Ty::obj("app/Second"), "second"),
            ("app/Second", Ty::obj("app/First"), "first"),
        ]);
        let ir = referencing(&[Ty::obj("app/First")]);
        assert!(NativeValueClasses::inventory(&ir, &facts).is_err());
    }

    #[test]
    fn a_nullable_occurrence_is_unboxed_only_where_its_null_is_free() {
        let facts = Facts(vec![
            ("app/Name", Ty::String, "value"),
            ("app/Count", Ty::Int, "n"),
            ("app/Maybe", Ty::nullable(Ty::String), "value"),
        ]);
        let ir = referencing(&[
            Ty::obj("app/Name"),
            Ty::obj("app/Count"),
            Ty::obj("app/Maybe"),
        ]);
        let inventory = NativeValueClasses::inventory(&ir, &facts).expect("an inventory");
        let nullable = |name: &str| Ty::nullable(Ty::obj(name));
        // A reference that cannot be `null` lends its `null` to the outer one.
        assert_eq!(
            inventory.project(nullable("app/Name")),
            Ty::nullable(Ty::String)
        );
        assert_eq!(
            inventory.unboxed(nullable("app/Name")),
            Some(type_name("app/Name"))
        );
        // A scalar has no `null` to spare, and a nullable underlying has already spent its own.
        assert_eq!(
            inventory.project(nullable("app/Count")),
            nullable("app/Count")
        );
        assert_eq!(inventory.unboxed(nullable("app/Count")), None);
        assert_eq!(
            inventory.project(nullable("app/Maybe")),
            nullable("app/Maybe")
        );
        // A non-null occurrence is always its underlying value.
        assert_eq!(inventory.project(Ty::obj("app/Count")), Ty::Int);
        assert_eq!(
            inventory.unboxed(Ty::obj("app/Count")),
            Some(type_name("app/Count"))
        );
    }
}
