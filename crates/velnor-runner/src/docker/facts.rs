//! Fact lifetimes.
//!
//! A fact learned about the host or the Engine is only reusable for as long as
//! the thing that produced it has not changed. Before this module every such
//! fact was treated alike: `docker info` was re-fetched once per job even
//! though its answer changes only when the daemon restarts, while the cgroup
//! boundary proof was thrown away by any non-zero `docker` exit — including an
//! ordinary failing user step, which cannot change a cgroup driver.
//!
//! The model is four lifetimes, each with the generation that invalidates it:
//!
//! | lifetime | invalidated by | key |
//! |---|---|---|
//! | [`FactLifetime::Host`] | the witness file the fact was derived from changing | [`host`] |
//! | [`FactLifetime::Daemon`] | `dockerd` restarting | [`daemon`] |
//! | [`FactLifetime::Image`] | the image id changing | [`image`] |
//! | [`FactLifetime::Job`] | the job ending | [`job`] |
//!
//! The rule that makes this safe: **a fact whose generation cannot be observed
//! is not cached.** [`host`] and [`daemon`] return `None` when their key cannot
//! be read — a missing drop-in, a daemon that is not the local systemd-managed
//! `dockerd`, a host without `/proc` — and [`Fact::get_or_try_init`] then
//! recomputes every time. Caching without an invalidation signal is how a fact
//! becomes quietly wrong, so it is not offered.

use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

/// How long a fact stays true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactLifetime {
    /// True until something on the host changes it. The witness is a concrete
    /// path whose modification time is the generation.
    Host,
    /// True until `dockerd` restarts.
    Daemon,
    /// True for one image id.
    Image,
    /// True for one job.
    Job,
}

impl FactLifetime {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Daemon => "daemon",
            Self::Image => "image",
            Self::Job => "job",
        }
    }
}

impl fmt::Display for FactLifetime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// The generation a cached fact is valid for.
///
/// A key compares equal only within the same lifetime, so a host generation can
/// never be mistaken for a daemon generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactKey {
    pub lifetime: FactLifetime,
    token: String,
}

impl FactKey {
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// Host generation witnessed by a file's modification time.
///
/// Returns `None` when the witness cannot be stat'ed: with no witness there is
/// no invalidation signal, so the fact must not be cached.
#[must_use]
pub fn host(witness: &Path) -> Option<FactKey> {
    let modified = std::fs::metadata(witness)
        .and_then(|metadata| metadata.modified())
        .ok()?;
    let since_epoch = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(FactKey {
        lifetime: FactLifetime::Host,
        token: format!("{}.{}", since_epoch.as_secs(), since_epoch.subsec_nanos()),
    })
}

/// Identity of the running `dockerd` generation.
///
/// Built from two host observations and no Engine round trip, because the point
/// of the key is to avoid asking the Engine:
///
/// * the daemon's pid and its kernel start time, read from `/proc`, which
///   together are unique per daemon process — a restart always changes one;
/// * the API socket's device, inode and modification time, which changes when a
///   daemon that is not socket-activated recreates it.
///
/// Returns `None` when the daemon is not a local process this host can see —
/// no pidfile, no `/proc`, a remote or VM-backed Engine. A daemon whose restart
/// cannot be observed has no invalidation signal, so its facts are not cached.
#[must_use]
pub fn daemon() -> Option<FactKey> {
    daemon_from(
        Path::new(DOCKER_PIDFILE),
        Path::new(crate::docker_lease::HOST_DOCKER_SOCKET),
    )
}

const DOCKER_PIDFILE: &str = "/var/run/docker.pid";
const DOCKER_PROCESS_NAME: &str = "dockerd";

fn daemon_from(pidfile: &Path, socket: &Path) -> Option<FactKey> {
    let pid = std::fs::read_to_string(pidfile)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    if comm.trim() != DOCKER_PROCESS_NAME {
        return None;
    }
    let start_time = process_start_time(pid)?;
    let socket_identity = socket_identity(socket)?;
    Some(FactKey {
        lifetime: FactLifetime::Daemon,
        token: format!("{pid}.{start_time}.{socket_identity}"),
    })
}

/// Field 22 of `/proc/<pid>/stat`: process start time in clock ticks since
/// boot. Parsed after the last `)` because the second field is a comm that may
/// itself contain spaces and parentheses.
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    // Field 3 (state) is the first entry after the comm, so start time (field
    // 22) is at offset 19 here.
    after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()
}

fn socket_identity(socket: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(socket).ok()?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |since_epoch| since_epoch.as_secs());
        Some(format!("{}.{}.{modified}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        None
    }
}

/// Generation of an image-derived fact: the resolved image id.
#[must_use]
pub fn image(image_id: &str) -> FactKey {
    FactKey {
        lifetime: FactLifetime::Image,
        token: image_id.to_string(),
    }
}

/// Generation of a job-scoped fact.
#[must_use]
pub fn job(job_id: &str) -> FactKey {
    FactKey {
        lifetime: FactLifetime::Job,
        token: job_id.to_string(),
    }
}

