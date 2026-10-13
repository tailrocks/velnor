//! One derived resource budget for the whole machine, and the share of it that
//! belongs to a single runner slot.
//!
//! Velnor, Cargo, `mbx`, Docker and BuildKit each default to "I own this
//! machine". Four slots on a sixteen-core host therefore run four Cargo builds
//! that each pick `-j16`, inside one `velnor-jobs.slice` whose aggregate quota
//! is 15.2 cores. Everything is runnable and nothing progresses.
//!
//! The fix is a single budget that is *derived* rather than assumed:
//!
//! * the machine's real CPU capacity is the smallest of the constraints that
//!   actually apply — `available_parallelism`, every cgroup v2 `cpu.max` on the
//!   path from the cgroup root to this process, the effective cpuset, and the
//!   aggregate quota on the job slice;
//! * memory is the smallest of `MemTotal` and every applicable `memory.max`;
//! * the slot's share is that budget divided by the number of provisioned
//!   slots, and it is what the job is told.
//!
//! A value that cannot be read is [`Observation::Unobservable`] and carries the
//! reason. It is never cached and never replaced by a guess — the same rule
//! [`crate::docker::facts`] applies to host facts. A budget with an
//! unobservable CPU capacity produces no `CARGO_BUILD_JOBS`, no `--cpus`, and
//! no scheduler sizing at all, because sizing to an invented number is how the
//! defect got here in the first place.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

/// Where the job slice's aggregate quota lives once systemd has created it.
/// The unit is packaged (`debian/velnor-jobs.slice`) with a drop-in that pins
/// the quota to 95% of the host's online CPUs; that is an *aggregate* ceiling
/// shared by every slot, so it bounds the machine budget but is never divided
/// into it twice.
const JOB_SLICE_CGROUP: &str = "velnor-jobs.slice";

/// Headroom mbx documents for its own default memory budget (85% of physical
/// memory, "leaving headroom for everything that is not a compiler"). Velnor
/// has to state the budget explicitly, so it states it with the same headroom
/// rather than inventing a different one.
const SCHEDULER_MEMORY_PERCENT: u64 = 85;

/// A fact that was either read from this machine or provably could not be.
///
/// The `Unobservable` arm carries why. Nothing in this module converts it into
/// a number: a caller either has a measurement or has nothing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation<T> {
    Observed(T),
    Unobservable(String),
}

impl<T> Observation<T> {
    pub(crate) fn value(&self) -> Option<&T> {
        match self {
            Self::Observed(value) => Some(value),
            Self::Unobservable(_) => None,
        }
    }
}

/// One constraint that was read, kept so the derivation can explain itself in
/// the job log instead of appearing as an unexplained number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Constraint {
    pub(crate) source: String,
    /// Milli-CPUs, so a fractional quota such as 15.2 cores survives.
    pub(crate) cpu_milli: Option<u64>,
    pub(crate) memory_bytes: Option<u64>,
}

/// The machine's capacity as actually constrained, not as advertised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostBudget {
    pub(crate) cpu_milli: Observation<u64>,
    pub(crate) memory_bytes: Observation<u64>,
    pub(crate) constraints: Vec<Constraint>,
}

/// The part of [`HostBudget`] that belongs to one runner slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotBudget {
    pub(crate) slots: NonZeroU32,
    /// Whole CPUs this slot may keep busy. Compiler drivers count in whole
    /// jobs, so the share is floored and never rounds up into the next slot.
    pub(crate) cpus: Observation<NonZeroU32>,
    pub(crate) memory_bytes: Observation<u64>,
    pub(crate) host: HostBudget,
}

