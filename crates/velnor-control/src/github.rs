//! GitHub Actions application ports and run operations.
//!
//! The HTTP implementation is injected. This layer owns pagination bounds,
//! authoritative status checks, and ambiguity handling for mutations.

use velnor_model::{GithubArtifact, GithubJob, GithubRun, RepositoryRef};

use crate::ports::PortError;

const MAX_PAGE: u32 = 100;
const CANCELLABLE_STATUSES: [&str; 5] =
    ["queued", "in_progress", "requested", "waiting", "pending"];

/// Typed GitHub transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubError {
    /// Authentication or permission was denied.
    Authorization,
    /// The remote identity was absent.
    NotFound,
    /// The request could not be classified as committed or uncommitted.
    Ambiguous,
    /// The remote transport failed without a definitive mutation result.
    Transport,
    /// Upstream response violated the DTO contract.
    InvalidResponse,
}

impl From<GithubError> for PortError {
    fn from(error: GithubError) -> Self {
        match error {
            GithubError::Authorization => PortError::Authorization {
                operation: "GitHub API".to_owned(),
            },
            GithubError::NotFound => PortError::Unavailable {
                resource: "GitHub resource".to_owned(),
            },
            GithubError::Ambiguous | GithubError::Transport => PortError::Operation {
                operation: "GitHub transport outcome is not authoritative".to_owned(),
            },
            GithubError::InvalidResponse => PortError::Operation {
                operation: "GitHub response violated its schema".to_owned(),
            },
        }
    }
}

/// Injected GitHub API operations. Implementations own authentication and
/// current protocol headers; callers never receive credential values.
pub trait GithubApi: Send + Sync {
    /// Read one page of runs.
    fn list_runs(
        &self,
        repository: &RepositoryRef,
        page: u32,
    ) -> Result<Vec<GithubRun>, GithubError>;
    /// Read one run by its stable id.
    fn get_run(&self, repository: &RepositoryRef, run_id: u64) -> Result<GithubRun, GithubError>;
    /// Read one attempt's jobs.
    fn list_jobs(
        &self,
        repository: &RepositoryRef,
        run_id: u64,
        attempt: u32,
    ) -> Result<Vec<GithubJob>, GithubError>;
    /// Read artifact metadata.
    fn list_artifacts(
        &self,
        repository: &RepositoryRef,
        run_id: u64,
    ) -> Result<Vec<GithubArtifact>, GithubError>;
    /// Request cancellation; the caller must inspect authoritative state first.
    fn cancel_run(&self, repository: &RepositoryRef, run_id: u64) -> Result<(), GithubError>;
    /// Request rerun and return the exact new run id from the response.
    fn rerun(&self, repository: &RepositoryRef, run_id: u64) -> Result<GithubRun, GithubError>;
    /// Dispatch and return the exact run response, never a list-difference guess.
    fn dispatch(
        &self,
        repository: &RepositoryRef,
        workflow: &str,
        reference: &str,
    ) -> Result<GithubRun, GithubError>;
}

/// API-backed GitHub run service.
pub struct GithubService<C> {
    client: C,
}

impl<C> GithubService<C> {
    /// Wrap one injected API client.
    #[must_use]
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

impl<C: GithubApi> GithubService<C> {
    /// Read a bounded page.
    pub fn list_runs(
        &self,
        repository: &RepositoryRef,
        page: u32,
    ) -> Result<Vec<GithubRun>, PortError> {
        if page == 0 || page > MAX_PAGE {
            return Err(PortError::Invalid {
                field: "page".to_owned(),
                message: format!("must be between 1 and {MAX_PAGE}"),
            });
        }
        self.client.list_runs(repository, page).map_err(Into::into)
    }

    /// Cancel only after authoritative state inspection.
    pub fn cancel_run(&self, repository: &RepositoryRef, run_id: u64) -> Result<bool, PortError> {
        let run = self
            .client
            .get_run(repository, run_id)
            .map_err(PortError::from)?;
        match run.status.as_str() {
            "completed" => return Ok(false),
            status if CANCELLABLE_STATUSES.contains(&status) => {}
            _ => return Err(GithubError::InvalidResponse.into()),
        }
        self.client
            .cancel_run(repository, run_id)
            .map_err(PortError::from)?;
        Ok(true)
    }

