//! Parsing of `+CompanionBlocksAndExtensions` declarations: the top-level `companion` modifier of a
//! companion extension, `companion { … }` blocks, and the lookahead that tells a companion block
//! from a `companion object`.
//!
//! A block introduces no singleton classifier: each member is recorded as a companion-associated
//! declaration whose receiver names the classifier that declared the block, the same associated
//! representation a written `companion fun C.name` has.

use super::*;

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
                tail.extend(self.skip_decl_prefix());
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
    /// Parse a `companion { ... }` block as associated declarations on its containing classifier.
    /// Unlike `companion object`, a block introduces no singleton classifier: its members use the
    /// same receiver-less associated-call representation as `companion fun/val C.name`.
    pub(super) fn parse_companion_block(&mut self, outer: &str, modifiers: &[String]) {
        let start = self.tok().span;
        self.bump(); // `companion`
        self.expect(TokenKind::LBrace, "'{'");
        loop {
            self.skip_newlines();
            if matches!(self.kind(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            let mut member_modifiers = self.parse_member_decl_prefix();
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
                }
                TokenKind::KwVal | TokenKind::KwVar => {
                    let lateinit = member_modifiers
                        .iter()
                        .any(|modifier| modifier == "lateinit");
                    let mut property = self.parse_top_property_c(
                        lateinit,
                        false,
                        member_modifiers.iter().any(|modifier| modifier == "const"),
                        false,
                    );
                    property.receiver = Some(receiver());
                    property.visibility = visibility_of(&member_modifiers);
                    property.is_open = !member_modifiers.iter().any(|modifier| modifier == "final")
                        && member_modifiers
                            .iter()
                            .any(|modifier| modifier == "open" || modifier == "override");
                    property.is_override = member_modifiers
                        .iter()
                        .any(|modifier| modifier == "override");
                    property.is_external = member_modifiers
                        .iter()
                        .any(|modifier| modifier == "external");
                    property.is_expect =
                        member_modifiers.iter().any(|modifier| modifier == "expect");
                    property.is_actual =
                        member_modifiers.iter().any(|modifier| modifier == "actual");
                    property.is_companion_extension = true;
                    property.is_companion_block_member = true;
                    let declaration = self.file.add_decl(Decl::Property(property));
                    self.file.decls.push(declaration);
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
