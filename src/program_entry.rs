//! The program entry point, as a phase-neutral contract.
//!
//! The frontend selects each source unit's Kotlin `main` and records its form; common IR carries
//! that form to the backends. Neither phase owns the other's model, so the form lives here, beside
//! the other cross-phase declaration facts (see `context_parameters`).

/// The parameter form of a Kotlin `main` entry point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainEntryParameters {
    /// `fun main()`.
    None,
    /// `fun main(args: Array<String>)`, including `vararg args: String` and a nullable array.
    Arguments,
}
