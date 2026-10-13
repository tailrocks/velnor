//! Pure terminal projection for the generator TUI.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;
use termrock::style::{DesignSystem, Glyph, PanelChrome, Role};
use termrock::widgets::{
    place_keyboard_help, Banner, EmptyState, HelpEntry, KeyboardHelp, KeyboardHelpSize,
    KeyboardHelpState, List, LoadingView, Panel, ScrollArea, Severity,
};

use super::{rows_for, selected_ids, App, FailedOperation, Overlay, Phase};
use crate::{PlannedAction, Unit, WriteOutcome};

pub(super) fn render(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem) {
    let area = frame.area();
    if area.width < super::MIN_WIDTH || area.height < super::MIN_HEIGHT {
        render_too_small(frame, app, system);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    render_title(frame, app, system, rows[0]);
    match app.phase {
        Phase::Scanning => render_scanning(frame, app, system, rows[1]),
        Phase::Empty => render_empty(frame, app, system, rows[1]),
        Phase::Configure => render_configure(frame, app, system, rows[1]),
        Phase::Review => render_review(frame, app, system, rows[1]),
        Phase::Generating => render_generating(frame, app, system, rows[1]),
        Phase::Complete => render_complete(frame, app, system, rows[1]),
        Phase::CheckDrift => render_check_drift(frame, app, system, rows[1]),
        Phase::Error(operation) => render_error(frame, app, system, rows[1], operation),
    }
    render_help(frame, app, system, rows[2], false);
    match app.overlay {
        Some(Overlay::Details) => render_details(frame, app, system),
        Some(Overlay::Help) => render_help(frame, app, system, area, true),
        None => (),
    }
}

fn render_title(frame: &mut Frame<'_>, app: &App, system: &DesignSystem, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "GitHub Actions",
                system.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", system.style(Role::TextMuted)),
            Span::styled(app.target.as_str(), system.style(Role::TextMuted)),
        ])),
        area,
    );
}

fn render_scanning(frame: &mut Frame<'_>, app: &App, system: &DesignSystem, area: Rect) {
    let target = format!("Reading checked-in metadata from {}", app.target);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(area);
    frame.render_widget(
        LoadingView::new("Scanning project", system.glyphs.loading(), system),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(target, system.style(Role::Text))),
            Line::from(Span::styled(
                "No project commands are run",
                system.style(Role::TextMuted),
            )),
        ])
        .alignment(ratatui::layout::Alignment::Center),
        rows[1],
    );
}

fn render_configure(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem, area: Rect) {
    let selected = app
        .selector
        .as_ref()
        .map_or(0, |selector| selected_ids(selector).len());
    let total = app.config.as_ref().map_or(0, |config| config.units.len());
    let query = app
        .selector
        .as_ref()
        .and_then(|selector| selector.search_query());
    let summary = if let Some(query) = query {
        let matches = app
            .config
            .as_ref()
            .map_or(0, |config| rows_for(config, Some(query)).len());
        format!("Filter /{query}  ·  {matches} matches")
    } else if selected == total {
        format!("All {total} detected checks selected")
    } else {
        format!("{selected} of {total} checks selected")
    };
    let notice_height = u16::from(app.notice.is_some());
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(notice_height),
            Constraint::Min(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Choose checks",
                system.style(Role::Text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(summary, system.style(Role::TextMuted))),
        ]),
        rows[0],
    );
    if let Some(notice) = app.notice.as_deref() {
        frame.render_widget(Banner::new(notice, Severity::Info, system), rows[1]);
    }
    if let (Some(config), Some(selector)) = (&app.config, &mut app.selector) {
        let query = selector.search_query();
        let list_rows = rows_for(config, query);
        frame.render_stateful_widget(
            List::new(&list_rows, system)
                .focused(true)
                .empty_message(Line::from("No checks match this filter")),
            rows[2],
            selector,
        );
    }
}

fn render_empty(frame: &mut Frame<'_>, app: &App, system: &DesignSystem, area: Rect) {
    let context = format!("Inspected {}", app.target);
    frame.render_widget(
        EmptyState::new("No supported project found", system)
            .explanation("No supported manifests or lockfiles found")
            .context(&context),
        area,
    );
}

