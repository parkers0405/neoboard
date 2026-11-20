use serde_json::{json, Value};

// The build, not the caller or the working directory, selects the manual.
const PAGES: &[(&str, &str)] = &[
    ("start", include_str!("../../../docs/start.md")),
    ("boards", include_str!("../../../docs/boards.md")),
    ("tasks", include_str!("../../../docs/tasks.md")),
    ("agents", include_str!("../../../docs/agents.md")),
    ("operations", include_str!("../../../docs/operations.md")),
];
const READ_CHARS: usize = 4096;
const SEARCH_HITS: usize = 20;
const SNIPPET_CHARS: usize = 160;

pub(super) fn tools() -> Vec<Value> {
    let ids: Vec<_> = PAGES.iter().map(|&(id, _)| id).collect();
    [
        ("docs_list", "List embedded manual page IDs in reading order.", json!({}), vec![]),
        ("docs_read", "Read Markdown by page ID. Offsets and limits count Unicode characters; resume with next_offset until null.", json!({
            "id":{"type":"string","enum":ids},
            "offset":{"type":"integer","minimum":0,"maximum":i64::MAX},
            "limit":{"type":"integer","minimum":1,"maximum":READ_CHARS}
        }), vec!["id"]),
        ("docs_search", "Find literal text (ASCII case-insensitive) in manual lines. Returns page IDs, 1-based line numbers and snippets starting at the match.", json!({
            "query":{"type":"string","minLength":1,"maxLength":128},
            "limit":{"type":"integer","minimum":1,"maximum":SEARCH_HITS}
        }), vec!["query"]),
    ].into_iter().map(|(name, description, properties, required)| json!({
        "name":name,"description":description,"annotations":{"readOnlyHint":true,"openWorldHint":false},
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect()
}

// Called after dispatch has validated the arguments against the schemas above.
pub(super) fn call(name: &str, args: &Value) -> Result<Value, &'static str> {
    match name {
        "docs_list" => Ok(json!({"pages":PAGES.iter().map(|&(id, _)| id).collect::<Vec<_>>()})),
        "docs_read" => {
            let &(id, body) = PAGES.iter().find(|&&(id, _)| args["id"] == id).ok_or("unknown_page")?;
            let offset = usize::try_from(args["offset"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX);
            let limit = args["limit"].as_u64().unwrap_or(READ_CHARS as u64) as usize;
            let mut chars = body.chars().skip(offset);
            let markdown: String = chars.by_ref().take(limit).collect();
            let next = chars.next().map(|_| offset + limit);
            Ok(json!({"id":id,"markdown":markdown,"next_offset":next}))
        }
        "docs_search" => {
            let query = args["query"].as_str().ok_or("invalid_query")?.trim().to_ascii_lowercase();
            if query.is_empty() { return Err("empty_query"); }
            let limit = args["limit"].as_u64().unwrap_or(SEARCH_HITS as u64) as usize;
            let mut hits = Vec::new();
            for &(id, body) in PAGES {
                for (index, line) in body.lines().enumerate() {
                    if let Some(pos) = line.to_ascii_lowercase().find(&query) {
                        if hits.len() == limit { return Ok(json!({"hits":hits,"has_more":true})); }
                        let snippet: String = line[pos..].chars().take(SNIPPET_CHARS).collect();
                        hits.push(json!({"id":id,"line":index+1,"snippet":snippet}));
                    }
                }
            }
            Ok(json!({"hits":hits,"has_more":false}))
        }
        _ => Err("unknown_tool"),
    }
}