impl HostBudget {
    /// Read the budget from a filesystem root.
    ///
    /// `root` is `/` in production and a synthetic tree in tests. `parallelism`
    /// is `std::thread::available_parallelism`, injected so the derivation is
    /// testable without owning the machine it runs on.
    pub(crate) fn observe(root: &Path, parallelism: Option<u32>) -> Self {
        let mut constraints = Vec::new();

        if let Some(cpus) = parallelism {
            constraints.push(Constraint {
                source: "available_parallelism".to_owned(),
                cpu_milli: Some(u64::from(cpus) * 1000),
                memory_bytes: None,
            });
        }

        if let Some(total) = read_mem_total_bytes(root) {
            constraints.push(Constraint {
                source: "/proc/meminfo MemTotal".to_owned(),
                cpu_milli: None,
                memory_bytes: Some(total),
            });
        }

        // Every cgroup between the v2 root and this process can carry a quota,
        // and the effective limit is the tightest of them. Reading only the
        // leaf is how a container-hosted daemon ends up believing it owns the
        // host.
        for cgroup in self_cgroup_chain(root) {
            constraints.extend(read_cgroup_constraints(root, &cgroup));
        }

        // The job slice is the aggregate ceiling every job container lives
        // under, whether or not this process is inside it.
        constraints.extend(read_cgroup_constraints(
            root,
            &PathBuf::from(JOB_SLICE_CGROUP),
        ));

        let cpu_milli = min_constraint(&constraints, |constraint| constraint.cpu_milli).map_or_else(
            || {
                Observation::Unobservable(
                    "no CPU capacity source could be read (available_parallelism, cgroup cpu.max, cpuset.cpus.effective)"
                        .to_owned(),
                )
            },
            Observation::Observed,
        );
        let memory_bytes = min_constraint(&constraints, |constraint| constraint.memory_bytes)
            .map_or_else(
                || {
                    Observation::Unobservable(
                        "no memory capacity source could be read (/proc/meminfo, cgroup memory.max)"
                            .to_owned(),
                    )
                },
                Observation::Observed,
            );

        Self {
            cpu_milli,
            memory_bytes,
            constraints,
        }
    }

    /// Observe the budget of the host this process is running on.
    pub(crate) fn observe_host() -> Self {
        let parallelism = std::thread::available_parallelism()
            .ok()
            .and_then(|value| u32::try_from(value.get()).ok());
        Self::observe(Path::new("/"), parallelism)
    }

    /// Divide the budget between `slots`.
    ///
    /// This is the whole point of the type: the slice quota is an aggregate,
    /// so a slot gets `budget / slots`, not `budget`. The share floors at one
    /// CPU because a slot that is admitted at all must be able to run one
    /// compiler process.
    pub(crate) fn per_slot(&self, slots: NonZeroU32) -> SlotBudget {
        let cpus = match &self.cpu_milli {
            Observation::Observed(milli) => {
                let share = milli / u64::from(slots.get()) / 1000;
                let share = u32::try_from(share).unwrap_or(u32::MAX).max(1);
                Observation::Observed(NonZeroU32::new(share).unwrap_or(NonZeroU32::MIN))
            }
            Observation::Unobservable(reason) => Observation::Unobservable(reason.clone()),
        };
        let memory_bytes = match &self.memory_bytes {
            Observation::Observed(bytes) => Observation::Observed(
                bytes / 100 * SCHEDULER_MEMORY_PERCENT / u64::from(slots.get()),
            ),
            Observation::Unobservable(reason) => Observation::Unobservable(reason.clone()),
        };
        SlotBudget {
            slots,
            cpus,
            memory_bytes,
            host: self.clone(),
        }
    }
}

impl SlotBudget {
    /// Cap the slot's share by a limit the operator already set on the job
    /// container (`--cpus`, from `VELNOR_JOB_CPUS`). An explicit operator
    /// limit is policy and is never widened; it only ever narrows the share.
    pub(crate) fn capped_by_container_cpus(mut self, container_cpus: Option<f64>) -> Self {
        let Some(container_cpus) = container_cpus else {
            return self;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cap = container_cpus.max(1.0).floor() as u32;
        let cap = NonZeroU32::new(cap).unwrap_or(NonZeroU32::MIN);
        if let Observation::Observed(cpus) = self.cpus {
            self.cpus = Observation::Observed(cpus.min(cap));
        } else {
            self.cpus = Observation::Observed(cap);
        }
        self
    }

    /// `--cpus` for this job's container, when the budget is known.
    ///
    /// Without it the container inherits the whole aggregate slice quota, so
    /// four slots each believe they may use 95% of the host.
    pub(crate) fn docker_cpu_option(&self) -> Option<[String; 2]> {
        self.cpus
            .value()
            .map(|cpus| ["--cpus".to_owned(), cpus.to_string()])
    }

    /// The environment that makes the budget real inside the job.
    ///
    /// * `CARGO_BUILD_JOBS` is Cargo's own job cap, and mbx documents it as the
    ///   way to cap how many weighted permits one build holds without shrinking
    ///   the machine-wide pool other builds share.
    /// * `MAKEFLAGS` carries the same number to the `make` invoked by `-sys`
    ///   build scripts, which otherwise pick their own `-j`.
    /// * `MBX_SCHEDULER_CPUS` / `MBX_SCHEDULER_MEMORY` are mbx's documented
    ///   environment contract for its scheduler pool (`crates/mbx/src/config.rs`
    ///   in `jdx/mr-boxington`, v1.7.0, the version this image pins). Their
    ///   defaults are "logical CPUs" and "85% of physical memory", neither of
    ///   which sees the slice quota, so both are stated explicitly.
    pub(crate) fn job_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(cpus) = self.cpus.value() {
            env.push(("CARGO_BUILD_JOBS".to_owned(), cpus.to_string()));
            env.push(("MAKEFLAGS".to_owned(), format!("-j{cpus}")));
            env.push(("MBX_SCHEDULER_CPUS".to_owned(), cpus.to_string()));
        }
        if let Some(bytes) = self.memory_bytes.value() {
            let mib = (bytes / (1024 * 1024)).max(1);
            env.push(("MBX_SCHEDULER_MEMORY".to_owned(), format!("{mib}MiB")));
        }
        env
    }