fn render_review(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem, area: Rect) {
    let selected = app
        .selector
        .as_ref()
        .map_or(0, |selector| selected_ids(selector).len());
    let total = app.config.as_ref().map_or(0, |config| config.units.len());
    let output = app
        .output_root
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string());
    let mut lines = vec![
        Line::from(Span::styled(
            review_title(app),
            system.style(Role::Text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Output  ", system.style(Role::TextMuted)),
            Span::raw(output),
        ]),
        Line::from(vec![
            Span::styled("Runners ", system.style(Role::TextMuted)),
            Span::raw(app.cli.runners.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Checks  ", system.style(Role::TextMuted)),
            Span::raw(format!("{selected} selected")),
        ]),
    ];
    if selected != total {
        lines.push(Line::from(Span::styled(
            "! Partial selection disables release and preview workflows",
            system.style(Role::Warning),
        )));
    }
    if let Some(notice) = app.notice.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("! {notice}"),
            system.style(Role::Warning),
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Planned files  ",
                system.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(plan_summary(app), system.style(Role::TextMuted)),
        ]),
    ]);
    if !app.cli.dry_run
        && !app.cli.check
        && !app.cli.force
        && app
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.conflicts.is_empty())
    {
        lines.push(Line::from(Span::styled(
            "! Existing generated files require --force before writing",
            system.style(Role::Warning),
        )));
    }
    push_file_lines(&mut lines, app, system);
    if let Some(config) = &app.config
        && !config.analysis.limitations.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("! {} scan limitations", config.analysis.limitations.len()),
            system.style(Role::Warning),
        )));
        lines.extend(config.analysis.limitations.iter().map(|limitation| {
            Line::from(Span::styled(
                format!("  {} {limitation}", system.glyphs.bullet()),
                system.style(Role::TextMuted),
            ))
        }));
    }
    render_scrolled_lines(frame, app, system, area, lines);
}

fn review_title(app: &App) -> &'static str {
    if app.cli.check {
        "Ready to check generated files"
    } else if app.cli.dry_run {
        "Ready to preview changes"
    } else {
        "Ready to generate CI"
    }
}

fn render_generating(frame: &mut Frame<'_>, app: &App, system: &DesignSystem, area: Rect) {
    let label = if app.cli.check {
        "Checking generated files"
    } else if app.cli.dry_run {
        "Previewing changes"
    } else {
        "Writing generated files"
    };
    frame.render_widget(
        LoadingView::new(label, system.glyphs.loading(), system),
        area,
    );
}

fn render_complete(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem, area: Rect) {
    let output = app
        .output_root
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string());
    let mut lines = vec![
        Line::from(Span::styled(
            completion_title(app),
            system.style(Role::Success).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(plan_summary(app), system.style(Role::Text))),
        Line::from(Span::styled(output, system.style(Role::TextMuted))),
        Line::from(""),
    ];
    push_file_lines(&mut lines, app, system);
    render_scrolled_lines(frame, app, system, area, lines);
}

fn completion_title(app: &App) -> &'static str {
    match app.outcome.as_ref() {
        Some(WriteOutcome::Written { .. }) => "✓ CI generated",
        Some(WriteOutcome::DryRun(_)) => "✓ Preview complete",
        Some(WriteOutcome::Unchanged) => "✓ CI already current",
        None => "✓ Complete",
    }
}

fn plan_summary(app: &App) -> String {
    let Some(plan) = app.plan.as_ref() else {
        return "Operation finished".to_owned();
    };
    let created = plan
        .files
        .iter()
        .filter(|file| file.action == PlannedAction::Create)
        .count();
    let updated = plan
        .files
        .iter()
        .filter(|file| file.action == PlannedAction::Update)
        .count();
    let deleted = plan
        .files
        .iter()
        .filter(|file| file.action == PlannedAction::Delete)
        .count();
    if created + updated + deleted == 0 {
        "No changes needed".to_owned()
    } else {
        format!("{created} create · {updated} update · {deleted} delete")
    }
}

