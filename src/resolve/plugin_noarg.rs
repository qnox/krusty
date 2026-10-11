//! Zero-argument constructors contributed by native compiler plugins: kotlinc's no-arg plugin.
//!
//! A plugin names annotations ([`crate::plugins::IrPlugin::no_arg_constructor_annotations`]); a
//! class they match ([`ClassPredicate`]) gets a generated `<init>()` that delegates to its
//! superclass's zero-argument constructor. This module makes kotlinc's two decisions once, where
//! the frontend publishes declaration headers:
//!
//! - its `FirNoArgDeclarationChecker`: a matched inner class, or one whose superclass has no
//!   constructor callable without arguments and is not matched itself, is an error on the class's
//!   name. kotlinc 2.4.20 reports nothing for a matched value class and generates nothing for it;
//! - its `FirNoArgConstructorGenerator`: which matched classes get the constructor. A class that
//!   already declares a constructor JVM callers can call without arguments gets none.
//!
//! The decision is published as the superclass constructor the generated one delegates to
//! ([`crate::fir::ResolvedModuleIndex::no_arg_constructor`]), and lowering builds the constructor
//! from it. A local class's annotations resolve only in its body
//! pass, after headers are published, so a local class never matches here.

use std::collections::HashMap;

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, NoArgSuperConstructor, ResolvedModuleIndex,
    StreamedHeaderModule,
};
use crate::types::TypeName;

use super::plugin_class_predicate::ClassPredicate;
use super::SymbolTable;

const NOARG_ON_INNER_CLASS: &str =
    "noarg constructor generation is not possible for inner classes.";
const NO_NOARG_CONSTRUCTOR_IN_SUPERCLASS: &str =
    "zero-argument constructor was not found in the superclass.";

/// The source classes that get a generated zero-argument constructor, with the superclass
/// constructor each one delegates to.
#[derive(Default)]
pub(super) struct NoArgConstructors {
    classes: HashMap<DeclarationId, NoArgSuperConstructor>,
}

impl NoArgConstructors {
    /// Match every source class against the annotations the enabled plugins name, report the
    /// plugin's errors, and record the classes that get a constructor.
    pub(super) fn collect(
        headers: &StreamedHeaderModule,
        table: &SymbolTable,
        classifier_types: &HashMap<DeclarationId, TypeName>,
        diags: &mut crate::diag::DiagSink,
    ) -> NoArgConstructors {
        let annotations = table
            .native_plugins
            .host("main")
            .no_arg_constructor_annotations();
        if annotations.is_empty() {
            return NoArgConstructors::default();
        }
        let mut predicate = ClassPredicate::new(table, annotations);
        let mut generated = NoArgConstructors::default();
        for stub in &headers.stubs {
            if stub.kind != DeclarationKind::Classifier || !is_class_kind(stub.flags) {
                continue;
            }
            let Some(&classifier) = classifier_types.get(&stub.id) else {
                continue;
            };
            let Some(class) = table.classes.get(&classifier) else {
                continue;
            };
            if !predicate.class_matches(classifier) {
                continue;
            }
            // kotlinc 2.4.20 neither reports nor generates anything for a matched value class.
            if stub.flags.has(DeclarationFlags::VALUE)
                || stub.flags.has(DeclarationFlags::VALUE_KEYWORD)
            {
                continue;
            }
            if stub.flags.has(DeclarationFlags::INNER) {
                diags.set_file(stub.source.raw());
                diags.error(class.name_span, NOARG_ON_INNER_CLASS);
                continue;
            }
            let own = constructors(headers, table, classifier);
            // kotlinc's checker asks only for a constructor whose parameters all have defaults; its
            // generator also needs that constructor to compile to `<init>()`.
            let superclass = class.super_internal;
            let super_constructors =
                superclass.map(|superclass| constructors(headers, table, superclass));
            let superclass_matches =
                superclass.is_some_and(|superclass| predicate.class_matches(superclass));
            let super_has = |test: fn(&Constructor) -> bool| {
                super_constructors
                    .as_ref()
                    .is_none_or(|constructors| constructors.iter().any(test))
            };
            if !own.iter().any(Constructor::all_defaulted)
                && !super_has(Constructor::all_defaulted)
                && !superclass_matches
            {
                diags.set_file(stub.source.raw());
                diags.error(class.name_span, NO_NOARG_CONSTRUCTOR_IN_SUPERCLASS);
                continue;
            }
            if stub.flags.has(DeclarationFlags::LOCAL_CLASS)
                || own.iter().any(Constructor::zero_parameter)
            {
                continue;
            }
            // The superclass constructor the generated one calls with every argument omitted,
            // else the no-arg constructor the plugin generates for a matched superclass. kotlinc
            // generates none over a superclass whose only all-defaulted constructor is a plain
            // secondary one: its checker accepts that constructor, its generator does not.
            let target = match &super_constructors {
                None => Some(NoArgSuperConstructor::Unrestricted),
                Some(constructors) => constructors
                    .iter()
                    .find(|constructor| constructor.zero_parameter())
                    .and_then(|constructor| constructor.target.clone())
                    .or(superclass_matches.then_some(NoArgSuperConstructor::Unrestricted)),
            };
            if let Some(target) = target {
                generated.classes.insert(stub.id, target);
            }
        }
        generated
    }

