//! The value tree build files read into, and the phases over it: reading a file against the
//! schema, refining a module's files into the values seen from one fragment, resolving default
//! references, and completing the result.

mod complete;
mod contexts;
mod node;
mod parse;
mod references;
mod refine;

pub use complete::complete;
pub use contexts::{Contexts, FileOrder};
pub use node::{Entry, FileId, Files, Node, Trace, Value};
pub use parse::{read, Paths};
pub use references::resolve;
pub use refine::{Conflict, Refiner};
