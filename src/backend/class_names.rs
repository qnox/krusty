//! The names Kotlin gives a member classifier: its dotted qualified name and its simple name.
//!
//! A member classifier is one declared at top level or nested in another. Its qualified name is
//! its package, then each enclosing classifier, then itself, joined with `.`
//! (`ClassId.asFqNameString`); its simple name is its own segment. Both are read from the exact
//! nesting identities the name tree records, never by splitting a rendered spelling: a source
//! segment may itself contain `$` or `.`. Local classifiers and anonymous objects are not member
//! classifiers and have no name of this shape; [`source_class_names`] gives every source classifier
//! of a file the name Kotlin's IR gives it.

use crate::ir::{ClassId, IrFile, IrLocalClassOwner};
use crate::types::TypeName;

/// The simple name Kotlin's IR gives an anonymous object (`SpecialNames.NO_NAME_PROVIDED`).
pub const ANONYMOUS_OBJECT_NAME: &str = "<no name provided>";

/// The qualified and simple name of a classifier.
pub struct ClassNames {
    /// `pkg.Outer.Inner`.
    pub qualified: String,
    /// `Inner`.
    pub simple: String,
}

/// The names of the member classifier `name`.
pub fn member_class_names(name: TypeName) -> ClassNames {
    let mut nested = Vec::new();
    let mut outer = name;
    while let Some(owner) = outer.nested_owner() {
        nested.push(
            outer
                .nested_segment_within(owner)
                .unwrap_or_else(|| outer.nested_segment_ref()),
        );
        outer = owner;
    }
    let simple = nested
        .first()
        .copied()
        .unwrap_or_else(|| outer.segment_ref())
        .to_string();
    // Boundary conversion: the package path is spelled with `/` in the name tree.
    let mut qualified = outer.render().replace('/', ".");
    for segment in nested.iter().rev() {
        qualified.push('.');
        qualified.push_str(segment);
    }
    ClassNames { qualified, simple }
}