    /// One line explaining the budget, its division, and any workflow value it
    /// overrode. Silent resource policy is indistinguishable from a bug, so
    /// this is meant to be printed, not inspected.
    pub(crate) fn notice(&self, overridden: &[String]) -> String {
        let cpus = match &self.cpus {
            Observation::Observed(cpus) => format!("{cpus} CPU(s)"),
            Observation::Unobservable(reason) => format!("unobservable ({reason})"),
        };
        let memory = match &self.memory_bytes {
            Observation::Observed(bytes) => format!("{}MiB", bytes / (1024 * 1024)),
            Observation::Unobservable(reason) => format!("unobservable ({reason})"),
        };
        let host = match &self.host.cpu_milli {
            Observation::Observed(milli) => {
                format!("{}.{} CPU(s)", milli / 1000, milli % 1000 / 100)
            }
            Observation::Unobservable(reason) => format!("unobservable ({reason})"),
        };
        let sources = self
            .host
            .constraints
            .iter()
            .map(|constraint| constraint.source.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let mut notice = format!(
            "velnor: host budget {host} from [{sources}]; {} slot(s); this job gets {cpus} and {memory}",
            self.slots
        );
        if !overridden.is_empty() {
            notice.push_str(&format!(
                "; daemon budget overrides workflow-set {}",
                overridden.join(", ")
            ));
        }
        notice
    }
}

/// Number of runner slots this daemon provisioned, derived rather than assumed.
///
/// `slot_dir` is this job's `…/work/slot-N` directory when the daemon runs more
/// than one slot; every slot is a separate OS process, so the only shared
/// evidence of how many there are is the set of sibling slot directories under
/// the daemon work root. `env_hint` is `VELNOR_SLOTS`, which the packaged units
/// pass to `--slots`. The larger of the two wins, because a slot whose
/// directory has not been created yet still competes for the machine.
pub(crate) fn observe_slots(
    slot_dir: Option<&Path>,
    env_hint: Option<&str>,
) -> Observation<NonZeroU32> {
    let from_hint = env_hint
        .and_then(|value| value.trim().parse::<u32>().ok())
        .and_then(NonZeroU32::new);
    let from_dirs = slot_dir.and_then(count_sibling_slots);
    match (from_dirs, from_hint) {
        (Some(dirs), Some(hint)) => Observation::Observed(dirs.max(hint)),
        (Some(dirs), None) => Observation::Observed(dirs),
        (None, Some(hint)) => Observation::Observed(hint),
        // A daemon running one slot does not create a `slot-N` directory at
        // all, so "no slot directory and no hint" is a single slot, observed.
        (None, None) if slot_dir.is_none() => Observation::Observed(NonZeroU32::MIN),
        (None, None) => {
            Observation::Unobservable("no sibling slot directories and no VELNOR_SLOTS".to_owned())
        }
    }
}

fn count_sibling_slots(slot_dir: &Path) -> Option<NonZeroU32> {
    if !is_slot_dir_name(slot_dir) {
        return None;
    }
    let parent = slot_dir.parent()?;
    let mut count = 0u32;
    for entry in std::fs::read_dir(parent).ok()? {
        let Ok(entry) = entry else { continue };
        if is_slot_dir_name(&entry.path()) && entry.path().is_dir() {
            count = count.saturating_add(1);
        }
    }
    NonZeroU32::new(count)
}

fn is_slot_dir_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn min_constraint(
    constraints: &[Constraint],
    pick: impl Fn(&Constraint) -> Option<u64>,
) -> Option<u64> {
    constraints.iter().filter_map(pick).min()
}

/// The cgroup v2 path of this process and every ancestor up to the root.
fn self_cgroup_chain(root: &Path) -> Vec<PathBuf> {
    let Ok(content) = std::fs::read_to_string(root.join("proc/self/cgroup")) else {
        return Vec::new();
    };
    // cgroup v2 has exactly one unified line, `0::<path>`.
    let Some(path) = content
        .lines()
        .find_map(|line| line.strip_prefix("0::").map(str::trim))
    else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut current = PathBuf::new();
    chain.push(current.clone());
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        current = current.join(segment);
        chain.push(current.clone());
    }
    chain
}

