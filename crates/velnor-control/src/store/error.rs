//! Store failures surfaced as [`MachineErrorEnvelope`] values.

use std::fmt;

use velnor_model::{ExitClass, MachineErrorEnvelope};

/// Every store failure carries the machine envelope; no bare strings cross
/// the API boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    /// Machine envelope describing the failure class and stable reason.
    pub envelope: MachineErrorEnvelope,
}

impl StoreError {
    #[must_use]
    pub fn new(class: ExitClass, reason: &str) -> Self {
        Self {
            envelope: MachineErrorEnvelope::new(class.as_str(), class.code(), reason),
        }
    }

    #[must_use]
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.envelope.remediation = Some(remediation.into());
        self
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "store failure class={} code={} reason={}",
            self.envelope.class, self.envelope.code, self.envelope.reason
        )?;
        if let Some(remediation) = &self.envelope.remediation {
            write!(f, " remediation={remediation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(inner, _)
                if inner.code == rusqlite::ErrorCode::DatabaseBusy
                    || inner.code == rusqlite::ErrorCode::DatabaseLocked =>
            {
                Self::new(ExitClass::Timeout, "store.locked")
                    .with_remediation("retry once the competing writer releases the database")
            }
            rusqlite::Error::QueryReturnedNoRows => {
                Self::new(ExitClass::Unavailable, "store.row.missing")
            }
            _ => Self::new(ExitClass::Operation, "store.operation.failed")
                .with_remediation(error.to_string()),
        }
    }
}

/// Result alias for every store operation.
pub type StoreResult<T> = Result<T, StoreError>;
