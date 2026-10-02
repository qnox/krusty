//! Parsing of `+CompanionBlocksAndExtensions` declarations: the top-level `companion` modifier of a
//! companion extension, `companion { … }` blocks, and the lookahead that tells a companion block
//! from a `companion object`.
//!
//! A block introduces no singleton classifier: each member is recorded as a companion-associated
//! declaration whose receiver names the classifier that declared the block, the same associated
//! representation a written `companion fun C.name` has.

use super::*;
use crate::ast::CompanionBlockMember;

/// What a classifier body's `companion` declarations contribute to the classifier: its
/// `companion object`, and the members its `companion { … }` blocks introduced, in source order.
#[derive(Default)]
pub(super) struct ClassifierCompanions {
    pub(super) object: Option<DeclId>,
    pub(super) block_members: Vec<CompanionBlockMember>,
}

impl Parser<'_> {
    /// `+CompanionBlocksAndExtensions`: `companion fun/val/var C.member …` is a real top-level
    /// declaration modifier. It is intentionally consumed only at file scope; inside a classifier,
    /// `companion object` and `companion { … }` select member grammar productions and must remain
    /// visible to that dispatcher.
    pub(super) fn take_top_level_companion_modifier(&mut self, mods: &mut Vec<String>) {
        if !(self.at(TokenKind::Ident) && self.keyword_text("companion")) {
            return;
        }
        let save = self.i;
        self.bump(); // `companion`
        let mut tail = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::At) || self.at_modifier() {
                tail.extend(self.extend_decl_prefix());
                continue;
            }
            let before_context = self.i;
            tail.extend(self.maybe_parse_context_receivers());
            if self.i != before_context {
                continue;
            }
            break;
        }
        if matches!(
            self.kind(),
            TokenKind::KwFun | TokenKind::KwVal | TokenKind::KwVar
        ) {
            mods.push("companion".to_string());
            mods.append(&mut tail);
        } else {
            self.i = save;
        }
    }

    pub(super) fn at_companion_declaration(&self) -> bool {
        self.at(TokenKind::Ident)
            && self.keyword_text("companion")
            && self.t.get(self.i + 1).is_some_and(|token| {
                token.kind == TokenKind::LBrace || self.token_keyword_text(*token, "object")
            })
    }
    pub(super) fn at_companion_object_declaration(&self) -> bool {
        self.at(TokenKind::Ident)
            && self.keyword_text("companion")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| self.token_keyword_text(*token, "object"))
    }
    /// Parse the `companion object` or `companion { … }` block at the cursor in the body of the
    /// classifier `outer`, recording what it contributes in `companions`.
    pub(super) fn parse_classifier_companion(
        &mut self,
        outer: &str,
        modifiers: &[String],
        companions: &mut ClassifierCompanions,
    ) {
        if self.at_companion_object_declaration() {
            companions.object = Some(self.parse_companion(outer, modifiers));
        } else {
            self.parse_companion_block(outer, modifiers, &mut companions.block_members);
        }
    }

    /// Parse a `companion { ... }` block as associated declarations on its containing classifier.
    /// Unlike `companion object`, a block introduces no singleton classifier: its members use the
    /// same receiver-less associated-call representation as `companion fun/val C.name`, and are
    /// appended to `members`, the classifier's record of them.
    fn parse_companion_block(
        &mut self,
        outer: &str,
        modifiers: &[String],
        members: &mut Vec<CompanionBlockMember>,
    ) {
        let start = self.tok().span;
        self.bump(); // `companion`
        self.expect(TokenKind::LBrace, "'{'");
        loop {
            self.skip_newlines();
            if matches!(self.kind(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            let mut member_modifiers = self.parse_member_decl_prefix();
            // The member's own `private`, not one the block itself carries.
            let private_modifier = declaration_modifiers::span(self, &member_modifiers, "private");
            // Likewise the member's own `actual`: it is hoisted to a file declaration, and only
            // what it wrote itself claims an `expect`.
            let own_modifiers = member_modifiers.clone();
            member_modifiers.extend(modifiers.iter().cloned());
            member_modifiers.push("companion".to_string());
            let receiver = || TypeRef {
                name: outer.to_string(),
                flags: TrFlags::default(),
                arg: None,
                targs: Vec::new(),
                span: start,
                fun_params: Vec::new(),
                fun_context_count: 0,
            };
            match self.kind() {
                TokenKind::KwFun => {
                    let mut function = self.parse_fun(&member_modifiers);
                    function.receiver = Some(receiver());
                    function.flags = function.flags.with_is_companion_block_member(true);
                    let declaration = self.file.add_decl(Decl::Fun(function));
                    self.file.decls.push(declaration);
                    declaration_modifiers::record_nested_actual(
                        &mut self.file,
                        &own_modifiers,
                        declaration,
                    );
                    members.push(CompanionBlockMember {
                        declaration,
                        private_modifier,
                    });
                }
                TokenKind::KwVal | TokenKind::KwVar => {
                    let mut property = self.parse_member_property(&member_modifiers, false, false);
                    property.receiver = Some(receiver());
                    property.is_companion_extension = true;
                    property.is_companion_block_member = true;
                    let declaration = self.file.add_decl(Decl::Property(property));
                    self.file.decls.push(declaration);
                    declaration_modifiers::record_nested_actual(
                        &mut self.file,
                        &own_modifiers,
                        declaration,
                    );
                    members.push(CompanionBlockMember {
                        declaration,
                        private_modifier,
                    });
                }
                _ => {
                    self.diags.error(
                        self.tok().span,
                        "expected a companion-block member declaration",
                    );
                    self.bump();
                }
            }
        }
        self.expect(TokenKind::RBrace, "'}'");
    }

    /// A companion-block member is hoisted as a companion extension of the class whose block
    /// declared it, so when that class is hoisted under `outer` its receiver names the class by the
    /// same lexical path. Returns whether `declaration` was such a member.
    pub(super) fn reprefix_companion_receiver(&mut self, declaration: DeclId, outer: &str) -> bool {
        let receiver = match self.file.decl_mut(declaration) {
            Decl::Fun(function) if function.is_companion_extension() => function.receiver.as_mut(),
            Decl::Property(property) if property.is_companion_extension => {
                property.receiver.as_mut()
            }
            _ => return false,
        };
        if let Some(receiver) = receiver {
            receiver.name = format!("{outer}.{}", receiver.name);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    #[test]
    fn companion_and_context_prefixes_preserve_all_annotations() {
        let source = "annotation class Before\n\
            annotation class After\n\
            class Ambient\n\
            class Coordinate\n\
            @Before context(_: Ambient) @After companion public fun Coordinate.first() = Unit\n\
            companion @Before public context(_: Ambient) @After val Coordinate.second get() = Unit\n";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert_eq!(
            diagnostics.render("test.kt", source),
            "",
            "prefix forms must parse without diagnostics"
        );

        let annotations = file
            .decls
            .iter()
            .filter_map(|&declaration| match file.decl(declaration) {
                Decl::Fun(function) if function.name == "first" => Some((
                    function.name.as_str(),
                    function
                        .annotations
                        .iter()
                        .map(|annotation| annotation.name.as_str())
                        .collect::<Vec<_>>(),
                )),
                Decl::Property(property) if property.name == "second" => Some((
                    property.name.as_str(),
                    property
                        .annotations
                        .iter()
                        .map(|annotation| annotation.name.as_str())
                        .collect::<Vec<_>>(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            annotations,
            vec![
                ("first", vec!["Before", "After"]),
                ("second", vec!["Before", "After"]),
            ]
        );
    }
}