fn read_cgroup_constraints(root: &Path, cgroup: &Path) -> Vec<Constraint> {
    let dir = root.join("sys/fs/cgroup").join(cgroup);
    let label = if cgroup.as_os_str().is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", cgroup.display())
    };
    let mut constraints = Vec::new();

    if let Some(milli) = read_trimmed(&dir.join("cpu.max")).and_then(|value| parse_cpu_max(&value))
    {
        constraints.push(Constraint {
            source: format!("cgroup {label} cpu.max"),
            cpu_milli: Some(milli),
            memory_bytes: None,
        });
    }
    if let Some(count) =
        read_trimmed(&dir.join("cpuset.cpus.effective")).and_then(|value| parse_cpu_list(&value))
    {
        constraints.push(Constraint {
            source: format!("cgroup {label} cpuset.cpus.effective"),
            cpu_milli: Some(u64::from(count) * 1000),
            memory_bytes: None,
        });
    }
    if let Some(bytes) =
        read_trimmed(&dir.join("memory.max")).and_then(|value| value.parse::<u64>().ok())
    {
        constraints.push(Constraint {
            source: format!("cgroup {label} memory.max"),
            cpu_milli: None,
            memory_bytes: Some(bytes),
        });
    }
    constraints
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

/// `cpu.max` is `<quota> <period>` in microseconds, or `max <period>`.
fn parse_cpu_max(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let quota = fields.next()?;
    if quota == "max" {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    let period = fields.next().unwrap_or("100000").parse::<u64>().ok()?;
    if period == 0 {
        return None;
    }
    Some(quota.checked_mul(1000)? / period)
}

/// `cpuset.cpus.effective` is a comma-separated list of ids and inclusive
/// ranges, e.g. `0-3,8,12-13`. An empty value means "inherit", not "none".
fn parse_cpu_list(value: &str) -> Option<u32> {
    if value.is_empty() {
        return None;
    }
    let mut count = 0u32;
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let range = match part.split_once('-') {
            Some((start, end)) => {
                let start = start.parse::<u32>().ok()?;
                let end = end.parse::<u32>().ok()?;
                if end < start {
                    return None;
                }
                end - start + 1
            }
            None => {
                part.parse::<u32>().ok()?;
                1
            }
        };
        count = count.checked_add(range)?;
    }
    (count > 0).then_some(count)
}

