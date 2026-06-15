use thiserror::Error;

#[derive(Error, Debug)]
pub enum TransactionAcqError {
    #[error("Failed to acquire read-only transaction. Drop read-write transaction and retry.")]
    SharedAcqFailed,
    #[error("Failed to acquire read-write transaction. Drop other transactions and retry.")]
    ExclusiveAcqFailed,
    #[error("All references to a parent document containing this structure has been dropped.")]
    DocumentDropped,
}
