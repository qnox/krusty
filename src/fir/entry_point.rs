//! Kotlin's program entry-point rule over one finalized top-level function header.
//!
//! The resolver applies this rule when it classifies top-level overload conflicts (an entry point
//! is file-local), and common lowering applies the same rule to publish each file's selected entry.
//! The rule genuinely names a declaration (`main`), so it is stated once, here.

use crate::types::Ty;

/// The parameter form of a Kotlin `main` entry point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainEntryParameters {
    /// `fun main()`.
    None,
    /// `fun main(args: Array<String>)`, including `vararg args: String`.
    Arguments,
}

/// The finalized header facts the entry-point rule reads from one top-level function.
#[derive(Clone, Copy, Debug)]
pub struct MainEntryShape<'a> {
    pub name: &'a str,
    pub has_extension_receiver: bool,
    pub type_parameter_count: usize,
    pub context_parameter_count: usize,
    pub parameters: &'a [Ty],
    pub result: Ty,
}

impl MainEntryShape<'_> {
    /// The entry-point form of this function, or `None` when Kotlin does not treat it as `main`:
    /// it must be named `main`, have no extension receiver, type parameters, or context
    /// parameters, return `Unit`, and take either nothing or one array of `String`.
    pub fn entry_parameters(&self) -> Option<MainEntryParameters> {
        if self.name != "main"
            || self.has_extension_receiver
            || self.type_parameter_count != 0
            || self.context_parameter_count != 0
            || self.result != Ty::Unit
        {
            return None;
        }
        match self.parameters {
            [] => Some(MainEntryParameters::None),
            [parameter] if parameter.array_read_elem() == Some(Ty::String) => {
                Some(MainEntryParameters::Arguments)
            }
            _ => None,
        }
    }
}
