//! Interactive scan, configure, and generate experience.
//!
//! Filesystem and generation effects stay in the generator core. This module
//! owns only terminal lifecycle, typed worker messages, selection policy, and
//! the application state rendered by [`view`].

mod view;

use std::collections::BTreeSet;
use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::text::Line;
use ratatui::Terminal;
use termrock::crossterm::{CrosstermBackend, Session, SessionOptions};
use termrock::interaction::Outcome;
use termrock::style::{ColorCapability, DesignSystem};
use termrock::widgets::{ListRow, ListState, ScrollAreaState};

use super::{
    apply_generated_write_plan, generated_files, plan_generated_write, scan_repository, Checkout,
    Cli, GeneratedWritePlan, GeneratorError, ProjectConfig, RepositorySource, RunnerMode,
    WriteOutcome,
};

const MIN_WIDTH: u16 = 52;
const MIN_HEIGHT: u16 = 16;

type ScanResult = Result<PreparedProject, String>;
type GenerationResult = Result<GenerationCompletion, String>;

enum GenerationCompletion {
    Finished {
        outcome: WriteOutcome,
        plan: GeneratedWritePlan,
    },
    CheckDrift(GeneratedWritePlan),
    PlanChanged(GeneratedWritePlan),
}

struct PreparedProject {
    checkout: Checkout,
    config: ProjectConfig,
    output_root: std::path::PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Scanning,
    Empty,
    Configure,
    Review,
    Generating,
    Complete,
    CheckDrift,
    Error(FailedOperation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailedOperation {
    Scan,
    Review,
    Generate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    Details,
    Help,
}

struct App {
    cli: Cli,
    target: String,
    phase: Phase,
    receiver: Receiver<ScanResult>,
    generation_receiver: Option<Receiver<GenerationResult>>,
    checkout: Option<Checkout>,
    config: Option<ProjectConfig>,
    output_root: Option<std::path::PathBuf>,
    selector: Option<ListState<String>>,
    scroll: ScrollAreaState,
    files: Option<std::collections::BTreeMap<std::path::PathBuf, String>>,
    plan: Option<GeneratedWritePlan>,
    outcome: Option<super::WriteOutcome>,
    notice: Option<String>,
    error: Option<String>,
    overlay: Option<Overlay>,
    exit: bool,
    terminal_size: (u16, u16),
}

impl App {
    fn new(cli: Cli, receiver: Receiver<ScanResult>) -> Self {
        Self {
            target: cli.target.clone(),
            cli,
            phase: Phase::Scanning,
            receiver,
            generation_receiver: None,
            checkout: None,
            config: None,
            output_root: None,
            selector: None,
            scroll: ScrollAreaState::new().axes(true, false),
            files: None,
            plan: None,
            outcome: None,
            notice: None,
            error: None,
            overlay: None,
            exit: false,
            terminal_size: (MIN_WIDTH, MIN_HEIGHT),
        }
    }

    fn poll_workers(&mut self) -> bool {
        let mut changed = false;
        if self.phase == Phase::Scanning {
            match self.receiver.try_recv() {
                Ok(Ok(prepared)) => {
                    self.finish_scan(prepared);
                    changed = true;
                }
                Ok(Err(error)) => {
                    self.fail(FailedOperation::Scan, error);
                    changed = true;
                }
                Err(TryRecvError::Empty) => (),
                Err(TryRecvError::Disconnected) => {
                    self.fail(
                        FailedOperation::Scan,
                        "the scan worker stopped unexpectedly".to_owned(),
                    );
                    changed = true;
                }
            }
        }
        if self.phase == Phase::Generating {
            let result = self.generation_receiver.as_ref().map(Receiver::try_recv);
            match result {
                Some(Ok(Ok(GenerationCompletion::Finished { outcome, plan }))) => {
                    self.outcome = Some(outcome);
                    self.plan = Some(plan);
                    self.generation_receiver = None;
                    self.phase = Phase::Complete;
                    self.scroll = ScrollAreaState::new().axes(true, false);
                    changed = true;
                }
                Some(Ok(Ok(GenerationCompletion::CheckDrift(plan)))) => {
                    self.plan = Some(plan);
                    self.generation_receiver = None;
                    self.phase = Phase::CheckDrift;
                    self.scroll = ScrollAreaState::new().axes(true, false);
                    changed = true;
                }
                Some(Ok(Ok(GenerationCompletion::PlanChanged(plan)))) => {
                    self.plan = Some(plan);
                    self.generation_receiver = None;
                    self.phase = Phase::Review;
                    self.notice = Some(
                        "Files changed since review · inspect the updated plan before confirming"
                            .to_owned(),
                    );
                    self.scroll = ScrollAreaState::new().axes(true, false);
                    changed = true;
                }
                Some(Ok(Err(error))) => {
                    self.generation_receiver = None;
                    self.fail(FailedOperation::Generate, error);
                    changed = true;
                }
                Some(Err(TryRecvError::Empty)) | None => (),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.generation_receiver = None;
                    self.fail(
                        FailedOperation::Generate,
                        "the generation worker stopped unexpectedly".to_owned(),
                    );
                    changed = true;
                }
            }
        }
        changed
    }

    fn finish_scan(&mut self, prepared: PreparedProject) {
        let ids = prepared
            .config
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>();
        let mut selector = ListState::new(ids.first().cloned());
        selector.enable_multi_select();
        if let Some(selection) = selector.selection_mut() {
            selection.select_all(&ids);
        }
        self.checkout = Some(prepared.checkout);
        self.output_root = Some(prepared.output_root);
        self.config = Some(prepared.config);
        self.selector = Some(selector);
        self.phase = if self
            .config
            .as_ref()
            .is_some_and(|config| config.units.is_empty())
        {
            Phase::Empty
        } else {
            Phase::Configure
        };
    }

    fn fail(&mut self, operation: FailedOperation, error: String) {
        self.phase = Phase::Error(operation);
        self.overlay = None;
        self.error = Some(error);
        self.scroll = ScrollAreaState::new().axes(true, false);
    }

    fn handle_event(&mut self, event: &Event) {
        if let Event::Resize(width, height) = event {
            self.terminal_size = (*width, *height);
            return;
        }
        if !self.terminal_is_supported() && !matches!(event, Event::Key(_)) {
            return;
        }
        match event {
            Event::Key(key) => self.handle_key(*key),
            Event::Mouse(mouse)
                if self.overlay == Some(Overlay::Details)
                    || self.overlay.is_none()
                        && matches!(
                            self.phase,
                            Phase::Review | Phase::Complete | Phase::CheckDrift | Phase::Error(_)
                        ) =>
            {
                self.handle_scroll_mouse(*mouse);
            }
            Event::Mouse(mouse) if self.phase == Phase::Configure => {
                self.handle_mouse(*mouse);
            }
            _ => (),
        }
    }

    fn handle_scroll_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                let _ = self.scroll.scroll_by(-3, 0);
            }
            MouseEventKind::ScrollDown => {
                let _ = self.scroll.scroll_by(3, 0);
            }
            _ => (),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if ignored_key_event(key) {
            return;
        }
        if self.phase == Phase::Generating {
            return;
        }
        if !self.terminal_is_supported() {
            if is_quit_key(key) {
                self.exit = true;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.exit = true;
            return;
        }
        if self.phase == Phase::Configure
            && self
                .selector
                .as_ref()
                .is_some_and(|selector| selector.search_query().is_some())
        {
            self.handle_configure_key(key);
            return;
        }
        if let Some(overlay) = self.overlay {
            if key.code == KeyCode::Esc
                || overlay == Overlay::Details && key.code == KeyCode::Char('i')
                || overlay == Overlay::Help && key.code == KeyCode::Char('?')
            {
                self.overlay = None;
            } else if overlay == Overlay::Details {
                self.handle_scroll_key(key);
            }
            return;
        }
        if key.code == KeyCode::Char('?') {
            self.overlay = Some(Overlay::Help);
            return;
        }
        if key.code == KeyCode::Char('q') {
            self.exit = true;
            return;
        }
        if let Phase::Error(operation) = self.phase {
            self.handle_error_key(operation, key);
            return;
        }
        if self.phase == Phase::CheckDrift {
            match key.code {
                KeyCode::Enter => self.exit = true,
                KeyCode::Esc => self.phase = Phase::Review,
                _ => {
                    self.handle_scroll_key(key);
                }
            }
            return;
        }
        if self.phase == Phase::Complete {
            match key.code {
                KeyCode::Enter => self.exit = true,
                KeyCode::Char('r') => self.retry_scan(),
                _ => {
                    self.handle_scroll_key(key);
                }
            }
            return;
        }
        if self.phase == Phase::Review {
            match key.code {
                KeyCode::Enter => self.start_generation(),
                KeyCode::Esc => self.phase = Phase::Configure,
                _ => {
                    self.handle_scroll_key(key);
                }
            }
            return;
        }
        if self.phase == Phase::Empty {
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('r')) {
                self.retry_scan();
            } else if key.code == KeyCode::Esc {
                self.exit = true;
            }
            return;
        }
        if self.phase != Phase::Configure {
            return;
        }
        self.handle_configure_key(key);
    }

    fn handle_error_key(&mut self, operation: FailedOperation, key: KeyEvent) {
        match (operation, key.code) {
            (FailedOperation::Scan, KeyCode::Enter | KeyCode::Char('r')) => self.retry_scan(),
            (FailedOperation::Review, KeyCode::Enter | KeyCode::Char('r')) => {
                self.prepare_review();
            }
            (FailedOperation::Generate, KeyCode::Enter | KeyCode::Char('r')) => {
                self.start_generation();
            }
            (FailedOperation::Review, KeyCode::Esc) => {
                self.error = None;
                self.phase = Phase::Configure;
            }
            (FailedOperation::Generate, KeyCode::Esc) => {
                self.error = None;
                self.phase = Phase::Review;
            }
            (FailedOperation::Scan, KeyCode::Esc) => self.exit = true,
            _ => {
                self.handle_scroll_key(key);
            }
        }
    }

    fn handle_scroll_key(&mut self, key: KeyEvent) {
        let routed = match key.code {
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
            }
            _ => key,
        };
        let _ = self.scroll.handle_key(routed.into());
    }

    fn handle_configure_key(&mut self, key: KeyEvent) {
        let searching = self
            .selector
            .as_ref()
            .is_some_and(|selector| selector.search_query().is_some());
        if !searching && key.code == KeyCode::Enter {
            self.prepare_review();
            return;
        }
        if !searching && key.code == KeyCode::Char('i') {
            self.scroll = ScrollAreaState::new().axes(true, false);
            self.overlay = Some(Overlay::Details);
            return;
        }
        let Some(selector) = self.selector.as_mut() else {
            return;
        };
        let query = selector.search_query().map(str::to_owned);
        let rows = self
            .config
            .as_ref()
            .map(|config| rows_for(config, query.as_deref()))
            .unwrap_or_default();
        let before = selected_ids(selector);
        let routed = match key.code {
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::SHIFT => {
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
            }
            _ => key,
        };
        let outcome = selector.handle_key(&rows, routed.into());
        if let Outcome::CheckToggled(id) = outcome {
            let checked = selector
                .selection()
                .is_some_and(|selection| selection.is_checked(&id));
            self.notice = Some(selection_status(
                self.config.as_ref(),
                selector,
                &before,
                &id,
                checked,
            ));
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        let query = self
            .selector
            .as_ref()
            .and_then(ListState::search_query)
            .map(str::to_owned);
        let rows = self
            .config
            .as_ref()
            .map(|config| rows_for(config, query.as_deref()))
            .unwrap_or_default();
        let (before, outcome) = {
            let Some(selector) = self.selector.as_mut() else {
                return;
            };
            let before = selected_ids(selector);
            let outcome = match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    selector.click(Position::new(mouse.column, mouse.row))
                }
                MouseEventKind::ScrollUp => {
                    let _ = selector.scroll_by(-3, rows.len());
                    Outcome::Changed
                }
                MouseEventKind::ScrollDown => {
                    let _ = selector.scroll_by(3, rows.len());
                    Outcome::Changed
                }
                _ => Outcome::Ignored,
            };
            (before, outcome)
        };
        if let Outcome::CheckToggled(id) = outcome
            && let Some(selector) = self.selector.as_mut()
        {
            let checked = selector
                .selection()
                .is_some_and(|selection| selection.is_checked(&id));
            self.notice = Some(selection_status(
                self.config.as_ref(),
                selector,
                &before,
                &id,
                checked,
            ));
        }
    }

    fn retry_scan(&mut self) {
        let Ok(source) = RepositorySource::parse(&self.target) else {
            return;
        };
        let receiver = spawn_scan(source, self.cli.runners, self.cli.output.clone());
        self.receiver = receiver;
        self.phase = Phase::Scanning;
        self.overlay = None;
        self.error = None;
        self.notice = None;
        self.outcome = None;
        self.files = None;
        self.plan = None;
    }

    fn prepare_review(&mut self) {
        let Some(config) = self.selected_config() else {
            return;
        };
        let files = generated_files(&config);
        let Some(output_root) = self.output_root.as_deref() else {
            self.fail(
                FailedOperation::Review,
                "output root was not resolved".to_owned(),
            );
            return;
        };
        let plan = match plan_generated_write(output_root, &files) {
            Ok(plan) => plan,
            Err(error) => {
                self.fail(FailedOperation::Review, error.to_string());
                return;
            }
        };
        self.files = Some(files);
        self.plan = Some(plan);
        self.error = None;
        self.scroll = ScrollAreaState::new().axes(true, false);
        self.phase = Phase::Review;
    }

    fn start_generation(&mut self) {
        let Some(config) = self.selected_config() else {
            return;
        };
        let Some(output_root) = self.output_root.clone() else {
            self.fail(
                FailedOperation::Generate,
                "output root was not resolved".to_owned(),
            );
            return;
        };
        let files = generated_files(&config);
        let files_for_worker = files.clone();
        let dry_run = self.cli.dry_run;
        let check = self.cli.check;
        let force = self.cli.force;
        let Some(reviewed_plan) = self.plan.clone() else {
            self.fail(
                FailedOperation::Review,
                "generation plan was not prepared".to_owned(),
            );
            return;
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = complete_generation(
                &output_root,
                &files_for_worker,
                dry_run,
                check,
                force,
                &reviewed_plan,
            )
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.files = Some(files);
        self.generation_receiver = Some(receiver);
        self.phase = Phase::Generating;
        self.notice = None;
    }

    fn selected_config(&mut self) -> Option<ProjectConfig> {
        let config = self.config.as_ref()?.clone();
        let selected = self
            .selector
            .as_ref()
            .map(|selector| selected_ids(selector).into_iter().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        if selected.is_empty() {
            self.notice = Some(String::from("Select at least one check before continuing"));
            return None;
        }
        let mut filtered = config.clone();
        filtered.units.retain(|unit| selected.contains(&unit.id));
        if filtered
            .units
            .iter()
            .any(|unit| unit.depends_on.iter().any(|id| !selected.contains(id)))
        {
            self.notice = Some(String::from(
                "Selection has a disabled dependency; restore it before continuing",
            ));
            return None;
        }
        if selected.len() != config.units.len() {
            filtered.release = None;
            filtered.release_enabled = false;
            filtered.release_reason = String::from(
                "Release workflows disabled because the TUI selected a partial configuration.",
            );
            filtered
                .workflow_files
                .retain(|file| file != "release.yml" && file != "preview.yml");
        }
        Some(filtered)
    }

    fn render(&mut self, frame: &mut ratatui::Frame<'_>, system: &DesignSystem) {
        let area = frame.area();
        self.terminal_size = (area.width, area.height);
        view::render(frame, self, system);
    }

    fn terminal_is_supported(&self) -> bool {
        self.terminal_size.0 >= MIN_WIDTH && self.terminal_size.1 >= MIN_HEIGHT
    }
}

fn complete_generation(
    output_root: &std::path::Path,
    files: &std::collections::BTreeMap<std::path::PathBuf, String>,
    dry_run: bool,
    check: bool,
    force: bool,
    reviewed_plan: &GeneratedWritePlan,
) -> Result<GenerationCompletion, GeneratorError> {
    let plan = plan_generated_write(output_root, files)?;
    if &plan != reviewed_plan {
        return Ok(GenerationCompletion::PlanChanged(plan));
    }
    if check && plan.has_drift() {
        return Ok(GenerationCompletion::CheckDrift(plan));
    }
    let outcome = apply_generated_write_plan(output_root, files, dry_run, check, force, &plan)?;
    Ok(GenerationCompletion::Finished { outcome, plan })
}

fn rows_for(config: &ProjectConfig, query: Option<&str>) -> Vec<ListRow<'static, String>> {
    let query = query.map(str::to_lowercase);
    config
        .units
        .iter()
        .filter(|unit| {
            query.as_ref().is_none_or(|query| {
                unit.id.to_lowercase().contains(query)
                    || unit.label.to_lowercase().contains(query)
                    || unit.kind.label().to_lowercase().contains(query)
            })
        })
        .map(|unit| {
            ListRow::item(
                unit.id.clone(),
                Line::from(format!("{} · {}", unit.id, unit.kind.label())),
            )
        })
        .collect()
}

