//! Output renderers for Velnor operator surfaces.
//!
//! Plan 065 owns the table/wide/json/yaml/jsonl/name renderer matrix. JSON,
//! YAML, and JSONL emit the versioned model resources verbatim; table and
//! wide are human views where durations render readably; `name` is the
//! unversioned canonical identity projection. Warnings always go to the
//! error stream, payloads always to the body stream. This crate depends
//! only on the shared model types and never on Clap or Axum.

#![forbid(unsafe_code)]

use std::io::Write;

use velnor_model::{
    Adapter, AnyResource, Capability, ConditionStatus, DurationMs, Event, Host, Instance, Job,
    Lease, QueueEntry, RepositoryRef, Reservation, Run, RunnerRegistration, Slot, SlotPhase,
};

/// Canonical spellings of the closed format set, in order.
pub const RENDER_FORMATS: [&str; 6] = ["table", "wide", "json", "yaml", "jsonl", "name"];

/// Closed set of output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputFormat {
    /// Aligned human table; durations human-readable.
    Table,
    /// Human table with provenance and summary columns.
    Wide,
    /// Pretty-printed versioned JSON.
    Json,
    /// Versioned YAML.
    Yaml,
    /// One compact versioned JSON object per line.
    Jsonl,
    /// Unversioned newline-delimited `<kind>:<name>` identity projection.
    Name,
}

impl OutputFormat {
    /// Every format in canonical order.
    pub const ALL: [OutputFormat; 6] = [
        OutputFormat::Table,
        OutputFormat::Wide,
        OutputFormat::Json,
        OutputFormat::Yaml,
        OutputFormat::Jsonl,
        OutputFormat::Name,
    ];

    /// Canonical spelling used by `-o/--output`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Table => "table",
            OutputFormat::Wide => "wide",
            OutputFormat::Json => "json",
            OutputFormat::Yaml => "yaml",
            OutputFormat::Jsonl => "jsonl",
            OutputFormat::Name => "name",
        }
    }

    /// Exact parse of the canonical spelling; anything else fails closed.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        OutputFormat::ALL.into_iter().find(|f| f.as_str() == raw)
    }

    /// True for formats whose bytes are meant for programs, not humans.
    #[must_use]
    pub const fn is_machine(self) -> bool {
        matches!(
            self,
            OutputFormat::Json | OutputFormat::Yaml | OutputFormat::Jsonl | OutputFormat::Name
        )
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether ANSI styling may be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorPolicy {
    /// Style allowed (headers bold, warnings yellow/red).
    Always,
    /// Never emit ANSI escapes.
    Never,
}

impl ColorPolicy {
    /// Resolve `--no-color` and TTY detection: color only when allowed and
    /// attached to a terminal.
    #[must_use]
    pub fn resolve(no_color: bool, is_tty: bool) -> Self {
        if no_color || !is_tty {
            ColorPolicy::Never
        } else {
            ColorPolicy::Always
        }
    }
}

/// Rendering knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Styling policy resolved by the caller.
    pub color: ColorPolicy,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            color: ColorPolicy::Never,
        }
    }
}

const ANSI_BOLD: &str = "\u{1b}[1m";
const ANSI_YELLOW: &str = "\u{1b}[33m";
const ANSI_RED: &str = "\u{1b}[31m";
const ANSI_RESET: &str = "\u{1b}[0m";

/// Human-table projection shared by every resource noun and the
/// [`AnyResource`] envelope.
///
/// Machine formats bypass this trait entirely and serialize the versioned
/// resource; only table/wide/name consult it.
pub trait Tabular {
    /// Column headers for the requested width.
    fn columns(&self, wide: bool) -> Vec<&'static str>;

    /// Row cells for this instance at the requested width.
    fn cells(&self, wide: bool) -> Vec<String>;

    /// Canonical `<kind>:<name>` identity used by the `name` format.
    fn identity(&self) -> String;
}

/// Shared trailing columns every wide view appends after the narrow set.
pub const WIDE_SUFFIX: [&str; 3] = ["SOURCE", "REASON", "LAST-TRANSITION"];

