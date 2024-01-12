use std::fmt::Display;
use std::{error::Error, io};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use std::str::FromStr;
use strum_macros::{Display, EnumString};
use todo_lib::todotxt::Task;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

struct App {
    project_state: StatefulList<String>,
    table_state: TableState,
    items: Vec<Task>,
    adding_item: bool,
    input: Input,
}

struct StatefulList<T> {
    state: ListState,
    items: Vec<T>,
}

impl<T> StatefulList<T> {
    fn with_items(items: Vec<T>) -> StatefulList<T> {
        StatefulList {
            state: ListState::default(),
            items,
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn unselect(&mut self) {
        self.state.select(None);
    }
}

impl App {
    fn new() -> App {
        let todo_file = get_todo_file();
        let todos = std::fs::read_to_string(todo_file).unwrap();
        let now = chrono::Local::now().date_naive();
        let todos: Vec<Task> = todos.split('\n').map(|x| Task::parse(&x, now)).collect();
        App {
            table_state: TableState::default(),
            project_state: StatefulList::with_items(vec![]),
            items: todos,
            adding_item: false,
            input: Input::default(),
        }
    }
    pub fn next(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn get_selected_item(&mut self) -> Option<&mut Task> {
        let index = self.table_state.selected();
        match index {
            Some(index) => Some(self.items.get_mut(index).unwrap()),
            None => None,
        }
    }
}

fn get_todo_file() -> std::path::PathBuf {
    let todo_file = directories::ProjectDirs::from("", "", "todo-list").unwrap();
    let todo_file = todo_file.config_dir().join("todo.txt");
    todo_file
}

struct Item {
    status: Status,
    description: String,
    due: Option<std::time::SystemTime>,
}

impl Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

#[derive(Debug, Display)]
enum Status {
    Done(std::time::SystemTime),
    InProgress,
    ToDo,
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
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if app.adding_item {
                match key.code {
                    KeyCode::Enter => {
                        let now = chrono::Local::now().date_naive();
                        let task = Task::parse(&app.input.to_string(), now);
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
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Char('a') => app.adding_item = true,
                    KeyCode::Char('c') => {
                        app.get_selected_item().unwrap().complete(
                            chrono::Local::now().date_naive(),
                            todo_lib::todotxt::CompletionMode::JustMark,
                        );
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    _ => {}
                }
                let new_todo: Vec<String> = app.items.iter().map(|x| x.to_string()).collect();
                let new_todo = new_todo.join("\n");
                let todo_file = get_todo_file();
                std::fs::write(todo_file, new_todo).unwrap();
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
        f.render_widget(input, chunks[1]);
        // Make the cursor visible and ask tui-rs to put it at the specified coordinates after rendering
        f.set_cursor(
            // Put cursor past the end of the input text
            chunks[1].x + ((app.input.visual_cursor()).max(scroll) - scroll) as u16 + 1,
            // Move one line down, from the border to the input line
            chunks[1].y + 1,
        )
    } else {
        let rects = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(f.size());

        let items: Vec<ListItem> = app
            .items
            .iter()
            .flat_map(|i| {
                i.projects.clone()
                //
            })
            // .cloned()
            .map(|x| ListItem::new(x).style(Style::default()))
            .collect();

        // Create a List from all list items and highlight the currently selected one
        let items = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Projects"))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");

        // We can now render the item list
        f.render_stateful_widget(items, rects[0], &mut app.project_state.state);

        let selected_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let normal_style = Style::default();
        let header_cells = ["Status", "Task", "Due"]
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::LightBlue)));
        let header = Row::new(header_cells)
            .style(normal_style)
            .bottom_margin(1)
            .height(1);
        // .bottom_margin(1);
        let rows = app.items.iter().map(|item| {
            let height = item.subject.chars().filter(|c| *c == '\n').count() + 1;
            let cells = vec![
                Cell::from(Text::from(item.finished.to_string())),
                Cell::from(Text::from(item.subject.to_string())),
            ];
            Row::new(cells).height(height as u16)
        });
        let t = Table::new(
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
        .highlight_symbol(">> ");
        f.render_stateful_widget(t, rects[1], &mut app.table_state);
    }
}