fn read_mem_total_bytes(root: &Path) -> Option<u64> {
    let content = std::fs::read_to_string(root.join("proc/meminfo")).ok()?;
    let line = content.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// A throwaway filesystem root holding synthetic `/proc` and
    /// `/sys/fs/cgroup` inputs. The crate's other unit tests build temp trees
    /// the same way rather than taking a dependency for it.
    struct SyntheticRoot(PathBuf);

    impl SyntheticRoot {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "velnor-host-budget-{label}-{}-{nonce}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for SyntheticRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn quota_smaller_than_the_core_count_wins() {
        let root = SyntheticRoot::new("quota_smaller_than_the_core_count_wins");
        write(root.path(), "proc/self/cgroup", "0::/velnor.slice/daemon\n");
        // Four CPUs of quota on a sixteen-CPU machine.
        write(
            root.path(),
            "sys/fs/cgroup/velnor.slice/daemon/cpu.max",
            "400000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        assert_eq!(budget.cpu_milli, Observation::Observed(4000));
    }

    #[test]
    fn a_cpuset_narrower_than_the_quota_wins() {
        let root = SyntheticRoot::new("a_cpuset_narrower_than_the_quota_wins");
        write(root.path(), "proc/self/cgroup", "0::/jobs\n");
        write(root.path(), "sys/fs/cgroup/jobs/cpu.max", "800000 100000\n");
        write(
            root.path(),
            "sys/fs/cgroup/jobs/cpuset.cpus.effective",
            "0-1\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        assert_eq!(budget.cpu_milli, Observation::Observed(2000));
    }

    #[test]
    fn an_ancestor_quota_is_not_missed() {
        let root = SyntheticRoot::new("an_ancestor_quota_is_not_missed");
        write(root.path(), "proc/self/cgroup", "0::/outer/inner\n");
        write(
            root.path(),
            "sys/fs/cgroup/outer/cpu.max",
            "200000 100000\n",
        );
        write(
            root.path(),
            "sys/fs/cgroup/outer/inner/cpu.max",
            "max 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(64));
        assert_eq!(budget.cpu_milli, Observation::Observed(2000));
    }

    #[test]
    fn the_job_slice_quota_bounds_the_machine() {
        let root = SyntheticRoot::new("the_job_slice_quota_bounds_the_machine");
        // 95% of sixteen CPUs, exactly what the packaged drop-in installs.
        write(
            root.path(),
            "sys/fs/cgroup/velnor-jobs.slice/cpu.max",
            "1520000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        assert_eq!(budget.cpu_milli, Observation::Observed(15_200));
    }

    #[test]
    fn an_unobservable_value_says_so_instead_of_guessing() {
        let root = SyntheticRoot::new("an_unobservable_value_says_so_instead_of_guessing");
        let budget = HostBudget::observe(root.path(), None);
        assert!(
            matches!(budget.cpu_milli, Observation::Unobservable(ref reason) if !reason.is_empty())
        );
        assert!(matches!(budget.memory_bytes, Observation::Unobservable(_)));
        // And nothing downstream invents a number from it.
        let slot = budget.per_slot(NonZeroU32::new(4).unwrap());
        assert!(slot.docker_cpu_option().is_none());
        assert!(slot.job_env().is_empty());
        assert!(slot.notice(&[]).contains("unobservable"));
    }

    #[test]
    fn memory_takes_the_tightest_of_meminfo_and_cgroup() {
        let root = SyntheticRoot::new("memory_takes_the_tightest_of_meminfo_and_cgroup");
        write(root.path(), "proc/meminfo", "MemTotal:       16777216 kB\n");
        write(root.path(), "proc/self/cgroup", "0::/jobs\n");
        write(
            root.path(),
            "sys/fs/cgroup/jobs/memory.max",
            &format!("{}\n", 4u64 * 1024 * 1024 * 1024),
        );
        let budget = HostBudget::observe(root.path(), Some(4));
        assert_eq!(
            budget.memory_bytes,
            Observation::Observed(4 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn slots_divide_the_budget_instead_of_each_claiming_the_host() {
        let root = SyntheticRoot::new("slots_divide_the_budget_instead_of_each_claiming_the_host");
        write(
            root.path(),
            "sys/fs/cgroup/velnor-jobs.slice/cpu.max",
            "1520000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        let host_cpus = *budget.cpu_milli.value().unwrap() / 1000;

        let slots = NonZeroU32::new(4).unwrap();
        let slot = budget.per_slot(slots);
        let share = slot.cpus.value().unwrap().get();

        assert_eq!(share, 3, "15.2 cores over four slots is three whole CPUs");
        assert!(
            u64::from(share) * u64::from(slots.get()) <= host_cpus,
            "four slots must fit inside the budget, not claim it four times"
        );
        assert_ne!(
            u64::from(share),
            host_cpus,
            "a slot must not be handed the whole machine"
        );
        assert_eq!(
            slot.job_env(),
            vec![
                ("CARGO_BUILD_JOBS".to_owned(), "3".to_owned()),
                ("MAKEFLAGS".to_owned(), "-j3".to_owned()),
                ("MBX_SCHEDULER_CPUS".to_owned(), "3".to_owned()),
            ]
        );
        assert_eq!(
            slot.docker_cpu_option(),
            Some(["--cpus".to_owned(), "3".to_owned()])
        );
    }

    #[test]
    fn a_single_slot_keeps_the_whole_budget() {
        let root = SyntheticRoot::new("a_single_slot_keeps_the_whole_budget");
        write(
            root.path(),
            "sys/fs/cgroup/velnor-jobs.slice/cpu.max",
            "1520000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        let slot = budget.per_slot(NonZeroU32::MIN);
        assert_eq!(slot.cpus.value().unwrap().get(), 15);
    }

    #[test]
    fn a_slot_share_never_falls_below_one_cpu() {
        let root = SyntheticRoot::new("a_slot_share_never_falls_below_one_cpu");
        write(
            root.path(),
            "sys/fs/cgroup/velnor-jobs.slice/cpu.max",
            "200000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(2));
        let slot = budget.per_slot(NonZeroU32::new(8).unwrap());
        assert_eq!(slot.cpus.value().unwrap().get(), 1);
    }

    #[test]
    fn an_operator_container_cap_narrows_but_never_widens_the_share() {
        let root =
            SyntheticRoot::new("an_operator_container_cap_narrows_but_never_widens_the_share");
        write(
            root.path(),
            "sys/fs/cgroup/velnor-jobs.slice/cpu.max",
            "1600000 100000\n",
        );
        let budget = HostBudget::observe(root.path(), Some(16));
        let slot = budget.per_slot(NonZeroU32::new(2).unwrap());
        assert_eq!(slot.cpus.value().unwrap().get(), 8);

        let narrowed = slot.clone().capped_by_container_cpus(Some(2.0));
        assert_eq!(narrowed.cpus.value().unwrap().get(), 2);

        let widened = slot.capped_by_container_cpus(Some(64.0));
        assert_eq!(widened.cpus.value().unwrap().get(), 8);
    }

    #[test]
    fn memory_share_is_stated_in_mbx_units() {
        let root = SyntheticRoot::new("memory_share_is_stated_in_mbx_units");
        write(root.path(), "proc/meminfo", "MemTotal:       16777216 kB\n");
        let budget = HostBudget::observe(root.path(), Some(8));
        let slot = budget.per_slot(NonZeroU32::new(4).unwrap());
        // 16GiB * 85% / 4 slots = 3481MiB.
        assert_eq!(
            slot.job_env()
                .into_iter()
                .find(|(name, _)| name == "MBX_SCHEDULER_MEMORY"),
            Some(("MBX_SCHEDULER_MEMORY".to_owned(), "3481MiB".to_owned()))
        );
    }

    #[test]
    fn slot_count_comes_from_sibling_directories() {
        let work = SyntheticRoot::new("slot_count_comes_from_sibling_directories");
        for index in 1..=4 {
            fs::create_dir_all(work.path().join(format!("slot-{index}"))).unwrap();
        }
        fs::create_dir_all(work.path().join("_velnor_mbx")).unwrap();
        let observed = observe_slots(Some(&work.path().join("slot-2")), None);
        assert_eq!(observed.value().unwrap().get(), 4);
    }

    #[test]
    fn a_slot_directory_that_does_not_exist_yet_still_counts_via_the_hint() {
        let work = SyntheticRoot::new(
            "a_slot_directory_that_does_not_exist_yet_still_counts_via_the_hint",
        );
        fs::create_dir_all(work.path().join("slot-1")).unwrap();
        let observed = observe_slots(Some(&work.path().join("slot-1")), Some("4"));
        assert_eq!(observed.value().unwrap().get(), 4);
    }

    #[test]
    fn no_slot_directory_is_one_slot() {
        assert_eq!(observe_slots(None, None).value().unwrap().get(), 1);
    }

    #[test]
    fn cpu_list_parsing_covers_ranges_and_singletons() {
        assert_eq!(parse_cpu_list("0-3,8,12-13"), Some(7));
        assert_eq!(parse_cpu_list("5"), Some(1));
        assert_eq!(parse_cpu_list(""), None);
        assert_eq!(parse_cpu_list("3-1"), None);
    }

    #[test]
    fn cpu_max_parsing_handles_max_and_fractions() {
        assert_eq!(parse_cpu_max("max 100000"), None);
        assert_eq!(parse_cpu_max("50000 100000"), Some(500));
        assert_eq!(parse_cpu_max("1520000 100000"), Some(15_200));
    }
}