fn push_file_lines(lines: &mut Vec<Line<'static>>, app: &App, system: &DesignSystem) {
    if let Some(plan) = &app.plan {
        for file in plan
            .files
            .iter()
            .filter(|file| file.action != PlannedAction::Same)
        {
            let (marker, role) = match file.action {
                PlannedAction::Create => ("+ CREATE ", Role::Success),
                PlannedAction::Update => ("~ UPDATE ", Role::Warning),
                PlannedAction::Delete => ("- DELETE ", Role::Danger),
                PlannedAction::Same => ("= SAME   ", Role::TextMuted),
            };
            lines.push(Line::from(vec![
                Span::styled(marker, system.style(role)),
                Span::raw(file.path.display().to_string()),
            ]));
        }
    }
}

fn render_check_drift(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            "! Generated files differ",
            system.style(Role::Warning).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(plan_summary(app), system.style(Role::Text))),
        Line::from(Span::styled(
            "Run without --check to apply these changes",
            system.style(Role::TextMuted),
        )),
        Line::from(""),
    ];
    push_file_lines(&mut lines, app, system);
    render_scrolled_lines(frame, app, system, area, lines);
}

fn render_error(
    frame: &mut Frame<'_>,
    app: &mut App,
    system: &DesignSystem,
    area: Rect,
    operation: FailedOperation,
) {
    let (summary, source) = match operation {
        FailedOperation::Scan => ("Project scan failed", "scan"),
        FailedOperation::Review => ("Review unavailable", "preflight"),
        FailedOperation::Generate => ("Generation failed", "write"),
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("{} {summary}", system.glyphs.resolve(Glyph::Error).text),
            system.style(Role::Danger).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("Source: {source}"),
            system.style(Role::TextMuted),
        )),
        Line::from(""),
        Line::from(app.error.as_deref().unwrap_or("Unknown error").to_owned()),
    ];
    render_scrolled_lines(frame, app, system, area, lines);
}

fn render_details(frame: &mut Frame<'_>, app: &mut App, system: &DesignSystem) {
    let area = centered_rect(frame.area(), 78, 20);
    let panel = Panel::new(system)
        .title("Check details")
        .footer("↑↓/jk scroll  ·  Esc close")
        .emphasis(PanelChrome::Focused)
        .overlay(true);
    let inner = panel.inner(area);
    frame.render_widget(Clear, frame.area());
    frame.render_widget(panel, area);
    let lines = selected_unit(app, system);
    render_scrolled_lines(frame, app, system, inner, lines);
}

fn selected_unit(app: &App, system: &DesignSystem) -> Vec<Line<'static>> {
    let selected_id = app.selector.as_ref().and_then(|selector| {
        selector.selected().or_else(|| {
            selector
                .selection()
                .and_then(|selection| selection.checked().first())
        })
    });
    let Some(unit) = app.config.as_ref().and_then(|config| {
        selected_id.and_then(|id| config.units.iter().find(|unit| &unit.id == id))
    }) else {
        return vec![Line::from("No check focused")];
    };
    unit_details(unit, system)
}

fn unit_details(unit: &Unit, system: &DesignSystem) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            unit.label.clone(),
            system.style(Role::Text).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("{}  {}", unit.kind.label(), unit.root)),
    ];
    if !unit.depends_on.is_empty() {
        lines.push(Line::from(format!(
            "Depends on  {}",
            unit.depends_on.join(", ")
        )));
    }
    if let Some(version) = &unit.tool_version {
        lines.push(Line::from(format!("Tool  {version}")));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled("Pull requests", system.style(Role::TextMuted))),
    ]);
    lines.extend(
        unit.pr_commands
            .iter()
            .map(|command| Line::from(format!("{} {command}", system.glyphs.bullet()))),
    );
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Main and scheduled runs",
            system.style(Role::TextMuted),
        )),
    ]);
    lines.extend(
        unit.full_commands
            .iter()
            .map(|command| Line::from(format!("{} {command}", system.glyphs.bullet()))),
    );
    lines
}

