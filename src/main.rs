use std::{
    collections::{HashMap, HashSet},
    io,
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use graph_rs_sdk::*;
use ratatui::{
    prelude::*,
    widgets::{block::Title, calendar::Monthly, *},
};

use serde::{Deserialize, Serialize};
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
    fn new(input_tasks: Option<Vec<Task>>) -> App {
        let todo_file = get_todo_file();
        let todos = std::fs::read_to_string(todo_file).unwrap();
        let now = chrono::Local::now().date_naive();
        let mut todos: Vec<Task> = todos.split('\n').map(|x| Task::parse(x, now)).collect();
        if let Some(mut extra_tasks) = input_tasks {
            todos.append(&mut extra_tasks);
        }
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
    let todo_file = directories::ProjectDirs::from("", "", "wyrcan").unwrap();
    let todo_file = todo_file.config_dir().join("todo.txt");
    todo_file
}
fn get_config_file() -> std::path::PathBuf {
    let todo_file = directories::ProjectDirs::from("", "", "wyrcan").unwrap();
    let todo_file = todo_file.config_dir().join("wyrcan.json");
    todo_file
}
use oauth2::{basic::BasicClient, reqwest::async_http_client, RefreshToken, TokenResponse};
// Alternatively, this can be `oauth2::curl::http_client` or a custom client.
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    RedirectUrl, Scope, TokenUrl,
};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use url::Url;