/// The names Kotlin's IR gives the source classifier `class` of `ir`. Its qualified name
/// (`kotlinFqName`) is the package, then each enclosing classifier, then the classifier itself,
/// joined with `.`; its simple name is its own segment. An executable scope contributes no segment,
/// so a local classifier declared in a function of `Outer` is `pkg.Outer.Local`, and an anonymous
/// object's own segment is [`ANONYMOUS_OBJECT_NAME`].
///
/// The names are read from the declaration-owned naming provenance a local classifier records (its
/// lexical owner and its source segment), never from its physical identity. `None` for a
/// classifier the compiler generated, and for one nested in an enum entry body, whose segment
/// Kotlin does not spell from source.
pub fn source_class_names(ir: &IrFile, class: ClassId) -> Option<ClassNames> {
    let declaration = &ir.classes[class as usize];
    if !declaration.is_source_declared || declaration.is_enum_entry {
        return None;
    }
    let Some(provenance) = ir.local_class_name_provenance.get(&class) else {
        if declaration.is_local_class || declaration.is_anonymous_object {
            return None;
        }
        // A member classifier. An owner this file declares with naming provenance of its own is
        // local, and then so would this classifier be: it has no member name.
        let mut owner = declaration.fq_name_id().nested_owner();
        while let Some(name) = owner {
            if ir
                .class_id_by_name(name)
                .is_some_and(|owner| ir.local_class_name_provenance.contains_key(&owner))
            {
                return None;
            }
            owner = name.nested_owner();
        }
        return Some(member_class_names(declaration.fq_name_id()));
    };
    let own = if declaration.is_anonymous_object {
        ANONYMOUS_OBJECT_NAME
    } else if provenance.ordinal.is_none() {
        provenance.segments.last()?.as_str()
    } else {
        return None;
    };
    let mut qualified = match provenance.lexical_owner {
        Some(IrLocalClassOwner::Class(owner)) => source_class_names(ir, owner)?.qualified,
        Some(IrLocalClassOwner::External(owner)) => member_class_names(owner).qualified,
        // Boundary conversion: the package path is spelled with `/` in the name tree.
        None => provenance.source.package.render().replace('/', "."),
    };
    if !qualified.is_empty() {
        qualified.push('.');
    }
    qualified.push_str(own);
    Some(ClassNames {
        qualified,
        simple: own.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{member_class_names, source_class_names};
    use crate::ir::{ClassId, IrClass, IrFile, IrLocalClassNameProvenance, IrLocalClassOwner};
    use crate::types::{type_name, TypeName};

    fn qualified(ir: &IrFile, class: ClassId) -> Option<String> {
        source_class_names(ir, class).map(|names| names.qualified)
    }

    fn declared(ir: &mut IrFile, name: TypeName) -> ClassId {
        let mut class = IrClass::synthetic(name);
        class.is_source_declared = true;
        ir.add_class(class)
    }

    /// A classifier declared in executable code under `owner` (the file when `None`), with its
    /// source `segments` and, for an unnamed one, its ordinal.
    fn local(
        ir: &mut IrFile,
        physical: &str,
        owner: Option<IrLocalClassOwner>,
        segments: &[&str],
        ordinal: Option<u32>,
    ) -> ClassId {
        let class = declared(ir, type_name(physical));
        ir.classes[class as usize].is_local_class = ordinal.is_none();
        ir.classes[class as usize].is_anonymous_object = ordinal.is_some();
        ir.local_class_name_provenance.insert(
            class,
            IrLocalClassNameProvenance {
                source: crate::ir::IrModuleSource {
                    source: crate::fir::SourceFileId::from_raw(0),
                    package: type_name("p/q"),
                },
                lexical_owner: owner,
                segments: segments.iter().map(|segment| segment.to_string()).collect(),
                ordinal,
                parents: Box::new([]),
            },
        );
        class
    }

    #[test]
    fn a_local_class_is_named_by_its_package_and_enclosing_classifiers_only() {
        let mut ir = IrFile::default();
        let outer = declared(&mut ir, type_name("p/q/Outer"));
        let in_function = local(&mut ir, "p/q/L0", None, &["box", "Local"], None);
        let in_member = local(
            &mut ir,
            "p/q/L1",
            Some(IrLocalClassOwner::Class(outer)),
            &["make", "Made"],
            None,
        );
        let nested = local(
            &mut ir,
            "p/q/L2",
            Some(IrLocalClassOwner::Class(in_function)),
            &["Deep"],
            None,
        );
        assert_eq!(qualified(&ir, outer).as_deref(), Some("p.q.Outer"));
        assert_eq!(qualified(&ir, in_function).as_deref(), Some("p.q.Local"));
        assert_eq!(qualified(&ir, in_member).as_deref(), Some("p.q.Outer.Made"));
        assert_eq!(qualified(&ir, nested).as_deref(), Some("p.q.Local.Deep"));
    }

    #[test]
    fn an_anonymous_object_has_no_name_provided() {
        let mut ir = IrFile::default();
        let outer = declared(&mut ir, type_name("p/q/Outer"));
        let in_file = local(&mut ir, "p/q/A0", None, &["box"], Some(1));
        let in_member = local(
            &mut ir,
            "p/q/A1",
            Some(IrLocalClassOwner::Class(outer)),
            &["make"],
            Some(1),
        );
        let external = local(
            &mut ir,
            "p/q/A2",
            Some(IrLocalClassOwner::External(
                type_name("p/r/Other").nested_child("Inner"),
            )),
            &["make"],
            Some(2),
        );
        assert_eq!(
            qualified(&ir, in_file).as_deref(),
            Some("p.q.<no name provided>")
        );
        assert_eq!(
            qualified(&ir, in_member).as_deref(),
            Some("p.q.Outer.<no name provided>")
        );
        assert_eq!(
            qualified(&ir, external).as_deref(),
            Some("p.r.Other.Inner.<no name provided>")
        );
    }

    #[test]
    fn a_generated_classifier_has_no_source_name() {
        let mut ir = IrFile::default();
        let generated = ir.add_class(IrClass::synthetic(type_name("p/q/Generated")));
        let named_generated = local(&mut ir, "p/q/G1", None, &["box"], Some(3));
        ir.classes[named_generated as usize].is_anonymous_object = false;
        assert!(source_class_names(&ir, generated).is_none());
        assert!(source_class_names(&ir, named_generated).is_none());
    }

    #[test]
    fn a_top_level_class_is_its_package_and_its_name() {
        let names = member_class_names(type_name("kotlin/IllegalStateException"));
        assert_eq!(names.qualified, "kotlin.IllegalStateException");
        assert_eq!(names.simple, "IllegalStateException");
    }

    #[test]
    fn a_class_in_the_root_package_has_no_package_prefix() {
        let names = member_class_names(type_name("Foo"));
        assert_eq!(names.qualified, "Foo");
        assert_eq!(names.simple, "Foo");
    }

    #[test]
    fn a_nested_class_is_joined_to_its_owner_with_a_dot() {
        let outer = type_name("p/q/Outer");
        let inner = outer.nested_child("Inner");
        let names = member_class_names(inner);
        assert_eq!(names.qualified, "p.q.Outer.Inner");
        assert_eq!(names.simple, "Inner");
    }
}
