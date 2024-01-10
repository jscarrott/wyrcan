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
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

struct App {
    state: TableState,
    items: Vec<todo_txt::Task>,
    adding_item: bool,
    input: Input,
}

impl App {
    fn new() -> App {
        App {
            state: TableState::default(),
            items: vec![
                todo_txt::Task {
                    status: Status::ToDo,
                    description: String::from("test"),
                    due: None,
                },
                Item {
                    status: Status::ToDo,
                    description: String::from("test2"),
                    due: None,
                },
            ],
            adding_item: false,
            input: Input::default(),
        }
    }
    pub fn next(&mut self) {
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

    pub fn previous(&mut self) {
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

    pub fn get_selected_item(&mut self) -> Option<&mut Item> {
        let index = self.state.selected();
        match index {
            Some(index) => Some(self.items.get_mut(index).unwrap()),
            None => None,
        }
    }
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
                        app.items.push(Item {
                            status: Status::ToDo,
                            description: app.input.to_string(),
                            due: None,
                        });
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
                    KeyCode::Char('a') => app.adding_item = true,
                    KeyCode::Char('c') => {
                        app.get_selected_item().unwrap().status =
                            Status::Done(std::time::SystemTime::now())
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    _ => {}
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
            .constraints([Constraint::Percentage(100)])
            .split(f.size());

        let selected_style = Style::default().add_modifier(Modifier::REVERSED);
        let normal_style = Style::default().bg(Color::Blue);
        let header_cells = ["Status", "Task", "Due"]
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::Red)));
        let header = Row::new(header_cells).style(normal_style).height(1);
        // .bottom_margin(1);
        let rows = app.items.iter().map(|item| {
            let height = item.description.chars().filter(|c| *c == '\n').count() + 1;
            let cells = vec![
                Cell::from(Text::from(item.status.to_string())),
                Cell::from(Text::from(item.description.clone())),
            ];
            Row::new(cells).height(height as u16)
        });
        let t = Table::new(
            rows,
            [
                Constraint::Percentage(50),
                Constraint::Max(30),
                Constraint::Min(10),
            ],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Table"))
        .highlight_style(selected_style)
        .highlight_symbol(">> ");
        f.render_stateful_widget(t, rects[0], &mut app.state);
    }
}
