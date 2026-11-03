//! kotlinc's containing-declaration rule for modifiers (`WRONG_MODIFIER_CONTAINING_DECLARATION`).
//!
//! Some modifiers are applicable only inside some kinds of declaration: `protected` needs a class
//! that can be subclassed or a companion, `inner` needs a class an instance can be nested in, and
//! so on. kotlinc's `FirModifierChecker.checkParent` describes the containing declaration by a
//! list of targets (`KotlinTarget.classActualTargets`, or the file) and reports a modifier when
//! none of those targets is in that modifier's set (`possibleParentTargetPredicateMap`). The
//! report sits on the keyword and names the container's first target. The checks read the
//! modifier list as written, which the parser keeps in [`File::declaration_prefixes`].
//!
//! kotlinc runs the target rule first and skips this one when the declaration itself cannot carry
//! the modifier, so each modifier is checked here only on declarations it applies to.

use crate::ast::{ClassDecl, Decl, DeclId, File, ModifierKeyword};
use crate::diag::{DiagSink, Span};

/// A `KotlinTarget` a containing declaration presents to the rule.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    ClassOnly,
    LocalClass,
    AnnotationClass,
    Interface,
    EnumClass,
    EnumEntry,
    Object,
    StandaloneObject,
    CompanionObject,
    File,
}

/// A containing declaration's target list (`KotlinTarget.*_LIST`): the first target names it in
/// the diagnostic. Every classifier list also holds `CLASS`, and an enum entry's `PROPERTY` and
/// `FIELD`; no modifier's set names those, so they are left out.
#[derive(Clone, Copy)]
struct Container {
    targets: &'static [Target],
    description: &'static str,
}

const CLASS: Container = Container {
    targets: &[Target::ClassOnly],
    description: "class",
};
const LOCAL_CLASS: Container = Container {
    targets: &[Target::LocalClass],
    description: "local class",
};
const ANNOTATION_CLASS: Container = Container {
    targets: &[Target::AnnotationClass],
    description: "annotation class",
};
const INTERFACE: Container = Container {
    targets: &[Target::Interface],
    description: "interface",
};
const ENUM_CLASS: Container = Container {
    targets: &[Target::EnumClass],
    description: "enum class",
};
const ENUM_ENTRY: Container = Container {
    targets: &[Target::EnumEntry],
    description: "enum entry",
};
const STANDALONE_OBJECT: Container = Container {
    targets: &[Target::StandaloneObject, Target::Object],
    description: "standalone object",
};
const COMPANION_OBJECT: Container = Container {
    targets: &[Target::CompanionObject, Target::Object],
    description: "companion object",
};
const FILE: Container = Container {
    targets: &[Target::File],
    description: "file",
};

/// The containing targets that admit `modifier`, or `None` when the rule does not constrain it.
/// An object literal's members see the local-class list, so `OBJECT_LITERAL` never matches here.
/// A top-level `companion` (`CompanionBlocksAndExtensions`) is the parser's to accept or reject.
fn admitted(modifier: ModifierKeyword) -> Option<&'static [Target]> {
    use Target::*;
    Some(match modifier {
        ModifierKeyword::Inner => &[ClassOnly, LocalClass, EnumClass, EnumEntry],
        ModifierKeyword::Override => &[
            ClassOnly, LocalClass, Object, Interface, EnumClass, EnumEntry,
        ],
        ModifierKeyword::Protected => &[ClassOnly, LocalClass, EnumClass, CompanionObject],
        ModifierKeyword::Internal => &[ClassOnly, LocalClass, Object, EnumClass, EnumEntry, File],
        ModifierKeyword::Private => &[
            ClassOnly, LocalClass, Object, Interface, EnumClass, EnumEntry, File,
        ],
        ModifierKeyword::Companion => &[ClassOnly, Interface, EnumClass, AnnotationClass, File],
        ModifierKeyword::Final => &[
            ClassOnly,
            LocalClass,
            Object,
            EnumClass,
            EnumEntry,
            AnnotationClass,
            File,
        ],
        _ => return None,
    })
}

/// What a declaration is, for the modifiers kotlinc's target rule lets it carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Declaration {
    Callable,
    Constructor,
    Class,
    OtherClassifier,
    Singleton,
}

impl Declaration {
    fn carries(self, modifier: ModifierKeyword) -> bool {
        match modifier {
            ModifierKeyword::Private | ModifierKeyword::Internal | ModifierKeyword::Protected => {
                true
            }
            ModifierKeyword::Override => self == Declaration::Callable,
            ModifierKeyword::Final => matches!(self, Declaration::Callable | Declaration::Class),
            ModifierKeyword::Inner => self == Declaration::Class,
            ModifierKeyword::Companion => self == Declaration::Singleton,
            _ => false,
        }
    }

    fn of_classifier(class: &ClassDecl) -> Declaration {
        if class.is_singleton() {
            Declaration::Singleton
        } else if class.kind == crate::ast::ClassKind::Class {
            Declaration::Class
        } else {
            Declaration::OtherClassifier
        }
    }
}