fn render_scrolled_lines(
    frame: &mut Frame<'_>,
    app: &mut App,
    system: &DesignSystem,
    area: Rect,
    lines: Vec<Line<'static>>,
) {
    let scroll_area = ScrollArea::new(system);
    app.scroll.set_viewport(area.width, area.height);
    let initial_height = wrapped_height(&lines, area.width.max(1));
    app.scroll.set_content_size(area.width, initial_height);
    let body = scroll_area.body_area(area, &app.scroll);
    if body.width != area.width {
        let wrapped_height = wrapped_height(&lines, body.width.max(1));
        app.scroll.set_viewport(body.width, body.height);
        app.scroll.set_content_size(body.width, wrapped_height);
    }
    let paragraph = Paragraph::new(lines)
        .style(system.style(Role::Text))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll.offset_y(), 0));
    frame.render_widget(paragraph, body);
    scroll_area.render_bars(area, frame.buffer_mut(), &app.scroll);
}

fn wrapped_height(lines: &[Line<'_>], width: u16) -> u16 {
    let paragraph = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
    u16::try_from(paragraph.line_count(width.max(1))).unwrap_or(u16::MAX)
}

fn render_help(frame: &mut Frame<'_>, app: &App, system: &DesignSystem, area: Rect, modal: bool) {
    let entries = help_entries(app);
    let mut state = if modal {
        KeyboardHelpState::modal()
    } else {
        KeyboardHelpState::new()
    };
    let target = if modal {
        place_keyboard_help(area, KeyboardHelpSize::default())
    } else {
        area
    };
    if modal {
        frame.render_widget(Clear, area);
    }
    frame.render_stateful_widget(
        KeyboardHelp::new(&entries, system).title("Keyboard"),
        target,
        &mut state,
    );
}

fn help_entries(app: &App) -> Vec<HelpEntry> {
    if app.overlay == Some(Overlay::Details) {
        return vec![HelpEntry::new("close", "Overlay", "Esc", "close")];
    }
    if app.phase == Phase::Configure
        && app
            .selector
            .as_ref()
            .is_some_and(|selector| selector.search_query().is_some())
    {
        return vec![
            HelpEntry::new("search", "Filter", "type", "filter").priority(1),
            HelpEntry::new("clear", "Filter", "Esc", "clear filter").priority(2),
        ];
    }
    let mut entries = match app.phase {
        Phase::Generating => Vec::new(),
        Phase::Scanning => vec![HelpEntry::new("quit", "Global", "q", "quit").priority(1)],
        Phase::Empty => {
            vec![HelpEntry::new("retry", "Action", "Enter", "retry").priority(1)]
        }
        Phase::Configure => vec![
            HelpEntry::new("review", "Action", "Enter", "review").priority(1),
            HelpEntry::new("toggle", "Selection", "Space", "toggle").priority(2),
            HelpEntry::new("move", "Navigation", "↑↓/jk", "move").priority(3),
            HelpEntry::new("filter", "Selection", "/", "filter").priority(4),
            HelpEntry::new("details", "Selection", "i", "details").priority(5),
        ],
        Phase::Review => vec![
            HelpEntry::new("confirm", "Action", "Enter", "confirm").priority(1),
            HelpEntry::new("back", "Navigation", "Esc", "back").priority(2),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(3),
        ],
        Phase::Complete => vec![
            HelpEntry::new("done", "Action", "Enter", "done").priority(1),
            HelpEntry::new("rescan", "Action", "r", "rescan").priority(2),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(3),
        ],
        Phase::CheckDrift => vec![
            HelpEntry::new("done", "Action", "Enter", "exit").priority(1),
            HelpEntry::new("back", "Navigation", "Esc", "back").priority(2),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(3),
        ],
        Phase::Error(FailedOperation::Scan) => vec![
            HelpEntry::new("retry", "Action", "Enter", "retry scan").priority(1),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(2),
        ],
        Phase::Error(FailedOperation::Review) => vec![
            HelpEntry::new("retry", "Action", "Enter", "retry review").priority(1),
            HelpEntry::new("back", "Navigation", "Esc", "back").priority(2),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(3),
        ],
        Phase::Error(FailedOperation::Generate) => vec![
            HelpEntry::new("retry", "Action", "Enter", "retry").priority(1),
            HelpEntry::new("back", "Navigation", "Esc", "back").priority(2),
            HelpEntry::new("scroll", "Navigation", "↑↓/jk", "scroll").priority(3),
        ],
    };
    if app.phase != Phase::Generating {
        entries.push(HelpEntry::new("help", "Global", "?", "help").priority(8));
        if app.phase != Phase::Scanning {
            entries.push(HelpEntry::new("quit", "Global", "q", "quit").priority(9));
        }
    }
    if app.overlay == Some(Overlay::Help) {
        entries.retain(|entry| entry.chord != "Esc");
        entries.push(HelpEntry::new("close", "Overlay", "Esc", "close").priority(0));
    }
    entries
}

fn render_too_small(frame: &mut Frame<'_>, app: &App, system: &DesignSystem) {
    let area = frame.area();
    let state = if app.phase == Phase::Generating {
        Line::from(Span::styled(
            "Generation running · input locked",
            system.style(Role::Warning),
        ))
    } else {
        Line::from("q quit")
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(
                    "{} Terminal too small",
                    system.glyphs.resolve(Glyph::Warning).text
                ),
                system.style(Role::Warning).add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "Need {}×{} · current {}×{}",
                super::MIN_WIDTH,
                super::MIN_HEIGHT,
                area.width,
                area.height
            )),
            state,
        ])
        .alignment(ratatui::layout::Alignment::Center),
        area,
    );
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{centered_rect, render, wrapped_height};
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use ratatui::{backend::TestBackend, Terminal};

    fn app() -> super::super::App {
        let (_sender, receiver) = std::sync::mpsc::channel();
        super::super::App::new(
            crate::Cli {
                target: ".".to_owned(),
                default_branch: None,
                output: None,
                runners: crate::RunnerMode::Github,
                dry_run: false,
                check: false,
                force: false,
                plain: false,
            },
            receiver,
        )
    }

    fn configured_app() -> super::super::App {
        let mut app = app();
        let unit = crate::Unit {
            id: "workspace-with-a-long-name".to_owned(),
            label: "Workspace with a long Unicode label λ".to_owned(),
            kind: crate::UnitKind::Rust,
            root: "crates/workspace-with-a-long-name".to_owned(),
            watch: Vec::new(),
            pr_commands: (0..20)
                .map(|index| format!("cargo test --package example-{index}"))
                .collect(),
            full_commands: vec!["cargo test --workspace --all-targets".to_owned()],
            depends_on: Vec::new(),
            cache: None,
            tool_version: Some("1.97.1".to_owned()),
        };
        let config = crate::ProjectConfig {
            repository: "example/project".to_owned(),
            profile: crate::RepositoryProfile::Generic,
            analysis: crate::AnalysisSummary {
                method: "static metadata".to_owned(),
                detected: vec!["Rust".to_owned()],
                limitations: vec!["Release remains disabled until reviewed".to_owned()],
            },
            verified: true,
            workflow_files: vec!["ci-pr.yml".to_owned()],
            notes: Vec::new(),
            default_branch: "main".to_owned(),
            runners: crate::RunnerMode::Github,
            github_runner: "ubuntu-24.04".to_owned(),
            velnor_labels: Vec::new(),
            release_enabled: false,
            release_reason: "not configured".to_owned(),
            release: None,
            units: vec![unit],
        };
        let id = "workspace-with-a-long-name".to_owned();
        let mut selector = termrock::widgets::ListState::new(Some(id.clone()));
        selector.enable_multi_select();
        if let Some(selection) = selector.selection_mut() {
            selection.select_all(&[id]);
        }
        app.config = Some(config);
        app.selector = Some(selector);
        app.output_root = Some(PathBuf::from("/tmp/generated-output"));
        app.files = Some(BTreeMap::from([(
            PathBuf::from(".github/workflows/ci-pr.yml"),
            "name: CI".to_owned(),
        )]));
        app.phase = super::super::Phase::Configure;
        app
    }

    fn render_text(app: &mut super::super::App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => match error {},
        };
        let system = super::super::design_system();
        let draw = terminal.draw(|frame| render(frame, app, &system));
        assert!(draw.is_ok(), "frame should render");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    }

    #[test]
    fn centered_rect_stays_inside_frame() {
        let frame = Rect::new(0, 0, 80, 24);
        let rect = centered_rect(frame, 76, 14);
        assert!(frame.contains((rect.x, rect.y).into()));
        assert!(frame.contains((rect.right() - 1, rect.bottom() - 1).into()));
    }

    #[test]
    fn scanning_frame_is_calm_and_honest() {
        let text = render_text(&mut app(), 80, 24);
        assert!(text.contains("GitHub Actions"));
        assert!(text.contains("Scanning project"));
        assert!(text.contains("No project commands are run"));
        assert!(!text.contains("[waiting]"));
        assert!(!text.contains("TR/phosphor"));
        assert!(text.contains("q quit"));
    }

    #[test]
    fn too_small_screen_names_exact_requirement() {
        let text = render_text(&mut app(), 40, 10);
        assert!(text.contains("Terminal too small"));
        assert!(text.contains("52×16"));
        assert!(text.contains("40×10"));
        assert!(text.contains("q quit"));
    }

    #[test]
    fn too_small_generating_screen_advertises_locked_running_state() {
        let mut app = app();
        app.phase = super::super::Phase::Generating;
        let text = render_text(&mut app, 40, 10);
        assert!(text.contains("Generation running"));
        assert!(text.contains("input locked"));
        assert!(!text.contains("q quit"));

        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.exit);
    }

    #[test]
    fn minimum_supported_terminal_renders_main_screen() {
        let text = render_text(&mut app(), 52, 16);
        assert!(text.contains("Scanning project"));
        assert!(!text.contains("Terminal too small"));
    }

    #[test]
    fn every_workflow_state_has_intentional_copy() {
        let mut app = configured_app();
        assert!(render_text(&mut app, 80, 24).contains("Choose checks"));

        app.phase = super::super::Phase::Review;
        assert!(render_text(&mut app, 80, 24).contains("Ready to generate CI"));

        app.phase = super::super::Phase::Generating;
        assert!(render_text(&mut app, 80, 24).contains("Writing generated files"));

        app.phase = super::super::Phase::Complete;
        app.outcome = Some(crate::WriteOutcome::DryRun(vec![PathBuf::from(
            ".github/workflows/ci-pr.yml",
        )]));
        assert!(render_text(&mut app, 80, 24).contains("Preview complete"));

        app.phase = super::super::Phase::Error(super::super::FailedOperation::Generate);
        app.error = Some("owned file changed".to_owned());
        assert!(render_text(&mut app, 80, 24).contains("Generation failed"));

        app.phase = super::super::Phase::Empty;
        let empty = render_text(&mut app, 80, 24);
        assert!(empty.contains("No supported project found"));
        assert!(empty.contains("No supported manifests or lockfiles found"));
        assert_eq!(empty.matches("retry").count(), 1);
        assert_eq!(empty.matches("q quit").count(), 1);
    }

    #[test]
    fn review_and_check_drift_show_delete_and_ownership_changes() {
        let mut app = configured_app();
        app.plan = Some(crate::GeneratedWritePlan {
            files: vec![
                crate::PlannedFile {
                    path: PathBuf::from(".github/workflows/old.yml"),
                    action: crate::PlannedAction::Delete,
                    preimage: crate::FilePreimage::Missing,
                },
                crate::PlannedFile {
                    path: PathBuf::from(".github/ci/unchanged.toml"),
                    action: crate::PlannedAction::Same,
                    preimage: crate::FilePreimage::Missing,
                },
                crate::PlannedFile {
                    path: PathBuf::from(crate::OWNERSHIP_STATE),
                    action: crate::PlannedAction::Update,
                    preimage: crate::FilePreimage::Missing,
                },
            ],
            changed: Vec::new(),
            stale: vec![PathBuf::from(".github/workflows/old.yml")],
            conflicts: Vec::new(),
            ownership_present: true,
            ownership_needs_refresh: true,
        });
        app.phase = super::super::Phase::Review;
        let review = render_text(&mut app, 80, 24);
        assert!(review.contains("0 create · 1 update · 1 delete"));
        assert!(review.contains("- DELETE .github/workflows/old.yml"));
        assert!(review.contains("~ UPDATE .github/ci/.github-actions-generator-state"));
        assert!(!review.contains("unchanged.toml"));

        app.phase = super::super::Phase::CheckDrift;
        let drift = render_text(&mut app, 80, 24);
        assert!(drift.contains("Generated files differ"));
        assert!(drift.contains("Run without --check"));
        assert!(!drift.contains("Generation failed"));
        assert!(!drift.contains("unchanged.toml"));

        app.phase = super::super::Phase::Complete;
        app.outcome = Some(crate::WriteOutcome::DryRun(Vec::new()));
        let complete = render_text(&mut app, 80, 24);
        assert!(!complete.contains("unchanged.toml"));
    }

    #[test]
    fn review_explains_when_preflight_changed_after_confirmation() {
        let mut app = configured_app();
        app.phase = super::super::Phase::Review;
        app.notice = Some(
            "Files changed since review · inspect the updated plan before confirming".to_owned(),
        );

        let review = render_text(&mut app, 80, 24);
        assert!(review.contains("Files changed since review"));
        assert!(review.contains("updated plan before confirming"));
    }

    #[test]
    fn long_errors_scroll_to_the_complete_recovery_detail() {
        let mut app = configured_app();
        app.phase = super::super::Phase::Error(super::super::FailedOperation::Generate);
        app.error = Some(format!(
            "{} FINAL-ERROR-DETAIL",
            "long failure context ".repeat(40)
        ));
        let first = render_text(&mut app, 52, 16);
        assert!(!first.contains("FINAL-ERROR-DETAIL"));
        assert!(app.scroll.overflows_y());

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let end = render_text(&mut app, 52, 16);
        assert!(end.contains("FINAL-ERROR-DETAIL"));
        assert_eq!(end.matches("Enter retry").count(), 1);
    }

    #[test]
    fn details_overlay_scrolls_long_commands_at_minimum_size() {
        let mut app = configured_app();
        app.overlay = Some(super::super::Overlay::Details);
        let text = render_text(&mut app, 52, 16);
        assert!(text.contains("Check details"));
        assert!(text.contains("↑↓/jk scroll"));
        assert!(app.scroll.overflows_y());
    }

    #[test]
    fn primary_action_and_filter_are_visible_without_stale_text() {
        let mut app = configured_app();
        let configure = render_text(&mut app, 80, 24);
        assert!(configure.contains("Enter review"));

        for code in [
            KeyCode::Char('/'),
            KeyCode::Char('r'),
            KeyCode::Char('u'),
            KeyCode::Char('s'),
            KeyCode::Char('t'),
        ] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let filtered = render_text(&mut app, 80, 24);
        assert!(filtered.contains("Filter /rust"));
        assert!(!filtered.contains("matchescted"));
        assert!(!filtered.contains("No checks match this filter"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.phase, super::super::Phase::Configure);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.phase, super::super::Phase::Review);
    }

    #[test]
    fn wrapped_height_counts_display_rows() {
        let lines = vec![Line::from(
            "a long line whose words need several narrow terminal rows",
        )];
        assert!(wrapped_height(&lines, 12) >= 5);
    }

    #[test]
    fn contextual_help_only_shows_active_actions() {
        let mut app = configured_app();
        app.overlay = Some(super::super::Overlay::Help);
        let configure = render_text(&mut app, 80, 24);
        assert!(configure.contains("close"));
        assert!(!configure.contains("retry scan"));

        app.overlay = None;
        app.phase = super::super::Phase::Error(super::super::FailedOperation::Scan);
        let error = render_text(&mut app, 80, 24);
        assert!(error.contains("retry scan"));
        assert!(!error.contains("toggle"));
    }
}