trait Projection {
    const KIND: &'static str;
    fn narrow_columns() -> &'static [&'static str];
    fn project(&self, wide: bool) -> Vec<String>;
    fn object_name(&self) -> &str;
}

macro_rules! impl_tabular {
    ($ty:ty) => {
        impl Tabular for $ty {
            fn columns(&self, wide: bool) -> Vec<&'static str> {
                let mut columns = <$ty as Projection>::narrow_columns().to_vec();
                if wide {
                    columns.extend(WIDE_SUFFIX);
                }
                columns
            }

            fn cells(&self, wide: bool) -> Vec<String> {
                let mut cells = <$ty as Projection>::project(self, wide);
                if wide {
                    cells.push(self.meta.source.as_str().to_owned());
                    cells.push(self.meta.reason.clone().unwrap_or_else(|| "-".to_owned()));
                    cells.push(rfc3339_cell(&self.meta.last_transition_time));
                }
                cells
            }

            fn identity(&self) -> String {
                format!(
                    "{}:{}",
                    <$ty as Projection>::KIND,
                    <$ty as Projection>::object_name(self)
                )
            }
        }
    };
}

fn rfc3339_cell(at: &velnor_model::Timestamp) -> String {
    at.to_rfc3339().unwrap_or_else(|_| "-".to_owned())
}

fn opt_cell(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "-".to_owned())
}

fn ms_cell(value: &Option<DurationMs>) -> String {
    value.map_or_else(|| "-".to_owned(), |ms| human_ms(ms.as_u64()))
}

/// Human duration rendering used only in table/wide cells; machine formats
/// carry unsigned `*_ms` fields instead.
#[must_use]
pub fn human_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        let whole = ms / 1_000;
        let tenth = (ms % 1_000) / 100;
        if tenth > 0 {
            format!("{whole}.{tenth}s")
        } else {
            format!("{whole}s")
        }
    } else if ms < 3_600_000 {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1_000)
    } else if ms < 86_400_000 {
        format!("{}h{:02}m", ms / 3_600_000, (ms % 3_600_000) / 60_000)
    } else {
        format!("{}d{:02}h", ms / 86_400_000, (ms % 86_400_000) / 3_600_000)
    }
}

fn repo_cell(repo: &RepositoryRef) -> String {
    repo.full_name()
}