/// Report each modifier written inside a declaration kind that does not admit it, in source order.
pub(super) fn check(file: &File, diagnostics: &mut DiagSink) {
    let mut findings = Findings {
        file,
        reports: Vec::new(),
    };
    for &id in &file.decls {
        match file.decl(id) {
            Decl::Class(class) => {
                if let Some(container) = classifier_container(file, id) {
                    findings.declaration(
                        class.name_span,
                        Declaration::of_classifier(class),
                        container,
                    );
                }
                findings.members(class, container_of(file, id, class));
            }
            Decl::Fun(function) if is_top_level(file, id) => {
                findings.declaration(function.span, Declaration::Callable, FILE)
            }
            Decl::Property(property) if is_top_level(file, id) => {
                findings.declaration(property.span, Declaration::Callable, FILE)
            }
            _ => {}
        }
    }
    findings.reports.sort_by_key(|(span, _)| span.lo);
    for (span, message) in findings.reports {
        diagnostics.error(span, message);
    }
}

fn is_top_level(file: &File, id: DeclId) -> bool {
    !file.is_local_declaration(id)
}

/// The declaration containing classifier `id`, when the parser records it: the file, the
/// classifier whose body declares it, or the enum entry whose body does. A classifier nested in a
/// local class or an object literal has no recorded owner and is not checked here.
fn classifier_container(file: &File, id: DeclId) -> Option<Container> {
    if let Some(owner) = file.hoisted_classifier_owner(id) {
        let Decl::Class(class) = file.decl(owner) else {
            return None;
        };
        return Some(container_of(file, owner, class));
    }
    if file.enum_entry_nested_classifier_owners.contains_key(&id) {
        return Some(ENUM_ENTRY);
    }
    let top_level = !file.hoisted_classifier_source_names.contains_key(&id)
        && !file.is_local_declaration(id)
        && !file.local_class_enclosing_declarations.contains_key(&id)
        && !file.is_anonymous_object_class(id);
    top_level.then_some(FILE)
}

/// The target list classifier `id` presents to its own members (`classActualTargets`).
fn container_of(file: &File, id: DeclId, class: &ClassDecl) -> Container {
    use crate::ast::ClassKind;
    let local = file.local_class_enclosing_declarations.contains_key(&id)
        || file
            .local_class_nested
            .values()
            .any(|nested| nested.contains(&id));
    if file.is_anonymous_object_class(id) {
        return LOCAL_CLASS;
    }
    match class.kind {
        ClassKind::Annotation => ANNOTATION_CLASS,
        ClassKind::Interface => INTERFACE,
        ClassKind::Enum if local => LOCAL_CLASS,
        ClassKind::Enum => ENUM_CLASS,
        ClassKind::Class if class.is_singleton() => {
            let companion = file.hoisted_classifier_owner(id).is_some_and(|owner| {
                matches!(file.decl(owner), Decl::Class(outer) if outer.companion == Some(id))
            });
            if companion {
                COMPANION_OBJECT
            } else {
                STANDALONE_OBJECT
            }
        }
        ClassKind::Class if local && class.inner_of.is_none() => LOCAL_CLASS,
        ClassKind::Class => CLASS,
    }
}

struct Findings<'f> {
    file: &'f File,
    reports: Vec<(Span, String)>,
}

impl Findings<'_> {
    /// Check the modifier list found at `anchor`, a token offset that keys it, for a declaration
    /// of kind `declaration` inside `container`.
    fn declaration(&mut self, anchor: Span, declaration: Declaration, container: Container) {
        let prefixes = &self.file.declaration_prefixes;
        let Some(prefix) = prefixes.at(anchor.lo) else {
            return;
        };
        for &(modifier, span) in prefixes.keywords(prefix) {
            let Some(admitted) = admitted(modifier) else {
                continue;
            };
            if !declaration.carries(modifier)
                || container
                    .targets
                    .iter()
                    .any(|target| admitted.contains(target))
            {
                continue;
            }
            self.reports.push((
                span,
                format!(
                    "modifier '{}' is not applicable inside '{}'.",
                    modifier.keyword(),
                    container.description
                ),
            ));
        }
    }

    fn members(&mut self, class: &ClassDecl, container: Container) {
        for parameter in class.props.iter().filter(|parameter| parameter.is_property) {
            self.declaration(parameter.declaration_span, Declaration::Callable, container);
        }
        for function in &class.methods {
            self.declaration(function.span, Declaration::Callable, container);
        }
        for property in &class.body_props {
            self.declaration(property.span, Declaration::Callable, container);
        }
        for constructor in &class.secondary_ctors {
            self.declaration(constructor.span, Declaration::Constructor, container);
        }
        for entry in &class.enum_entries {
            for function in &entry.methods {
                self.declaration(function.span, Declaration::Callable, ENUM_ENTRY);
            }
            for property in &entry.props {
                self.declaration(property.span, Declaration::Callable, ENUM_ENTRY);
            }
        }
    }
}