#[derive(Debug, Serialize, Deserialize)]
pub struct TodoList {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    displayName: Option<String>,
    // ... Any other fields
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TodoLists {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Vec<TodoList>>,
    // ... Any other fields
}
#[derive(Debug, Serialize, Deserialize)]
pub struct MicrosoftTasks {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Vec<MicrosoftTask>>,
    // ... Any other fields
}
#[derive(Debug, Serialize, Deserialize)]
pub struct MicrosoftTask {
    id: String,
    title: String,
    createdDateTime: String,
    lastModifiedDateTime: String,
    status: String,
    // ... Any other fields
}

impl MicrosoftTask {
    fn get_completed_mark(&self) -> String {
        match self.status.as_ref() {
            "completed" => "x ".to_owned(),
            _ => "".to_owned(),
        }
    }
}

trait GetTodoTxt {
    fn get_todo_txt(&self) -> todo_lib::todotxt::Task;
}

impl GetTodoTxt for MicrosoftTask {
    fn get_todo_txt(&self) -> todo_lib::todotxt::Task {
        Task {
            subject: self.title.clone(),
            ..Default::default()
        }
    }
}

impl GetTodoTxt for Task {
    fn get_todo_txt(&self) -> todo_lib::todotxt::Task {
        self.clone()
    }
}

async fn get_microsoft_tasks() -> eyre::Result<Vec<Task>> {
    let graph_client_id = ClientId::new(String::from("d4b5c04f-2a75-421c-84f0-dd898502c93e"));
    let auth_url =
        AuthUrl::new("https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_string())
            .expect("Invalid authorization endpoint URL");
    let token_url =
        TokenUrl::new("https://login.microsoftonline.com/common/oauth2/v2.0/token".to_string())
            .expect("Invalid token endpoint URL");

    // Set up the config for the Microsoft Graph OAuth2 process.
    let client = BasicClient::new(graph_client_id, None, auth_url, Some(token_url))
        // Microsoft Graph requires client_id and client_secret in URL rather than
        // using Basic authentication.
        .set_auth_type(AuthType::RequestBody)
        // This example will be running its own server at localhost:3003.
        // See below for the server implementation.
        .set_redirect_uri(
            RedirectUrl::new("http://localhost:3003/redirect".to_string())
                .expect("Invalid redirect URL"),
        );
    let refresh_token: Result<String, std::io::Error> = std::fs::read_to_string(get_config_file());
    let refresh_token = match refresh_token {
        Ok(token) => serde_json::from_str(&token).unwrap(),
        Err(_) => get_oauth_key(&client).await,
    };
    let auth_token = client
        .exchange_refresh_token(&refresh_token)
        .request_async(async_http_client)
        .await
        .unwrap();
    let bearer_token = auth_token.access_token().secret();

    let graph_client = Graph::new(bearer_token);
    let resp = graph_client
        .me()
        .todo()
        .lists()
        .list_lists()
        .send()
        .await?
        .json::<TodoLists>()
        .await
        .unwrap();
    let wyrcan_list = resp
        .value
        .as_ref()
        .unwrap()
        .iter()
        .find(|x| x.displayName.as_ref().unwrap().as_str() == "Wyrcan")
        .unwrap();

    let id = wyrcan_list.id.as_ref().unwrap();
    let list: MicrosoftTasks = graph_client
        .me()
        .todo()
        .list(id)
        .tasks()
        .list_tasks()
        .send()
        .await?
        .json()
        .await?;
    Ok(list
        .value
        .unwrap()
        .into_iter()
        .map(|x| {
            Task::parse(
                &format!("{}{} id:{}", x.get_completed_mark(), x.title, x.id),
                chrono::Local::now().date_naive(),
            )
        })
        .collect())
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // let mut client = reqwest::blocking::Client::new();
    // let resp = client
    //     .get("https://graph.microsoft.com/v1.0/me/todo/lists/AQMkADAwATYwMAItODQwOC0zYTdlLTAwAi0wMAoALgAAA6XTWk_Pnx1Es7FQsRaCZL4BAFc_HazdTXFFn0H3zltP3_8ABmsOTmIAAAA=\\/tasks")
    //     .bearer_auth(bearer_token)
    //     .send()
    //     .unwrap()
    //     .text()
    //     .unwrap();

    let tasks = get_microsoft_tasks().await?;

    println!("{tasks:#?}");

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let app = App::new(Some(tasks));
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

async fn get_oauth_key(
    client: &oauth2::Client<
        oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>,
        oauth2::StandardTokenResponse<oauth2::EmptyExtraTokenFields, oauth2::basic::BasicTokenType>,
        oauth2::basic::BasicTokenType,
        oauth2::StandardTokenIntrospectionResponse<
            oauth2::EmptyExtraTokenFields,
            oauth2::basic::BasicTokenType,
        >,
        oauth2::StandardRevocableToken,
        oauth2::StandardErrorResponse<oauth2::RevocationErrorResponseType>,
    >,
) -> RefreshToken {
    // Microsoft Graph supports Proof Key for Code Exchange (PKCE - https://oauth.net/2/pkce/).
    // Create a PKCE code verifier and SHA-256 encode it as a code challenge.
    let (pkce_code_challenge, pkce_code_verifier) = PkceCodeChallenge::new_random_sha256();

    // Generate the authorization URL to which we'll redirect the user.
    let (authorize_url, csrf_state) = client
        .authorize_url(CsrfToken::new_random)
        // This example requests read access to OneDrive.
        .add_scope(Scope::new(
            "https://graph.microsoft.com/Files.Read".to_string(),
        ))
        .add_scope(Scope::new("offline_access".to_string()))
        .set_pkce_challenge(pkce_code_challenge)
        .url();

    println!(
        "Open this URL in your browser:\n{}\n",
        authorize_url.to_string()
    );

    // A very naive implementation of the redirect server.
    let listener = TcpListener::bind("127.0.0.1:3003").unwrap();
    for stream in listener.incoming() {
        if let Ok(mut stream) = stream {
            let code;
            let state;
            {
                let mut reader = BufReader::new(&stream);

                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();

                let redirect_url = request_line.split_whitespace().nth(1).unwrap();
                let url = Url::parse(&("http://localhost".to_string() + redirect_url)).unwrap();

                let code_pair = url
                    .query_pairs()
                    .find(|pair| {
                        let &(ref key, _) = pair;
                        key == "code"
                    })
                    .unwrap();

                let (_, value) = code_pair;
                code = AuthorizationCode::new(value.into_owned());

                let state_pair = url
                    .query_pairs()
                    .find(|pair| {
                        let &(ref key, _) = pair;
                        key == "state"
                    })
                    .unwrap();

                let (_, value) = state_pair;
                state = CsrfToken::new(value.into_owned());
            }

            let message = "Go back to your terminal :)";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                message.len(),
                message
            );
            stream.write_all(response.as_bytes()).unwrap();

            println!("MS Graph returned the following code:\n{}\n", code.secret());
            println!(
                "MS Graph returned the following state:\n{} (expected `{}`)\n",
                state.secret(),
                csrf_state.secret()
            );

            // Exchange the code with a token.
            let token = client
                .exchange_code(code)
                // Send the PKCE code verifier in the token request
                .set_pkce_verifier(pkce_code_verifier)
                .request_async(async_http_client)
                .await;

            println!("MS Graph returned the following token:\n{:?}\n", token);
            let token = token.unwrap();
            println!(
                "Access token {:?}\n refresh token {:?}",
                &token.access_token(),
                token.refresh_token()
            );

            // The server will terminate itself after collecting the first code.
            // let token = client
            //     .exchange_refresh_token(&token.refresh_token().unwrap())
            //     .request(http_client);

            // let token = token.unwrap();
            // println!(
            //     "Access token {:?}\n refresh token {:?}",
            //     &token.access_token(),
            //     token.refresh_token()
            // );
            let serialized_refresh_token =
                serde_json::to_string_pretty(&token.refresh_token()).unwrap();
            std::fs::write(get_config_file(), serialized_refresh_token).unwrap();
            return token.refresh_token().unwrap().clone();
        }
    }
    panic!("oauth didn't work")
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