    /// Publish each matched class's generated constructor into the module index.
    pub(super) fn publish(self, index: &mut ResolvedModuleIndex) {
        for (classifier, superclass_constructor) in self.classes {
            index.publish_no_arg_constructor(classifier, superclass_constructor);
        }
    }
}

/// kotlinc's no-arg plugin applies to a `class`: not an interface, object, enum or annotation
/// class.
fn is_class_kind(flags: DeclarationFlags) -> bool {
    !flags.has(DeclarationFlags::INTERFACE)
        && !flags.has(DeclarationFlags::SINGLETON)
        && !flags.has(DeclarationFlags::ENUM)
        && !flags.has(DeclarationFlags::ANNOTATION_CLASS)
}

/// The facts of one declared constructor that the plugin's rules read.
struct Constructor {
    /// The constructor as a generated one delegates to it; `None` when it has no identity.
    target: Option<NoArgSuperConstructor>,
    primary: bool,
    parameter_defaults: Vec<bool>,
    jvm_overloads: bool,
}

impl Constructor {
    /// Every parameter has a default: kotlinc's checker's notion of a no-arg constructor.
    fn all_defaulted(&self) -> bool {
        self.parameter_defaults.iter().all(|default| *default)
    }

    /// Callable as `<init>()`: no parameters, or all defaulted on the primary constructor (which
    /// gets kotlinc's no-arg overload) or on a `@JvmOverloads` one.
    fn zero_parameter(&self) -> bool {
        self.all_defaulted()
            && (self.parameter_defaults.is_empty() || self.primary || self.jvm_overloads)
    }
}