    /// Return the exact upstream rerun identity.
    pub fn rerun(&self, repository: &RepositoryRef, run_id: u64) -> Result<GithubRun, PortError> {
        self.client.rerun(repository, run_id).map_err(Into::into)
    }

    /// Return the exact upstream dispatch identity.
    pub fn dispatch(
        &self,
        repository: &RepositoryRef,
        workflow: &str,
        reference: &str,
    ) -> Result<GithubRun, PortError> {
        if workflow.trim().is_empty() || reference.trim().is_empty() {
            return Err(PortError::Invalid {
                field: "workflow/reference".to_owned(),
                message: "both values are required".to_owned(),
            });
        }
        self.client
            .dispatch(repository, workflow, reference)
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_model::{GithubRun, RepositoryRef, Timestamp};

    struct Fake {
        run: GithubRun,
        canceled: std::sync::Mutex<u32>,
    }

    impl GithubApi for Fake {
        fn list_runs(&self, _: &RepositoryRef, _: u32) -> Result<Vec<GithubRun>, GithubError> {
            Ok(vec![self.run.clone()])
        }
        fn get_run(&self, _: &RepositoryRef, _: u64) -> Result<GithubRun, GithubError> {
            Ok(self.run.clone())
        }
        fn list_jobs(
            &self,
            _: &RepositoryRef,
            _: u64,
            _: u32,
        ) -> Result<Vec<GithubJob>, GithubError> {
            Ok(Vec::new())
        }
        fn list_artifacts(
            &self,
            _: &RepositoryRef,
            _: u64,
        ) -> Result<Vec<GithubArtifact>, GithubError> {
            Ok(Vec::new())
        }
        fn cancel_run(&self, _: &RepositoryRef, _: u64) -> Result<(), GithubError> {
            *self.canceled.lock().expect("counter") += 1;
            Ok(())
        }
        fn rerun(&self, _: &RepositoryRef, _: u64) -> Result<GithubRun, GithubError> {
            Ok(self.run.clone())
        }
        fn dispatch(&self, _: &RepositoryRef, _: &str, _: &str) -> Result<GithubRun, GithubError> {
            Ok(self.run.clone())
        }
    }

    fn run(status: &str) -> GithubRun {
        GithubRun {
            id: 7,
            repository: RepositoryRef::new("tailrocks", "velnor"),
            number: 8,
            attempt: 1,
            workflow: "ci.yml".to_owned(),
            head_sha: "abc".to_owned(),
            head_branch: "main".to_owned(),
            event: "push".to_owned(),
            status: status.to_owned(),
            conclusion: None,
            url: None,
            observed_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn completed_run_is_not_cancelled_again() {
        let fake = Fake {
            run: run("completed"),
            canceled: std::sync::Mutex::new(0),
        };
        let service = GithubService::new(fake);
        assert!(!service
            .cancel_run(&RepositoryRef::new("tailrocks", "velnor"), 7)
            .expect("cancel check"));
    }

    #[test]
    fn unknown_run_status_fails_closed_without_cancelling() {
        let fake = Fake {
            run: run("mysterious"),
            canceled: std::sync::Mutex::new(0),
        };
        let service = GithubService::new(fake);
        let result = service.cancel_run(&RepositoryRef::new("tailrocks", "velnor"), 7);

        assert_eq!(result, Err(PortError::from(GithubError::InvalidResponse)));
        assert_eq!(*service.client.canceled.lock().expect("counter"), 0);
    }

    #[test]
    fn malformed_run_status_fails_closed_without_cancelling() {
        let fake = Fake {
            run: run(" in_progress "),
            canceled: std::sync::Mutex::new(0),
        };
        let service = GithubService::new(fake);
        let result = service.cancel_run(&RepositoryRef::new("tailrocks", "velnor"), 7);

        assert_eq!(result, Err(PortError::from(GithubError::InvalidResponse)));
        assert_eq!(*service.client.canceled.lock().expect("counter"), 0);
    }

    #[test]
    fn known_cancellable_status_is_cancelled() {
        let fake = Fake {
            run: run("in_progress"),
            canceled: std::sync::Mutex::new(0),
        };
        let service = GithubService::new(fake);

        assert!(service
            .cancel_run(&RepositoryRef::new("tailrocks", "velnor"), 7)
            .expect("cancel check"));
        assert_eq!(*service.client.canceled.lock().expect("counter"), 1);
    }
}
