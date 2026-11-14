//! Literal text the runtime's renderers emit.

use super::encode::{Code, Module};
use super::runtime::Runtime;

impl Runtime {
    /// Push the string constant `literal`.
    pub(super) fn text(&mut self, module: &mut Module, code: &mut Code, literal: &str) {
        let units = literal.encode_utf16().collect::<Vec<_>>();
        self.string_constant(module, code, &units);
    }
}
