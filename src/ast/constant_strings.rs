//! Folding of compile-time constant strings from parser syntax.

use super::{Decl, Expr, ExprId, File, TemplatePart};
use crate::kt_string::{KtString, KtStringBuf};

impl File {
    pub(crate) fn const_string_value(&self, expression: ExprId) -> Option<KtString> {
        self.const_string_value_at_depth(expression, 0)
    }

    fn const_string_value_at_depth(&self, expression: ExprId, depth: u32) -> Option<KtString> {
        if depth > 32 {
            return None;
        }
        // Pass-1 syntax compaction replaces expressions outside the retained inline/default
        // dependency closure with a non-arena sentinel. Constant probes are optional semantic
        // queries, so an absent node is simply not a foldable string; it must never index the
        // compacted arena.
        match self.expr_arena.get(expression.0 as usize)? {
            Expr::StringLit(value) => Some(value.clone()),
            Expr::CharLit(unit) => {
                let mut value = KtStringBuf::new();
                value.push_unit(*unit);
                Some(value.finish())
            }
            Expr::Name(name) => self.top_level_const_string_at_depth(name, depth + 1),
            Expr::Template(parts) => {
                let mut value = KtStringBuf::new();
                for part in parts {
                    match part {
                        TemplatePart::Str(text) => value.push_kt(text),
                        TemplatePart::Expr(expression) => value
                            .push_kt(&self.const_string_value_at_depth(*expression, depth + 1)?),
                    }
                }
                Some(value.finish())
            }
            _ => None,
        }
    }

    fn top_level_const_string_at_depth(&self, name: &str, depth: u32) -> Option<KtString> {
        if depth > 32 {
            return None;
        }
        self.decls
            .iter()
            .find_map(|&declaration| match self.decl(declaration) {
                Decl::Property(property) if property.name == name => {
                    property.init.and_then(|initializer| {
                        self.const_string_value_at_depth(initializer, depth + 1)
                    })
                }
                _ => None,
            })
    }
}
