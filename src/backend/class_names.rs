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
use crate::libraries::ClassifierDeclarationKey;
use crate::types::{ClassifierDeclarationName, TypeName};

use super::BackendClassifierSource;

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

/// What Kotlin reflection names one classifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionNames {
    /// The dotted package the classifier is declared in, empty for the root package.
    pub package: String,
    /// `simpleName`: absent for an anonymous object.
    pub simple: Option<String>,
    /// `qualifiedName`: absent for a local class and an anonymous object.
    pub qualified: Option<String>,
}

impl ReflectionNames {
    /// What kind of declaration the names describe.
    pub fn kind(&self) -> ReflectionNameKind {
        match (&self.qualified, &self.simple) {
            (Some(_), _) => ReflectionNameKind::Member,
            (None, Some(_)) => ReflectionNameKind::Local,
            (None, None) => ReflectionNameKind::Anonymous,
        }
    }
}

/// Which names a classifier publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionNameKind {
    /// A top-level or nested classifier: both names.
    Member,
    /// A local class: a simple name only.
    Local,
    /// An anonymous object, or a classifier the compiler generated: neither.
    Anonymous,
}

/// The reflection names of the class `class` that `ir` declares.
pub fn declared_class(ir: &IrFile, class: ClassId) -> ReflectionNames {
    let declaration = &ir.classes[class as usize];
    let mut outer = declaration.fq_name;
    while let Some(owner) = outer.nested_owner() {
        outer = owner;
    }
    // Boundary conversion: the package path is spelled with `/` in the name tree.
    let package = outer.package().replace('/', ".");
    if declaration.is_anonymous_object || !declaration.is_source_declared {
        return ReflectionNames {
            package,
            simple: None,
            qualified: None,
        };
    }
    if declaration.is_local_class {
        let simple = ir
            .local_class_name_provenance
            .get(&class)
            .and_then(|provenance| provenance.segments.last())
            .cloned()
            .unwrap_or_else(|| declaration.fq_name.nested_segment_ref().to_string());
        return ReflectionNames {
            package,
            simple: Some(simple),
            qualified: None,
        };
    }
    let ClassNames { qualified, simple } = member_class_names(declaration.fq_name);
    ReflectionNames {
        package,
        simple: Some(simple),
        qualified: Some(qualified),
    }
}

/// The reflection names of a non-local classifier from the declaration name its provider
/// published, or `None` when the provider published none for it or for one of its lexical owners.
pub fn published_classifier(
    classifiers: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Option<ReflectionNames> {
    let mut segments = Vec::new();
    let mut current = classifier;
    // Each step leaves a classifier for its lexical owner; a chain longer than any real nesting
    // is a cycle in the published facts.
    for _ in 0..64 {
        let declaration = classifiers
            .classifier_declaration_name(ClassifierDeclarationKey::Ordinary(current))
            .flatten()?;
        match declaration {
            ClassifierDeclarationName::TopLevel { package, name } => {
                segments.push(name.to_string());
                segments.reverse();
                // Boundary conversion: the names are emitted text, not a lookup key.
                let package = package.render().replace('/', ".");
                let simple = segments.last().cloned();
                let path = segments.join(".");
                let qualified = if package.is_empty() {
                    path
                } else {
                    format!("{package}.{path}")
                };
                return Some(ReflectionNames {
                    package,
                    simple,
                    qualified: Some(qualified),
                });
            }
            ClassifierDeclarationName::Nested { owner, name } => {
                segments.push(name.to_string());
                current = owner;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
