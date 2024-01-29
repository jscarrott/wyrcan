use std::{
    collections::{HashMap, HashSet},
    io,
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{block::Title, calendar::Monthly, *},
};

use todo_lib::todotxt::Task;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

#[derive(Debug)]
enum FocussedTab {
    Projects,
    TODOs,
    Contexts,
}

impl FocussedTab {}

struct App {
    project_state: StatefulList,
    table_state: TableState,
    context_state: ListState,
    items: Vec<Task>,
    adding_item: bool,
    input: Input,
    focus: FocussedTab,
    sorting: bool,
}

struct StatefulList {
    state: ListState,
}

impl StatefulList {
    fn new() -> StatefulList {
        StatefulList {
            state: ListState::default(),
        }
    }
}

impl App {
    fn new() -> App {
        let todo_file = get_todo_file();
        let todos = std::fs::read_to_string(todo_file).unwrap();
        let now = chrono::Local::now().date_naive();
        let todos: Vec<Task> = todos.split('\n').map(|x| Task::parse(x, now)).collect();
        App {
            table_state: TableState::default(),
            project_state: StatefulList::new(),
            context_state: ListState::default(),
            items: todos,
            adding_item: false,
            input: Input::default(),
            focus: FocussedTab::TODOs,
            sorting: true,
        }
    }
    pub fn next(&mut self) {
        match self.focus {
            FocussedTab::Projects => {
                let i = self.next_project();
                self.project_state.state.select(Some(i));
            }
            FocussedTab::TODOs => {
                let i = self.next_todo();
                self.table_state.select(Some(i));
            }
            FocussedTab::Contexts => {
                let i = self.next_context();
                self.context_state.select(Some(i));
            }
        }
    }
    pub fn previous(&mut self) {
        match self.focus {
            FocussedTab::Projects => {
                self.previous_project();
                // self.project_state.state.select(Some(i));
            }
            FocussedTab::TODOs => {
                self.previous_todo();
                // self.table_state.select(Some(i));
            }
            FocussedTab::Contexts => self.previous_context(),
        }
    }

    fn next_todo(&mut self) -> usize {
        match self.table_state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        }
    }
    fn next_project(&mut self) -> usize {
        match self.project_state.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        }
    }
    fn next_context(&mut self) -> usize {
        match self.context_state.selected() {
            Some(i) => {
                if i >= self.get_contexts().len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        }
    }
    pub fn previous_todo(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.get_projects().len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }
    pub fn previous_project(&mut self) {
        let i = match self.project_state.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.get_projects().len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.project_state.state.select(Some(i));
    }
    pub fn previous_context(&mut self) {
        let i = match self.context_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.get_contexts().len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.context_state.select(Some(i));
    }

    fn next_pane(&mut self) {
        self.focus = match self.focus {
            FocussedTab::Projects => FocussedTab::Contexts,
            FocussedTab::TODOs => FocussedTab::Projects,
            FocussedTab::Contexts => FocussedTab::TODOs,
        }
    }
    fn prev_pane(&mut self) {
        self.focus = match self.focus {
            FocussedTab::Projects => FocussedTab::TODOs,
            FocussedTab::TODOs => FocussedTab::Contexts,
            FocussedTab::Contexts => FocussedTab::Projects,
        }
    }
    pub fn get_selected_item(&mut self) -> Option<&mut Task> {
        let index = self.table_state.selected();
        let mut todos = self.get_todo_list();
        match index {
            Some(index) => {
                let t = todos.remove(index);
                Some(t)
            }
            None => None,
        }
    }
    pub fn remove_selected_item(&mut self) {
        let selected = self.get_selected_item();
        if let Some(item) = selected {
            let selected = item.clone();
            self.items.retain(|x| *x != selected)
        }
    }
    pub fn get_selected_project(&self) -> Option<String> {
        let index = self.project_state.state.selected();
        // println!("{:?}", index);
        match index {
            Some(index) => self.get_projects().get_mut(index).cloned(),
            None => None,
        }
    }
    pub fn get_selected_context(&self) -> Option<String> {
        let index = self.context_state.selected();
        match index {
            Some(index) => self.get_contexts().get_mut(index).cloned(),
            None => None,
        }
    }
    fn get_projects(&self) -> Vec<String> {
        let items: HashSet<String> = self
            .items
            .iter()
            .flat_map(|i| {
                i.projects.clone()
                //
            })
            // .cloned()
            // .map(|x| ListItem::new(x).style(Style::default()))
            .collect();
        let mut items: Vec<String> = items.into_iter().collect();
        items.sort();
        items
    }
    fn get_contexts(&self) -> Vec<String> {
        let items: HashSet<String> = self
            .items
            .iter()
            .flat_map(|i| {
                i.contexts.clone()
                //
            })
            // .cloned()
            // .map(|x| ListItem::new(x).style(Style::default()))
            .collect();
        let mut items: Vec<String> = items.into_iter().collect();
        items.sort();
        items
    }

    pub fn get_todo_list(&mut self) -> Vec<&mut Task> {
        let mut filter_conf = todo_lib::tfilter::Conf::default();
        let project_filter = self.get_selected_project();
        let context_filter = self.get_selected_context();
        let mut todos: Vec<&mut Task>;
        let project_filter = match project_filter {
            Some(project) => vec![project.clone()],
            None => vec![],
        };
        let context_filter = match context_filter {
            Some(context) => vec![context.clone()],
            None => vec![],
        };
        let tag_filter = todo_lib::tfilter::TagFilter {
            projects: project_filter,
            contexts: context_filter,
            tags: vec![],
            hashtags: vec![],
        };
        filter_conf.include = tag_filter;
        filter_conf.all = todo_lib::tfilter::TodoStatus::All;
        // println!("{:?}", project);
        let mut ids = todo_lib::tfilter::filter(&self.items, &filter_conf);
        // println!("{:?}", ids);

        // } else {
        //     todos = self.items.iter_mut().collect();
        // };
        if self.sorting {
            let sort_conf = todo_lib::tsort::Conf {
                fields: Some(String::from("done")),
                rev: false,
            };
            todo_lib::tsort::sort(&mut ids, &self.items, &sort_conf);
        }
        let mut lookup_vec: HashMap<usize, &mut Task> = self.items.iter_mut().enumerate().collect();
        todos = ids.iter().filter_map(|id| lookup_vec.remove(id)).collect();
        // todos = self
        //     .items
        //     .iter_mut()
        //     .enumerate()
        //     .filter_map(|(id, x)| if ids.contains(&id) { Some(x) } else { None })
        //     .collect();

        todos
    }
}

