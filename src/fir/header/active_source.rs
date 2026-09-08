//! Short-lived parser-to-stable declaration binding for one Pass-1 source.

use std::collections::HashMap;

use crate::ast::{DeclId, File};

use super::{
    DeclarationFlags, DeclarationId, DeclarationIds, DeclarationKind, DeclarationStub,
    ExtractedFileStubs, SourceFileId,
};

/// Stable headers extracted from one parser unit, plus its short-lived parser-to-header binding.
/// The lifetime prevents the binding from escaping the active `File`; only `stubs` enter the
/// finished module.
pub(crate) struct ActiveSourceHeaders<'file> {
    source: SourceFileId,
    stubs: Vec<DeclarationStub>,
    primary_declarations: Vec<Option<DeclarationId>>,
    type_parameter_declarations: HashMap<u32, DeclarationId>,
    enclosing_classifiers: HashMap<DeclarationId, DeclId>,
    direct_classifier_children: HashMap<DeclId, Vec<DeclId>>,
    classifier_members: HashMap<(DeclId, ActiveClassifierMemberKind, u32), DeclarationId>,
    declaration_syntax: HashMap<DeclarationId, ActiveDeclarationSyntax>,
    active_file: std::marker::PhantomData<&'file File>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ActiveClassifierMemberKind {
    PrimaryConstructor,
    SecondaryConstructor,
    ConstructorProperty,
    Method,
    BodyProperty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveDeclarationSyntax {
    Primary(DeclId),
    ClassifierMethod(DeclId, u32),
    ClassifierProperty(DeclId, u32),
    EnumEntryMethod(DeclId, u32, u32),
    EnumEntryProperty(DeclId, u32, u32),
}

