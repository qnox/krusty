//! Transaction ownership for provisional postponed-argument signature evaluation.

/// Semantic state published by a provisional argument must commit or roll back with the graph-walk
/// memo that produced it. Implementations without graph-local side effects use the default no-op
/// transaction.
pub trait PostponedArgumentTransaction {
    fn begin_postponed_argument_transaction(&self) {}

    fn commit_postponed_argument_transaction(&self) {}

    fn rollback_postponed_argument_transaction(&self) {}
}