impl Projection for Host {
    const KIND: &'static str = "Host";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "HOSTNAME", "AGENT", "LABELS"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.hostname.clone(),
            opt_cell(&self.agent_version),
            self.labels.keys().cloned().collect::<Vec<_>>().join(","),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Instance {
    const KIND: &'static str = "Instance";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "HOST", "VERSION", "UPTIME", "SLOTS", "BUSY"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.host.clone(),
            self.version.clone(),
            ms_cell(&self.uptime_ms),
            self.slots_configured.to_string(),
            self.slots_busy.to_string(),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Slot {
    const KIND: &'static str = "Slot";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "HOST", "INDEX", "CLASS", "PHASE", "JOB"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.host.clone(),
            self.index.to_string(),
            self.slot_kind.as_str().to_owned(),
            self.phase.as_str().to_owned(),
            opt_cell(&self.job),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for RunnerRegistration {
    const KIND: &'static str = "RunnerRegistration";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "LABELS", "EPHEMERAL", "ONLINE"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.labels.keys().cloned().collect::<Vec<_>>().join(","),
            self.ephemeral.to_string(),
            self.online.to_string(),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Job {
    const KIND: &'static str = "Job";
    fn narrow_columns() -> &'static [&'static str] {
        &[
            "NAME",
            "REPO",
            "RUN",
            "WORKFLOW",
            "QUEUED",
            "DURATION",
            "CONCLUSION",
        ]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            repo_cell(&self.repository),
            opt_cell(&self.run),
            self.workflow.clone(),
            ms_cell(&self.queued_ms),
            ms_cell(&self.duration_ms),
            opt_cell(&self.conclusion),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Run {
    const KIND: &'static str = "Run";
    fn narrow_columns() -> &'static [&'static str] {
        &[
            "NAME",
            "REPO",
            "NUMBER",
            "BRANCH",
            "EVENT",
            "STATUS",
            "CONCLUSION",
        ]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            repo_cell(&self.repository),
            self.number.to_string(),
            self.head_branch.clone(),
            self.event.clone(),
            self.status.clone(),
            opt_cell(&self.conclusion),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for QueueEntry {
    const KIND: &'static str = "QueueEntry";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "POSITION", "JOB", "WAIT"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.position.to_string(),
            self.job.clone(),
            ms_cell(&self.wait_ms),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Event {
    const KIND: &'static str = "Event";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "SEQ", "OCCURRED", "EVENT", "SUBJECT"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.sequence.to_string(),
            rfc3339_cell(&self.occurred_at),
            self.event_kind.clone(),
            self.subject.clone(),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Reservation {
    const KIND: &'static str = "Reservation";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "SLOT", "PURPOSE", "EXPIRES"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.slot.clone(),
            self.purpose.clone(),
            rfc3339_cell(&self.expires_at),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Lease {
    const KIND: &'static str = "Lease";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "HOLDER", "TTL", "EXPIRES"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.holder.clone(),
            ms_cell(&self.ttl_ms),
            rfc3339_cell(&self.expires_at),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Capability {
    const KIND: &'static str = "Capability";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "KEY", "SUPPORTED"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.key.clone(),
            self.supported.to_string(),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl Projection for Adapter {
    const KIND: &'static str = "Adapter";
    fn narrow_columns() -> &'static [&'static str] {
        &["NAME", "ADAPTER", "VERSION", "ACTIONS"]
    }
    fn project(&self, _wide: bool) -> Vec<String> {
        vec![
            self.meta.name.clone(),
            self.adapter.clone(),
            self.version.clone(),
            self.actions.join(","),
        ]
    }
    fn object_name(&self) -> &str {
        &self.meta.name
    }
}

impl_tabular!(Host);
impl_tabular!(Instance);
impl_tabular!(Slot);
impl_tabular!(RunnerRegistration);
impl_tabular!(Job);
impl_tabular!(Run);
impl_tabular!(QueueEntry);
impl_tabular!(Event);
impl_tabular!(Reservation);
impl_tabular!(Lease);
impl_tabular!(Capability);
impl_tabular!(Adapter);

impl Tabular for AnyResource {
    fn columns(&self, wide: bool) -> Vec<&'static str> {
        match self {
            AnyResource::Host(r) => r.columns(wide),
            AnyResource::Instance(r) => r.columns(wide),
            AnyResource::Slot(r) => r.columns(wide),
            AnyResource::RunnerRegistration(r) => r.columns(wide),
            AnyResource::Job(r) => r.columns(wide),
            AnyResource::Run(r) => r.columns(wide),
            AnyResource::QueueEntry(r) => r.columns(wide),
            AnyResource::Event(r) => r.columns(wide),
            AnyResource::Reservation(r) => r.columns(wide),
            AnyResource::Lease(r) => r.columns(wide),
            AnyResource::Capability(r) => r.columns(wide),
            AnyResource::Adapter(r) => r.columns(wide),
        }
    }

    fn cells(&self, wide: bool) -> Vec<String> {
        match self {
            AnyResource::Host(r) => r.cells(wide),
            AnyResource::Instance(r) => r.cells(wide),
            AnyResource::Slot(r) => r.cells(wide),
            AnyResource::RunnerRegistration(r) => r.cells(wide),
            AnyResource::Job(r) => r.cells(wide),
            AnyResource::Run(r) => r.cells(wide),
            AnyResource::QueueEntry(r) => r.cells(wide),
            AnyResource::Event(r) => r.cells(wide),
            AnyResource::Reservation(r) => r.cells(wide),
            AnyResource::Lease(r) => r.cells(wide),
            AnyResource::Capability(r) => r.cells(wide),
            AnyResource::Adapter(r) => r.cells(wide),
        }
    }

    fn identity(&self) -> String {
        AnyResource::identity(self)
    }
}

/// Warning lines demanded by the data itself: degraded conditions and
/// warning-grade slot phases. Renderers write these to stderr; commands may
/// reuse the helper so every surface agrees on what counts as a warning.
#[must_use]
pub fn collect_warnings(resources: &[AnyResource]) -> Vec<String> {
    let mut warnings = Vec::new();
    for resource in resources {
        for condition in &resource.meta().conditions {
            if condition.is_warning() {
                let detail = condition
                    .message
                    .as_deref()
                    .or(condition.reason.as_deref())
                    .unwrap_or("");
                warnings.push(format!(
                    "{} {}={}: {}",
                    resource.identity(),
                    condition.kind,
                    ConditionStatus::False.as_str(),
                    detail
                ));
            }
        }
        if let AnyResource::Slot(slot) = resource
            && slot.phase.is_warning()
        {
            warnings.push(format!(
                "{} phase={}",
                resource.identity(),
                slot.phase.as_str()
            ));
        }
    }
    warnings
}

fn render_one_table(
    kind: &str,
    items: &[&AnyResource],
    wide: bool,
    options: &RenderOptions,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    let columns = items[0].columns(wide);
    let rows: Vec<Vec<String>> = items.iter().map(|item| item.cells(wide)).collect();
    let mut widths: Vec<usize> = columns.iter().map(|column| column.len()).collect();
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let styled = options.color == ColorPolicy::Always;
    let mut header = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            header.push_str("  ");
        }
        header.push_str(column);
        header.push_str(&" ".repeat(widths[index] - column.len()));
    }
    let header = header.trim_end().to_owned();
    if styled {
        writeln!(out, "{ANSI_BOLD}{kind}{ANSI_RESET} {header}")?;
    } else {
        writeln!(out, "{kind} {header}")?;
    }
    for (row_index, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (index, cell) in row.iter().enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            let mut painted = cell.clone();
            if styled
                && columns[index] == "PHASE"
                && let AnyResource::Slot(slot) = items[row_index]
                && slot.phase == SlotPhase::Error
            {
                painted = format!("{ANSI_RED}{cell}{ANSI_RESET}");
            }
            line.push_str(&painted);
            let pad = widths[index].saturating_sub(cell.chars().count());
            line.push_str(&" ".repeat(pad));
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

fn render_table(
    items: &[AnyResource],
    wide: bool,
    options: &RenderOptions,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    let mut sections: Vec<(&'static str, Vec<&AnyResource>)> = Vec::new();
    for resource in items {
        match sections.last_mut() {
            Some((kind, group)) if *kind == resource.kind() => group.push(resource),
            _ => sections.push((resource.kind(), vec![resource])),
        }
    }
    for (section_index, (kind, group)) in sections.iter().enumerate() {
        if section_index > 0 {
            writeln!(out)?;
        }
        render_one_table(kind, group, wide, options, out)?;
    }
    Ok(())
}

/// Render `items` in `format`, payload to `stdout`, warnings to `stderr`.
///
/// Deterministic: machine formats serialize the versioned resources exactly
/// as the model defines them, table layouts derive only from data, and no
/// map iteration order leaks into any output.
///
/// # Errors
/// Propagates underlying writer failures.
pub fn render(
    format: OutputFormat,
    items: &[AnyResource],
    options: &RenderOptions,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::io::Result<()> {
    let color = options.color == ColorPolicy::Always;
    for warning in collect_warnings(items) {
        if color {
            writeln!(stderr, "{ANSI_YELLOW}warning: {warning}{ANSI_RESET}")?;
        } else {
            writeln!(stderr, "warning: {warning}")?;
        }
    }
    match format {
        OutputFormat::Table => render_table(items, false, options, stdout),
        OutputFormat::Wide => render_table(items, true, options, stdout),
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *stdout, &items)?;
            writeln!(&mut *stdout)?;
            Ok(())
        }
        OutputFormat::Yaml => {
            let text = serde_yaml::to_string(&items).map_err(std::io::Error::other)?;
            stdout.write_all(text.as_bytes())
        }
        OutputFormat::Jsonl => {
            for item in items {
                serde_json::to_writer(&mut *stdout, item)?;
                writeln!(&mut *stdout)?;
            }
            Ok(())
        }
        OutputFormat::Name => {
            for item in items {
                writeln!(&mut *stdout, "{}", item.identity())?;
            }
            Ok(())
        }
    }
}
