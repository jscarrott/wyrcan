use crate::sync::{Remote, Sections};
use anyhow::{anyhow, Context};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub section_id: Option<String>,
    #[serde(default)]
    pub child_order: i64,
}

#[derive(Deserialize)]
struct Page<T> {
    results: Vec<T>,
    next_cursor: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> anyhow::Result<(Vec<T>, Option<String>)> {
    let p: Page<T> = serde_json::from_str(body).context("bad Todoist page")?;
    Ok((p.results, p.next_cursor))
}

#[cfg(test)]
pub fn parse_page(body: &str) -> anyhow::Result<(Vec<Task>, Option<String>)> {
    parse(body)
}

/// Follows next_cursor until null or empty; `fetch` gets the cursor and returns one page.
fn paginate<T>(mut fetch: impl FnMut(Option<&str>) -> anyhow::Result<(Vec<T>, Option<String>)>) -> anyhow::Result<Vec<T>> {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let (mut items, next) = fetch(cursor.as_deref())?;
        all.append(&mut items);
        match next {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => return Ok(all),
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
    project_id: String,
}

impl Client {
    pub fn new(token: String) -> Client {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let base = std::env::var("CLAUDE_BOARD_API")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://api.todoist.com/api/v1".into());
        Client { agent, base, token, project_id: String::new() }
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<String> {
        let mut req = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(k, v);
        }
        let mut resp = req.call().with_context(|| format!("GET {path}"))?;
        Ok(resp.body_mut().read_to_string()?)
    }

    fn list<T: serde::de::DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Vec<T>> {
        paginate(|cursor| {
            let mut q = query.to_vec();
            if let Some(c) = cursor {
                q.push(("cursor", c));
            }
            parse(&self.get(path, &q)?)
        })
    }

    fn find_id(&self, path: &str, query: &[(&str, &str)], name: &str) -> anyhow::Result<Option<String>> {
        #[derive(Deserialize)]
        struct Item {
            id: String,
            name: String,
        }
        let items: Vec<Item> = self.list(path, query)?;
        Ok(items.into_iter().find(|i| i.name == name).map(|i| i.id))
    }

    fn resolve(&self, project: &str) -> anyhow::Result<(String, Sections)> {
        let hint = "create the project in Todoist";
        let pid = self
            .find_id("/projects", &[], project)?
            .ok_or_else(|| anyhow!("Todoist project {project:?} not found; {hint}"))?;
        let sec = |n: &str| {
            self.find_id("/sections", &[("project_id", &pid)], n)?
                .ok_or_else(|| anyhow!("section {n:?} not found in {project:?}; {hint}"))
        };
        let (up_next, in_progress, needs_review) = (sec("Up next")?, sec("In progress")?, sec("Needs review")?);
        // The board's own section: made on demand.
        let parked = match self.find_id("/sections", &[("project_id", &pid)], "Parked")? {
            Some(id) => id,
            None => self.create("/sections", serde_json::json!({ "name": "Parked", "project_id": pid }))?,
        };
        Ok((pid, Sections { up_next, in_progress, needs_review, parked }))
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        self.agent
            .post(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
    }

    /// POSTs JSON and returns the new object's id.
    fn create(&self, path: &str, body: serde_json::Value) -> anyhow::Result<String> {
        let mut resp = self.post(path).send_json(body).with_context(|| format!("POST {path}"))?;
        #[derive(Deserialize)]
        struct Created {
            id: String,
        }
        Ok(resp.body_mut().read_json::<Created>()?.id)
    }
}

/// Resolves the project and its sections (creating "Parked" if missing).
pub fn connect(cfg: &crate::config::Config) -> anyhow::Result<(Client, Sections)> {
    let mut c = Client::new(cfg.token()?);
    let (pid, sections) = c.resolve(&cfg.project)?;
    c.project_id = pid;
    Ok((c, sections))
}

impl Remote for Client {
    fn tasks(&self) -> anyhow::Result<Vec<Task>> {
        self.list("/tasks", &[("project_id", &self.project_id)])
    }

    fn create_task(&self, title: &str, description: &str, labels: &[String], section_id: Option<&str>) -> anyhow::Result<String> {
        let mut body = serde_json::json!({ "content": title, "description": description, "labels": labels, "project_id": self.project_id });
        if let Some(s) = section_id {
            body["section_id"] = s.into();
        }
        self.create("/tasks", body)
    }

    fn move_task(&self, id: &str, section_id: &str) -> anyhow::Result<()> {
        self.post(&format!("/tasks/{id}/move"))
            .send_json(serde_json::json!({ "section_id": section_id }))
            .context("moving task")?;
        Ok(())
    }

    fn complete(&self, id: &str) -> anyhow::Result<()> {
        self.post(&format!("/tasks/{id}/close")).send_empty().context("closing task")?;
        Ok(())
    }

    fn add_comment(&self, id: &str, text: &str) -> anyhow::Result<()> {
        self.create("/comments", serde_json::json!({ "task_id": id, "content": text }))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAGE: &str = include_str!("../tests/fixtures/tasks_page.json");

    #[test]
    fn parse_page_reads_cursor() {
        let (tasks, cur) = parse_page(PAGE).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "6fX1");
        assert_eq!(tasks[0].section_id.as_deref(), Some("S1"));
        assert_eq!(cur.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_page_null_cursor() {
        let (tasks, cur) = parse_page(r#"{"results":[],"next_cursor":null}"#).unwrap();
        assert!(tasks.is_empty());
        assert_eq!(cur, None);
    }

    #[test]
    fn parse_page_ignores_unknown_fields() {
        let (tasks, _) = parse_page(PAGE).unwrap();
        assert_eq!(tasks[1].section_id, None);
        assert_eq!(tasks[1].child_order, 2);
    }

    #[test]
    fn paginate_follows_cursors() {
        let pages = [r#"{"results":[{"id":"a","content":"x","section_id":null}],"next_cursor":"c1"}"#,
                     r#"{"results":[{"id":"b","content":"y","section_id":null}],"next_cursor":null}"#];
        let mut seen = vec![];
        let all: Vec<Task> = paginate(|c| {
            seen.push(c.map(String::from));
            parse(pages[seen.len() - 1])
        })
        .unwrap();
        assert_eq!(all.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(seen, [None, Some("c1".to_string())]);
    }

    #[test]
    fn paginate_stops_on_empty_cursor() {
        let mut calls = 0;
        let all: Vec<Task> = paginate(|_| {
            calls += 1;
            assert!(calls < 3, "looped on empty cursor");
            parse(r#"{"results":[{"id":"a","content":"x","section_id":null}],"next_cursor":""}"#)
        })
        .unwrap();
        assert_eq!((all.len(), calls), (1, 1));
    }

    #[test]
    #[ignore] // talks to the real account; run only with the user present
    fn live_connect_finds_board() {
        let (c, ids) = connect(&crate::config::Config::from_env()).unwrap();
        assert!(!ids.up_next.is_empty() && !ids.parked.is_empty());
        c.tasks().unwrap();
    }
}