fn selected_ids(selector: &ListState<String>) -> Vec<String> {
    selector
        .selection()
        .map_or_else(Vec::new, |selection| selection.checked().to_vec())
}

fn replace_selection(selector: &mut ListState<String>, selected: &[String]) {
    if let Some(selection) = selector.selection_mut() {
        selection.clear();
        selection.select_all(selected);
    }
}

fn dependent_ids(config: &ProjectConfig, selected: &[String], candidate: &str) -> Vec<String> {
    config
        .units
        .iter()
        .filter(|unit| unit.id != candidate && selected.contains(&unit.id))
        .filter(|unit| unit.depends_on.iter().any(|id| id == candidate))
        .map(|unit| unit.id.clone())
        .collect()
}

fn selection_status(
    config: Option<&ProjectConfig>,
    selector: &mut ListState<String>,
    before: &[String],
    id: &str,
    checked: bool,
) -> String {
    if checked {
        let dependencies = config.map_or_else(Vec::new, |config| {
            required_dependencies(config, &selected_ids(selector), id)
        });
        if dependencies.is_empty() {
            format!("Enabled {id}")
        } else {
            if let Some(selection) = selector.selection_mut() {
                selection.select_all(&dependencies);
            }
            format!(
                "Enabled {id} · also enabled dependencies {}",
                dependencies.join(", ")
            )
        }
    } else {
        let blocked = config
            .map(|config| dependent_ids(config, before, id))
            .unwrap_or_default();
        if blocked.is_empty() {
            format!("Disabled {id}")
        } else {
            replace_selection(selector, before);
            format!("Kept {} enabled · required by {}", id, blocked.join(", "))
        }
    }
}