/// The declared constructors of `classifier`: from its source declaration, or from its dependency
/// provider. An unknown classifier declares none.
fn constructors(
    headers: &StreamedHeaderModule,
    table: &SymbolTable,
    classifier: TypeName,
) -> Vec<Constructor> {
    let jvm_overloads = crate::types::type_name("kotlin/jvm/JvmOverloads");
    if let Some(class) = table.classes.get(&classifier) {
        // A source class's constructors are its owned constructor declarations by sibling
        // ordinal: 0 for the primary, `n` for the `n`th secondary.
        let declaration = |sibling: usize| {
            let owner = class.stable_declaration?;
            headers
                .declarations
                .owned(owner)
                .iter()
                .copied()
                .find(|declaration| {
                    headers
                        .declarations
                        .anchor(*declaration)
                        .is_some_and(|anchor| {
                            anchor.kind == DeclarationKind::Constructor
                                && anchor.sibling as usize == sibling
                        })
                })
        };
        let primary = class.has_primary_ctor.then(|| Constructor {
            target: declaration(0).map(NoArgSuperConstructor::Declared),
            primary: true,
            parameter_defaults: class
                .ctor_param_names
                .iter()
                .map(|(_, default)| *default)
                .collect(),
            jvm_overloads: class
                .primary_constructor_annotations
                .contains(&jvm_overloads),
        });
        let secondaries = class
            .secondary_ctor_call_sigs
            .iter()
            .zip(&class.secondary_constructor_annotations)
            .enumerate()
            .map(|(ordinal, (signature, annotations))| Constructor {
                target: declaration(ordinal + 1).map(NoArgSuperConstructor::Declared),
                primary: false,
                parameter_defaults: signature.param_defaults.clone(),
                jvm_overloads: annotations.contains(&jvm_overloads),
            });
        return primary.into_iter().chain(secondaries).collect();
    }
    crate::symbol_source::SymbolSource::classifier(table.libraries.as_ref(), classifier)
        .map(|shape| {
            shape
                .constructors
                .iter()
                .map(|constructor| Constructor {
                    target: external_target(constructor),
                    primary: constructor.is_primary_constructor(),
                    // Default PRESENCE is a source-call fact. `default_values` is only the
                    // provider's optional closed-value payload and may be empty for a call or
                    // parameter reference that the dependency realizes in its own constructor.
                    parameter_defaults: (0..constructor.params.len())
                        .map(|index| constructor.call_sig.param_has_default(index))
                        .collect(),
                    jvm_overloads: constructor.annotations.contains(&jvm_overloads),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A dependency constructor as a generated constructor delegates to it: by its provider identity,
/// so lowering reaches its exact declaration (and, with parameters, its defaults).
fn external_target(constructor: &crate::libraries::LibraryMember) -> Option<NoArgSuperConstructor> {
    let declaration = constructor.external_identity?;
    let parameters = constructor
        .params
        .iter()
        .copied()
        .map(crate::fir::ResolvedTy::new)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(NoArgSuperConstructor::External {
        declaration,
        parameters,
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use super::*;
    use crate::libraries::{LibraryMember, LibraryType, ResolvedSymbols, SemanticPlatform};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::Ty;

    struct DefaultedConstructorPlatform {
        identity: TypeName,
        classifier: Arc<LibraryType>,
    }

    impl SymbolSource for DefaultedConstructorPlatform {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            let key = SymbolNamespace::classifier_key(self.identity);
            if namespace == key.0 && name == key.1 {
                Rc::new(ResolvedSymbols {
                    classifier_name: Some(self.identity),
                    classifier: Some(Arc::clone(&self.classifier)),
                    ..ResolvedSymbols::default()
                })
            } else {
                Rc::new(ResolvedSymbols::default())
            }
        }
    }

    impl SemanticPlatform for DefaultedConstructorPlatform {}

    #[test]
    fn dependency_constructor_defaults_come_from_its_call_shape() {
        let identity = crate::types::type_name("fixture/Base");
        let mut constructor = LibraryMember::new(
            "<init>".to_owned(),
            vec![Ty::Int],
            Ty::Unit,
            "(I)V".to_owned(),
        );
        constructor.call_sig.param_defaults = vec![true];
        assert!(constructor.default_values.is_empty());
        let unknown = LibraryMember::new(
            "<init>".to_owned(),
            vec![Ty::String],
            Ty::Unit,
            "(Ljava/lang/String;)V".to_owned(),
        );
        assert!(unknown.call_sig.param_defaults.is_empty());
        let mut classifier = LibraryType::declaration_header();
        classifier.constructors.extend([constructor, unknown]);
        let platform = DefaultedConstructorPlatform {
            identity,
            classifier: Arc::new(classifier),
        };
        let mut diagnostics = crate::diag::DiagSink::new();
        let table = super::super::signature_collection::collect_signatures_with_cp(
            &[],
            Box::new(platform),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let headers = crate::fir::inventory_parsed_source_headers(&[], &[]);

        let constructors = constructors(&headers, &table, identity);
        let [defaulted, unknown] = constructors.as_slice() else {
            panic!("two dependency constructors")
        };
        assert_eq!(defaulted.parameter_defaults, [true]);
        assert_eq!(unknown.parameter_defaults, [false]);
    }
}