fn get_todo_file() -> std::path::PathBuf {
    let todo_file = directories::ProjectDirs::from("", "", "todo-list").unwrap();
    let todo_file = todo_file.config_dir().join("todo.txt");
    todo_file
}

fn main() -> eyre::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let app = App::new();
    let res = run_app(&mut terminal, app);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    let mut last_list = String::from("");
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if app.adding_item {
                match key.code {
                    KeyCode::Enter => {
                        let now = chrono::Local::now().date_naive();
                        let mut task = Task::parse(&app.input.to_string(), now);
                        if task.create_date.is_none() {
                            task.create_date = Some(now);
                        }
                        app.items.push(task);
                        app.input.reset();
                        app.adding_item = false;
                    }
                    KeyCode::Esc => {
                        app.adding_item = false;
                    }
                    _ => {
                        app.input.handle_event(&Event::Key(key));
                    }
                }
            } else if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Tab => app.next_pane(),
                    KeyCode::BackTab => app.prev_pane(),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Char('D') => app.remove_selected_item(),
                    KeyCode::Char('a') => app.adding_item = true,
                    KeyCode::Char('e') => {
                        app.input = app.get_selected_item().unwrap().to_string().into();
                        app.remove_selected_item();
                        app.adding_item = true
                    }
                    KeyCode::Char('s') => app.sorting = !app.sorting,
                    KeyCode::Char('c') => {
                        let sel_task = app.get_selected_item().unwrap();
                        match sel_task.finished {
                            true => {
                                sel_task.uncomplete(todo_lib::todotxt::CompletionMode::JustMark)
                            }
                            false => sel_task.complete(
                                chrono::Local::now().date_naive(),
                                todo_lib::todotxt::CompletionMode::JustMark,
                            ),
                        };
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    KeyCode::Esc => {
                        app.project_state.state.select(None);
                        app.table_state.select(None);
                        app.context_state.select(None);
                        app.focus = FocussedTab::TODOs;
                    }
                    _ => {}
                }
                let new_todo: Vec<String> = app.items.iter().map(|x| x.to_string()).collect();
                let new_todo = new_todo.join("\n");
                let todo_file = get_todo_file();
                if last_list != new_todo {
                    std::fs::write(todo_file, &new_todo).unwrap();
                    last_list = new_todo;
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    if app.adding_item {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Min(1),
                ]
                .as_ref(),
            )
            .split(f.size());

        // let p = Paragraph::new(app.input.value())
        // .block(Block::default().borders(Borders::ALL).title("Input"));
        // let mut text = Text::from(Line::from(msg));
        // text.patch_style(style);
        // let help_message = Paragraph::new(text);
        // f.render_widget(help_message, chunks[0]);

        let width = chunks[0].width.max(3) - 3; // keep 2 for borders and 1 for cursor

        let scroll = app.input.visual_scroll(width as usize);
        let input = Paragraph::new(app.input.value())
            .scroll((0, scroll as u16))
            .block(Block::default().borders(Borders::ALL).title("Input"));
        // let header_style = Style::default()
        //     .add_modifier(Modifier::BOLD)
        //     .fg(Color::Green);

        // let default_style = Style::default()
        //     .add_modifier(Modifier::BOLD)
        //     .bg(Color::Rgb(50, 50, 50));

        // Monthly::new(
        //     Date::from_calendar_date(2024, time::Month::January, 1).unwrap(),
        //     es,
        // )
        // .show_surrounding(Style::default().add_modifier(Modifier::DIM))
        // .show_weekdays_header(header_style)
        // .default_style(default_style)
        // .show_month_header(Style::default());
        f.render_widget(input, chunks[1]);
        // Make the cursor visible and ask tui-rs to put it at the specified coordinates after rendering
        f.set_cursor(
            // Put cursor past the end of the input text
            chunks[1].x + ((app.input.visual_cursor()).max(scroll) - scroll) as u16 + 1,
            // Move one line down, from the border to the input line
            chunks[1].y + 1,
        )
    } else {
        let top_level = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(f.size());
        let filter_pane = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(top_level[0]);

        let contexts = app.get_contexts();
        let contexts: Vec<ListItem<'_>> =
            contexts.iter().map(|x| ListItem::new(x.clone())).collect();
        let projects = app.get_projects();
        let projects: Vec<ListItem<'_>> =
            projects.iter().map(|x| ListItem::new(x.clone())).collect();

        // Create a List from all list items and highlight the currently selected one
        let mut projects = List::new(projects)
            .block(Block::default().borders(Borders::ALL).title("Projects"))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">>");
        let mut contexts = List::new(contexts)
            .block(Block::default().borders(Borders::ALL).title("Contexts"))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">>");

        // We can now render the item list

        let selected_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let normal_style = Style::default();
        let header_cells = ["Status", "Task", "Due"]
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::LightBlue)));
        let header = Row::new(header_cells)
            .style(normal_style)
            // .bottom_margin(1)
            .height(1);
        // .bottom_margin(1);
        let todos = app.get_todo_list();
        let rows = todos.iter().map(|item| {
            let height = item.subject.chars().filter(|c| *c == '\n').count() + 1;
            let mut cells = vec![
                Cell::from(format_status(item.finished).alignment(Alignment::Center)),
                Cell::from(Text::from(item.subject.to_string())),
            ];
            if let Some(date) = item.due_date {
                cells.push(Cell::from(Text::from(date.to_string())));
            } else {
                cells.push(Cell::from(Text::from("N/A")));
            }
            Row::new(cells).height(height as u16)
        });
        let mut t = Table::new(
            rows,
            [
                Constraint::Max(10),
                Constraint::Max(70),
                Constraint::Min(10),
            ],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("TODOs"))
        .highlight_style(selected_style)
        .highlight_symbol(">>");
        match app.focus {
            FocussedTab::Projects => {
                projects = projects.block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::new().blue())
                        .title(Title::from("Projects".not_dim().white().on_blue())),
                );
            }
            FocussedTab::TODOs => {
                t = t.block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::new().blue())
                        .title("TODOs".not_dim().white().on_blue()),
                );
            }
            FocussedTab::Contexts => {
                contexts = contexts.block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::new().blue())
                        .title(Title::from("Contexts".not_dim().white().on_blue())),
                );
            }
        };
        f.render_stateful_widget(projects, filter_pane[0], &mut app.project_state.state);
        f.render_stateful_widget(contexts, filter_pane[1], &mut app.context_state);
        f.render_stateful_widget(t, top_level[1], &mut app.table_state);
    }
}

fn format_status(input: bool) -> Line<'static> {
    match input {
        true => Line::from("✓".green()),
        false => Line::from("●".yellow()),
    }
}
