use serde::Deserialize;

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
