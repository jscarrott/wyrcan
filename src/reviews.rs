//! Parsers for `gh` output used to import PRs awaiting the user's review. Pure: no process calls.
use crate::model::PrState;

#[derive(Debug, PartialEq)]
pub struct Found {
    pub url: String,
    pub title: String,
    pub author: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, PartialEq)]
pub struct PrReview {
    pub state: PrState,
    pub reviewed_by: Vec<String>,
    pub head: String,
}

/// From `gh search prs --json url,title,author,repository,number`.
pub fn parse_search(json: &str) -> Option<Vec<Found>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut out: Vec<Found> = Vec::new();
    for i in v.as_array()? {
        let Some(url) = i["url"].as_str().filter(|u| u.starts_with("https://")) else { continue };
        if out.iter().any(|f| f.url == url) {
            continue;
        }
        out.push(Found {
            url: url.into(),
            title: i["title"].as_str().unwrap_or_default().chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
            author: i["author"]["login"].as_str().unwrap_or_default().into(),
            repo: i["repository"]["nameWithOwner"].as_str().unwrap_or_default().into(),
            number: i["number"].as_u64().unwrap_or(0),
        });
    }
    Some(out)
}

/// From `gh pr view URL --json state,reviews,headRefName`.
pub fn parse_pr_review(json: &str) -> Option<PrReview> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let state = match v["state"].as_str()? {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        "OPEN" => PrState::Waiting,
        _ => PrState::Unknown,
    };
    let mut reviewed_by: Vec<String> = Vec::new();
    for l in v["reviews"].as_array().into_iter().flatten().filter_map(|r| r["author"]["login"].as_str()) {
        if !reviewed_by.iter().any(|x| x == l) {
            reviewed_by.push(l.into());
        }
    }
    Some(PrReview { state, reviewed_by, head: v["headRefName"].as_str().unwrap_or_default().into() })
}

/// `git@github.com:o/r.git` or `https://github.com/o/r(.git)` -> `o/r`.
pub fn parse_remote(url: &str) -> Option<String> {
    let rest = url.strip_prefix("git@github.com:").or_else(|| url.strip_prefix("https://github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, r) = rest.split_once('/')?;
    (!o.is_empty() && !r.is_empty() && !r.contains('/')).then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(n: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{n}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn search_parses_dedupes_and_sanitises() {
        let f = parse_search(&fixture("search_prs")).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], Found { url: "https://github.com/acme/delta/pull/812".into(), title: "Add pcap filter".into(), author: "alice".into(), repo: "acme/delta".into(), number: 812 });
        assert!(!f[1].title.contains('\n') && !f[1].title.contains('\u{1b}'));
        assert!(parse_search("nope").is_none());
    }

    #[test]
    fn pr_review_reads_state_reviewers_and_head() {
        let r = parse_pr_review(&fixture("pr_review")).unwrap();
        assert_eq!(r, PrReview { state: PrState::Merged, reviewed_by: vec!["john".into()], head: "feat/x".into() });
        assert!(parse_pr_review("nope").is_none());
    }

    #[test]
    fn remote_urls() {
        assert_eq!(parse_remote("git@github.com:acme/delta.git").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("https://github.com/acme/delta").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("https://github.com/acme/delta.git").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("/local/path"), None);
    }
}