impl<'file> ActiveSourceHeaders<'file> {
    pub(super) fn bind(
        source: SourceFileId,
        file: &'file File,
        declarations: &DeclarationIds,
        extracted: ExtractedFileStubs,
        type_parameter_declarations: HashMap<u32, DeclarationId>,
    ) -> Self {
        let classifier_parsers = extracted
            .primary_declarations
            .iter()
            .enumerate()
            .filter_map(|(raw, declaration)| {
                let declaration = (*declaration)?;
                declarations
                    .stable_anchor(declaration)
                    .is_some_and(|anchor| anchor.kind == DeclarationKind::Classifier)
                    .then(|| {
                        (
                            declaration,
                            DeclId(u32::try_from(raw).expect("too many parser declarations")),
                        )
                    })
            })
            .collect::<HashMap<_, _>>();
        let enclosing_classifier = |declaration: DeclarationId| {
            let mut owner = declarations
                .stable_anchor(declaration)
                .and_then(|anchor| anchor.owner);
            while let Some(candidate) = owner {
                let anchor = declarations
                    .stable_anchor(candidate)
                    .expect("a declaration owner must have a stable anchor");
                if anchor.kind == DeclarationKind::Classifier {
                    if let Some(&parser) = classifier_parsers.get(&candidate) {
                        return Some(parser);
                    }
                }
                owner = anchor.owner;
            }
            None
        };
        let enclosing_classifiers = extracted
            .stubs
            .iter()
            .filter_map(|stub| enclosing_classifier(stub.id).map(|parser| (stub.id, parser)))
            .collect();
        let mut direct_classifier_children = HashMap::new();
        for (&declaration, &parser) in &classifier_parsers {
            let Some(parent) = enclosing_classifier(declaration) else {
                continue;
            };
            direct_classifier_children
                .entry(parent)
                .or_insert_with(Vec::new)
                .push(parser);
        }
        let stubs_by_declaration = extracted
            .stubs
            .iter()
            .map(|stub| (stub.id, stub))
            .collect::<HashMap<_, _>>();
        let mut classifier_members = HashMap::new();
        let mut declaration_syntax = extracted
            .primary_declarations
            .iter()
            .enumerate()
            .filter_map(|(raw, declaration)| {
                let declaration = (*declaration)?;
                let parser = DeclId(u32::try_from(raw).expect("too many parser declarations"));
                matches!(
                    file.decl(parser),
                    crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_)
                )
                .then_some((declaration, ActiveDeclarationSyntax::Primary(parser)))
            })
            .collect::<HashMap<_, _>>();
        for (&declaration, &parser) in &classifier_parsers {
            for &member in declarations.owned(declaration) {
                let anchor = declarations
                    .stable_anchor(member)
                    .expect("an owned declaration must have a stable anchor");
                let Some((kind, ordinal)) = (match anchor.kind {
                    DeclarationKind::Constructor if anchor.sibling == 0 => {
                        Some((ActiveClassifierMemberKind::PrimaryConstructor, 0))
                    }
                    DeclarationKind::Constructor => Some((
                        ActiveClassifierMemberKind::SecondaryConstructor,
                        anchor.sibling - 1,
                    )),
                    DeclarationKind::Function => {
                        Some((ActiveClassifierMemberKind::Method, anchor.sibling))
                    }
                    DeclarationKind::Property => stubs_by_declaration.get(&member).map(|stub| {
                        let kind = if stub.flags.has(DeclarationFlags::PROPERTY_PARAMETER) {
                            ActiveClassifierMemberKind::ConstructorProperty
                        } else {
                            ActiveClassifierMemberKind::BodyProperty
                        };
                        (kind, anchor.sibling)
                    }),
                    DeclarationKind::Classifier
                    | DeclarationKind::EnumEntry
                    | DeclarationKind::TypeAlias
                    | DeclarationKind::Accessor
                    | DeclarationKind::Initializer
                    | DeclarationKind::Script => None,
                }) else {
                    continue;
                };
                assert!(
                    classifier_members
                        .insert((parser, kind, ordinal), member)
                        .is_none(),
                    "one parser classifier member must bind to one stable declaration",
                );
                let syntax = match kind {
                    ActiveClassifierMemberKind::Method => {
                        Some(ActiveDeclarationSyntax::ClassifierMethod(parser, ordinal))
                    }
                    ActiveClassifierMemberKind::BodyProperty => {
                        Some(ActiveDeclarationSyntax::ClassifierProperty(parser, ordinal))
                    }
                    ActiveClassifierMemberKind::PrimaryConstructor
                    | ActiveClassifierMemberKind::SecondaryConstructor
                    | ActiveClassifierMemberKind::ConstructorProperty => None,
                };
                if let Some(syntax) = syntax {
                    declaration_syntax.insert(member, syntax);
                }
            }
            let crate::ast::Decl::Class(class) = file.decl(parser) else {
                unreachable!("a classifier parser binding must name a class declaration")
            };
            for entry_ordinal in 0..class.enum_entries.len() {
                let entry_ordinal = u32::try_from(entry_ordinal).expect("too many enum entries");
                let Some(entry) = declarations
                    .owned(declaration)
                    .iter()
                    .copied()
                    .find(|entry| {
                        declarations.stable_anchor(*entry).is_some_and(|anchor| {
                            anchor.kind == DeclarationKind::EnumEntry
                                && anchor.sibling == entry_ordinal
                        })
                    })
                else {
                    continue;
                };
                for &member in declarations.owned(entry) {
                    let anchor = declarations
                        .stable_anchor(member)
                        .expect("an enum-entry member must have a stable anchor");
                    let syntax = match anchor.kind {
                        DeclarationKind::Function => ActiveDeclarationSyntax::EnumEntryMethod(
                            parser,
                            entry_ordinal,
                            anchor.sibling,
                        ),
                        DeclarationKind::Property => ActiveDeclarationSyntax::EnumEntryProperty(
                            parser,
                            entry_ordinal,
                            anchor.sibling,
                        ),
                        DeclarationKind::Classifier
                        | DeclarationKind::EnumEntry
                        | DeclarationKind::TypeAlias
                        | DeclarationKind::Constructor
                        | DeclarationKind::Accessor
                        | DeclarationKind::Initializer
                        | DeclarationKind::Script => continue,
                    };
                    declaration_syntax.insert(member, syntax);
                }
            }
        }
        Self {
            source,
            stubs: extracted.stubs,
            primary_declarations: extracted.primary_declarations,
            type_parameter_declarations,
            enclosing_classifiers,
            direct_classifier_children,
            classifier_members,
            declaration_syntax,
            active_file: std::marker::PhantomData,
        }
    }

    pub(crate) const fn source(&self) -> SourceFileId {
        self.source
    }

    pub(crate) fn stubs(&self) -> &[DeclarationStub] {
        &self.stubs
    }

    pub(crate) fn declaration(&self, parser: DeclId) -> Option<DeclarationId> {
        self.primary_declarations
            .get(parser.0 as usize)
            .copied()
            .flatten()
    }

    pub(crate) fn declarations(&self) -> impl Iterator<Item = (DeclId, DeclarationId)> + '_ {
        self.primary_declarations
            .iter()
            .enumerate()
            .filter_map(|(raw, declaration)| {
                let parser = DeclId(u32::try_from(raw).expect("too many parser declarations"));
                declaration.map(|stable| (parser, stable))
            })
    }

    pub(crate) fn type_parameter_declaration(&self, signature_start: u32) -> Option<DeclarationId> {
        self.type_parameter_declarations
            .get(&signature_start)
            .copied()
    }

    pub(crate) fn enclosing_classifier(&self, declaration: DeclarationId) -> Option<DeclId> {
        self.enclosing_classifiers.get(&declaration).copied()
    }

    pub(crate) fn direct_classifier_children(&self) -> &HashMap<DeclId, Vec<DeclId>> {
        &self.direct_classifier_children
    }

    fn classifier_member(
        &self,
        classifier: DeclId,
        kind: ActiveClassifierMemberKind,
        ordinal: u32,
    ) -> Option<DeclarationId> {
        self.classifier_members
            .get(&(classifier, kind, ordinal))
            .copied()
    }

    pub(crate) fn primary_constructor(&self, classifier: DeclId) -> Option<DeclarationId> {
        self.classifier_member(
            classifier,
            ActiveClassifierMemberKind::PrimaryConstructor,
            0,
        )
    }

    pub(crate) fn secondary_constructor(
        &self,
        classifier: DeclId,
        ordinal: u32,
    ) -> Option<DeclarationId> {
        self.classifier_member(
            classifier,
            ActiveClassifierMemberKind::SecondaryConstructor,
            ordinal,
        )
    }

    pub(crate) fn constructor_property(
        &self,
        classifier: DeclId,
        ordinal: u32,
    ) -> Option<DeclarationId> {
        self.classifier_member(
            classifier,
            ActiveClassifierMemberKind::ConstructorProperty,
            ordinal,
        )
    }

    pub(crate) fn method(&self, classifier: DeclId, ordinal: u32) -> Option<DeclarationId> {
        self.classifier_member(classifier, ActiveClassifierMemberKind::Method, ordinal)
    }

    pub(crate) fn body_property(&self, classifier: DeclId, ordinal: u32) -> Option<DeclarationId> {
        self.classifier_member(
            classifier,
            ActiveClassifierMemberKind::BodyProperty,
            ordinal,
        )
    }

    pub(crate) fn function(
        &self,
        file: &'file File,
        declaration: DeclarationId,
    ) -> Option<&'file crate::ast::FunDecl> {
        match *self.declaration_syntax.get(&declaration)? {
            ActiveDeclarationSyntax::Primary(parser) => match file.decl(parser) {
                crate::ast::Decl::Fun(function) => Some(function),
                crate::ast::Decl::Class(_) | crate::ast::Decl::Property(_) => None,
            },
            ActiveDeclarationSyntax::ClassifierMethod(classifier, ordinal) => {
                let crate::ast::Decl::Class(class) = file.decl(classifier) else {
                    return None;
                };
                class.methods.get(ordinal as usize)
            }
            ActiveDeclarationSyntax::EnumEntryMethod(classifier, entry, ordinal) => {
                let crate::ast::Decl::Class(class) = file.decl(classifier) else {
                    return None;
                };
                class
                    .enum_entries
                    .get(entry as usize)?
                    .methods
                    .get(ordinal as usize)
            }
            ActiveDeclarationSyntax::ClassifierProperty(_, _)
            | ActiveDeclarationSyntax::EnumEntryProperty(_, _, _) => None,
        }
    }

    pub(crate) fn property(
        &self,
        file: &'file File,
        declaration: DeclarationId,
    ) -> Option<&'file crate::ast::PropDecl> {
        match *self.declaration_syntax.get(&declaration)? {
            ActiveDeclarationSyntax::Primary(parser) => match file.decl(parser) {
                crate::ast::Decl::Property(property) => Some(property),
                crate::ast::Decl::Class(_) | crate::ast::Decl::Fun(_) => None,
            },
            ActiveDeclarationSyntax::ClassifierProperty(classifier, ordinal) => {
                let crate::ast::Decl::Class(class) = file.decl(classifier) else {
                    return None;
                };
                class.body_props.get(ordinal as usize)
            }
            ActiveDeclarationSyntax::EnumEntryProperty(classifier, entry, ordinal) => {
                let crate::ast::Decl::Class(class) = file.decl(classifier) else {
                    return None;
                };
                class
                    .enum_entries
                    .get(entry as usize)?
                    .props
                    .get(ordinal as usize)
            }
            ActiveDeclarationSyntax::ClassifierMethod(_, _)
            | ActiveDeclarationSyntax::EnumEntryMethod(_, _, _) => None,
        }
    }
}