fn ignored_key_event(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Release
        || key.kind == KeyEventKind::Repeat
            && !matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Char('j' | 'k')
            )
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
}

fn animation_interval(phase: Phase) -> Option<Duration> {
    matches!(phase, Phase::Scanning | Phase::Generating).then_some(Duration::from_millis(80))
}

fn required_dependencies(config: &ProjectConfig, selected: &[String], id: &str) -> Vec<String> {
    let mut present = selected.iter().cloned().collect::<BTreeSet<_>>();
    let mut required = BTreeSet::new();
    let mut pending = config
        .units
        .iter()
        .find(|unit| unit.id == id)
        .map_or_else(Vec::new, |unit| unit.depends_on.clone());
    while let Some(dependency) = pending.pop() {
        if present.insert(dependency.clone()) {
            required.insert(dependency.clone());
            if let Some(unit) = config.units.iter().find(|unit| unit.id == dependency) {
                pending.extend(unit.depends_on.iter().cloned());
            }
        }
    }
    required.into_iter().collect()
}

fn spawn_scan(
    source: RepositorySource,
    runners: RunnerMode,
    output: Option<std::path::PathBuf>,
) -> Receiver<ScanResult> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| {
            let checkout = source.checkout()?;
            let config = scan_repository(checkout.path(), runners)?;
            let output_root = match output.as_deref() {
                Some(path) => super::resolve_output_path(path)?,
                None => source.output_root(checkout.path())?,
            };
            Ok(PreparedProject {
                checkout,
                config,
                output_root,
            })
        })()
        .map_err(|error: GeneratorError| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

pub(super) fn run(cli: &Cli) -> Result<(), GeneratorError> {
    let source = RepositorySource::parse(&cli.target)?;
    let receiver = spawn_scan(source, cli.runners, cli.output.clone());
    let mut app = App::new(cli.clone(), receiver);
    let system = design_system();
    let mut session = Session::enter(io::stdout(), SessionOptions::default())
        .map_err(|error| GeneratorError::usage(format!("start TUI session: {error}")))?;
    let backend = CrosstermBackend::new(session.writer_mut());
    let mut terminal = Terminal::new(backend)
        .map_err(|error| GeneratorError::usage(format!("start terminal renderer: {error}")))?;
    let result = event_loop(&mut terminal, &mut app, &system);
    drop(terminal);
    session
        .restore()
        .map_err(|error| GeneratorError::usage(format!("restore terminal session: {error}")))?;
    result
}

fn design_system() -> DesignSystem {
    let capability = match ColorCapability::detect_from_env() {
        ColorCapability::Monochrome => ColorCapability::Monochrome,
        _ => ColorCapability::Ansi16,
    };
    DesignSystem::phosphor().quantize(capability)
}

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    system: &DesignSystem,
) -> Result<(), GeneratorError> {
    let mut redraw = true;
    loop {
        redraw |= app.poll_workers();
        if redraw {
            terminal
                .draw(|frame| app.render(frame, system))
                .map_err(|error| GeneratorError::usage(format!("draw TUI frame: {error}")))?;
            redraw = false;
        }
        if app.exit {
            if app.phase == Phase::CheckDrift {
                let differences = app
                    .plan
                    .as_ref()
                    .map_or_else(Vec::new, GeneratedWritePlan::differences);
                return Err(GeneratorError::usage(format!(
                    "generated files differ: {}; rerun generate",
                    super::display_paths(differences.iter()),
                )));
            }
            if matches!(app.phase, Phase::Error(_)) {
                return Err(GeneratorError::usage(
                    app.error
                        .clone()
                        .unwrap_or_else(|| "TUI operation failed".to_owned()),
                ));
            }
            return Ok(());
        }
        if let Some(interval) = animation_interval(app.phase) {
            if !event::poll(interval)
                .map_err(|error| GeneratorError::usage(format!("poll TUI input: {error}")))?
            {
                continue;
            }
            let event = event::read()
                .map_err(|error| GeneratorError::usage(format!("read TUI input: {error}")))?;
            app.handle_event(&event);
            redraw = true;
        } else {
            let event = event::read()
                .map_err(|error| GeneratorError::usage(format!("read TUI input: {error}")))?;
            app.handle_event(&event);
            redraw = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    };

    use super::{
        dependent_ids, required_dependencies, selection_status, App, FailedOperation, Overlay,
        Phase,
    };

    fn unit(id: &str, dependencies: &[&str]) -> crate::Unit {
        crate::Unit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: crate::UnitKind::Rust,
            root: ".".to_owned(),
            watch: Vec::new(),
            pr_commands: vec!["cargo test".to_owned()],
            full_commands: vec!["cargo test --all-targets".to_owned()],
            depends_on: dependencies
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
            cache: None,
            tool_version: None,
        }
    }

    fn config() -> crate::ProjectConfig {
        crate::ProjectConfig {
            repository: "test".to_owned(),
            profile: crate::RepositoryProfile::Generic,
            analysis: crate::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["ci-pr.yml".to_owned(), "release.yml".to_owned()],
            notes: Vec::new(),
            default_branch: "main".to_owned(),
            runners: crate::RunnerMode::Github,
            github_runner: "ubuntu-24.04".to_owned(),
            velnor_labels: Vec::new(),
            release_enabled: true,
            release_reason: String::new(),
            release: None,
            units: vec![
                unit("core", &[]),
                unit("middle", &["core"]),
                unit("app", &["middle"]),
            ],
        }
    }

    fn configured_app() -> App {
        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut app = App::new(
            crate::Cli {
                target: ".".to_owned(),
                default_branch: None,
                output: None,
                runners: crate::RunnerMode::Github,
                dry_run: true,
                check: false,
                force: false,
                plain: false,
            },
            receiver,
        );
        let config = config();
        let ids = config
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>();
        let mut selector = termrock::widgets::ListState::new(ids.first().cloned());
        selector.enable_multi_select();
        if let Some(selection) = selector.selection_mut() {
            selection.select_all(&ids);
        }
        app.config = Some(config);
        app.selector = Some(selector);
        app.output_root = Some(PathBuf::from("."));
        app.phase = Phase::Configure;
        app
    }

    #[test]
    fn dependencies_block_disabling_required_units() {
        let config = config();
        assert_eq!(
            dependent_ids(
                &config,
                &["core".to_owned(), "middle".to_owned(), "app".to_owned()],
                "core"
            ),
            vec!["middle"]
        );
    }

    #[test]
    fn enabling_check_adds_full_dependency_closure() {
        let config = config();
        assert_eq!(
            required_dependencies(&config, &["app".to_owned()], "app"),
            vec!["core", "middle"]
        );
        let mut selector = termrock::widgets::ListState::new(Some("app".to_owned()));
        selector.enable_multi_select();
        if let Some(selection) = selector.selection_mut() {
            selection.select_all(&["app".to_owned()]);
        }
        let message = selection_status(
            Some(&config),
            &mut selector,
            &["app".to_owned()],
            "app",
            true,
        );
        assert_eq!(super::selected_ids(&selector).len(), 3);
        assert!(message.contains("core, middle"));
    }

    #[test]
    fn configure_enter_opens_review_without_writing() {
        let mut app = configured_app();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.phase, Phase::Review);
        assert!(app.generation_receiver.is_none());
        assert!(app.files.is_some());
    }

    #[test]
    fn too_small_terminal_blocks_hidden_actions_until_resize() {
        let mut app = configured_app();
        app.handle_event(&Event::Resize(super::MIN_WIDTH - 1, super::MIN_HEIGHT));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.phase, Phase::Configure);
        assert!(app.generation_receiver.is_none());

        app.handle_event(&Event::Resize(super::MIN_WIDTH, super::MIN_HEIGHT));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.phase, Phase::Review);
    }

    #[test]
    fn too_small_terminal_keeps_global_quit_available() {
        let mut app = configured_app();
        app.handle_event(&Event::Resize(super::MIN_WIDTH - 1, super::MIN_HEIGHT));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));
        assert!(app.exit);

        let mut app = configured_app();
        app.handle_event(&Event::Resize(super::MIN_WIDTH, super::MIN_HEIGHT - 1));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.exit);
    }

    #[test]
    fn undocumented_g_does_not_advance_screens() {
        let mut app = configured_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.phase, Phase::Configure);
        app.phase = Phase::Review;
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.phase, Phase::Review);
        assert!(app.generation_receiver.is_none());
    }

    #[test]
    fn check_drift_is_a_result_state_not_a_write_error() {
        let mut app = configured_app();
        let plan = crate::GeneratedWritePlan {
            files: vec![crate::PlannedFile {
                path: PathBuf::from(crate::OWNERSHIP_STATE),
                action: crate::PlannedAction::Update,
                preimage: crate::FilePreimage::Missing,
            }],
            changed: Vec::new(),
            stale: Vec::new(),
            conflicts: Vec::new(),
            ownership_present: true,
            ownership_needs_refresh: true,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(sender
            .send(Ok(super::GenerationCompletion::CheckDrift(plan)))
            .is_ok());
        app.generation_receiver = Some(receiver);
        app.phase = Phase::Generating;

        app.poll_workers();
        assert_eq!(app.phase, Phase::CheckDrift);
        assert!(app.error.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.exit);
    }

    #[test]
    fn changed_preflight_returns_to_review_for_confirmation() {
        let mut app = configured_app();
        let updated_plan = crate::GeneratedWritePlan {
            files: vec![crate::PlannedFile {
                path: PathBuf::from(".github/workflows/ci-pr.yml"),
                action: crate::PlannedAction::Update,
                preimage: crate::FilePreimage::Missing,
            }],
            changed: vec![PathBuf::from(".github/workflows/ci-pr.yml")],
            stale: Vec::new(),
            conflicts: vec![PathBuf::from(".github/workflows/ci-pr.yml")],
            ownership_present: true,
            ownership_needs_refresh: false,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(sender
            .send(Ok(super::GenerationCompletion::PlanChanged(
                updated_plan.clone()
            )))
            .is_ok());
        app.generation_receiver = Some(receiver);
        app.phase = Phase::Generating;

        app.poll_workers();
        assert_eq!(app.phase, Phase::Review);
        assert_eq!(app.plan, Some(updated_plan));
        assert!(app
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("changed since review")));
    }

    #[test]
    fn confirmation_rejects_filesystem_changes_after_review() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "github-actions-unified-confirmation-{}-{nonce}",
            std::process::id()
        ));
        let relative = PathBuf::from(".github/workflows/ci-pr.yml");
        let content = format!("{}name: CI\n", crate::GENERATED_HEADER);
        let files = BTreeMap::from([(relative.clone(), content.clone())]);
        assert!(fs::create_dir_all(root.join(".github/workflows")).is_ok());
        let reviewed = crate::plan_generated_write(&root, &files);
        assert!(reviewed.is_ok());
        let Some(reviewed) = reviewed.ok() else {
            return;
        };
        assert!(fs::write(root.join(&relative), content).is_ok());

        let result = super::complete_generation(&root, &files, true, false, false, &reviewed);
        assert!(matches!(
            result,
            Ok(super::GenerationCompletion::PlanChanged(_))
        ));
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn release_and_repeat_activation_events_are_ignored() {
        let mut app = configured_app();
        let mut release = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_key(release);
        assert_eq!(app.phase, Phase::Configure);

        let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle_key(repeat);
        assert_eq!(app.phase, Phase::Configure);
    }

    #[test]
    fn generation_cannot_exit_while_worker_may_be_writing() {
        let mut app = configured_app();
        app.phase = Phase::Generating;
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.exit);
    }

    #[test]
    fn failures_reset_scroll_and_jk_scroll_every_scroll_view() {
        let mut app = configured_app();
        app.scroll.set_viewport(40, 5);
        app.scroll.set_content_size(40, 30);
        let _ = app.scroll.scroll_by(12, 0);
        assert!(app.scroll.offset_y() > 0);
        app.fail(FailedOperation::Generate, "long error".to_owned());
        assert_eq!(app.scroll.offset_y(), 0);

        app.scroll.set_viewport(40, 5);
        app.scroll.set_content_size(40, 30);
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(app.scroll.offset_y() > 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.scroll.offset_y(), 0);
    }

    #[test]
    fn only_loading_phases_use_timed_polling() {
        assert!(super::animation_interval(Phase::Scanning).is_some());
        assert!(super::animation_interval(Phase::Generating).is_some());
        assert!(super::animation_interval(Phase::Configure).is_none());
        assert!(super::animation_interval(Phase::Review).is_none());
        assert!(super::animation_interval(Phase::Complete).is_none());
    }

    #[test]
    fn write_error_preserves_review_and_help_traps_keys() {
        let mut app = configured_app();
        app.phase = Phase::Error(FailedOperation::Generate);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.phase, Phase::Review);

        app.overlay = Some(Overlay::Help);
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.exit);
        assert_eq!(app.overlay, Some(Overlay::Help));
    }

    #[test]
    fn details_overlay_owns_mouse_wheel() {
        let mut app = configured_app();
        app.overlay = Some(Overlay::Details);
        app.scroll.set_viewport(40, 5);
        app.scroll.set_content_size(40, 30);
        app.handle_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.scroll.offset_y() > 0);
    }

    #[test]
    fn partial_selection_disables_release_outputs() {
        let mut app = configured_app();
        if let Some(selector) = app.selector.as_mut() {
            super::replace_selection(selector, &["core".to_owned()]);
        }
        let selected = app.selected_config();
        assert!(selected.is_some());
        let selected = selected.unwrap_or_else(config);
        assert!(!selected.release_enabled);
        assert!(!selected
            .workflow_files
            .iter()
            .any(|file| file == "release.yml"));
    }
}