/// A value cached against the generation that can invalidate it.
///
/// `Fact` is deliberately not a general-purpose memo: the only way to read it
/// is to supply the current generation, so a caller cannot accidentally consume
/// a stale value, and supplying `None` (generation unobservable) forces a fresh
/// computation.
pub struct Fact<T> {
    name: &'static str,
    lifetime: FactLifetime,
    state: Mutex<Option<(FactKey, T)>>,
}

impl<T: Clone> Fact<T> {
    #[must_use]
    pub const fn new(name: &'static str, lifetime: FactLifetime) -> Self {
        Self {
            name,
            lifetime,
            state: Mutex::new(None),
        }
    }

    /// Return the cached value when `key` matches the generation it was learned
    /// in, otherwise compute it and cache it against `key`.
    ///
    /// `key` of `None` means the generation cannot be observed: the value is
    /// computed and not cached.
    ///
    /// # Errors
    /// Whatever `compute` returns.
    pub fn get_or_try_init<E>(
        &self,
        key: Option<FactKey>,
        compute: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let Some(key) = key else {
            tracing::debug!(
                target: "velnor.docker",
                fact = self.name,
                lifetime = self.lifetime.label(),
                cached = false,
                reason = "generation-unobservable",
                "docker fact recomputed"
            );
            return compute();
        };
        debug_assert_eq!(key.lifetime, self.lifetime);
        {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some((cached_key, value)) = state.as_ref()
                && *cached_key == key
            {
                tracing::debug!(
                    target: "velnor.docker",
                    fact = self.name,
                    lifetime = self.lifetime.label(),
                    cached = true,
                    "docker fact reused"
                );
                return Ok(value.clone());
            }
        }
        let value = compute()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        *state = Some((key, value.clone()));
        tracing::debug!(
            target: "velnor.docker",
            fact = self.name,
            lifetime = self.lifetime.label(),
            cached = false,
            reason = "new-generation",
            "docker fact learned"
        );
        Ok(value)
    }

    /// Drop the cached value. Only for a caller that has observed the fact's
    /// own generation change out of band; never as a reaction to an unrelated
    /// failure.
    pub fn invalidate(&self) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn a_fact_is_recomputed_when_its_generation_changes() {
        let fact: Fact<u32> = Fact::new("test", FactLifetime::Daemon);
        let computed = AtomicU32::new(0);
        let compute = || -> Result<u32, ()> { Ok(computed.fetch_add(1, Ordering::SeqCst)) };

        let first = FactKey {
            lifetime: FactLifetime::Daemon,
            token: "generation-1".into(),
        };
        assert_eq!(fact.get_or_try_init(Some(first.clone()), compute), Ok(0));
        assert_eq!(fact.get_or_try_init(Some(first), compute), Ok(0));
        assert_eq!(computed.load(Ordering::SeqCst), 1);

        let second = FactKey {
            lifetime: FactLifetime::Daemon,
            token: "generation-2".into(),
        };
        assert_eq!(fact.get_or_try_init(Some(second), compute), Ok(1));
        assert_eq!(computed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_unobservable_generation_is_never_cached() {
        let fact: Fact<u32> = Fact::new("test", FactLifetime::Host);
        let computed = AtomicU32::new(0);
        let compute = || -> Result<u32, ()> { Ok(computed.fetch_add(1, Ordering::SeqCst)) };
        assert_eq!(fact.get_or_try_init(None, compute), Ok(0));
        assert_eq!(fact.get_or_try_init(None, compute), Ok(1));
        assert_eq!(computed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_computation_is_not_cached() {
        let fact: Fact<u32> = Fact::new("test", FactLifetime::Daemon);
        let key = FactKey {
            lifetime: FactLifetime::Daemon,
            token: "generation".into(),
        };
        assert_eq!(
            fact.get_or_try_init(Some(key.clone()), || Err::<u32, &str>("boom")),
            Err("boom")
        );
        assert_eq!(
            fact.get_or_try_init(Some(key), || Ok::<u32, &str>(7)),
            Ok(7)
        );
    }

    #[test]
    fn keys_of_different_lifetimes_never_compare_equal() {
        assert_ne!(image("same"), job("same"));
        assert_eq!(image("same"), image("same"));
        assert_ne!(image("a"), image("b"));
        assert_eq!(image("same").lifetime, FactLifetime::Image);
        assert_eq!(job("same").lifetime, FactLifetime::Job);
    }

    #[test]
    fn host_generation_follows_the_witness_file() {
        let dir = std::env::temp_dir().join(format!("velnor-fact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let witness = dir.join("witness");
        assert_eq!(host(&witness), None);
        std::fs::write(&witness, "one").unwrap();
        let first = host(&witness).expect("witness exists");
        assert_eq!(first.lifetime, FactLifetime::Host);
        assert_eq!(host(&witness), Some(first));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn daemon_generation_is_unobservable_without_a_local_dockerd() {
        let dir = std::env::temp_dir().join(format!("velnor-daemon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("docker.pid");
        assert_eq!(daemon_from(&pidfile, &dir), None);
        std::fs::write(&pidfile, "not-a-pid").unwrap();
        assert_eq!(daemon_from(&pidfile, &dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
