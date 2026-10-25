//! Host blocks -> existing phone turn schema. Stable IDs permit native patches.
//! Tool/approval blocks remain visible notices; this adapter never approves them.
use serde_json::{json, Value};

pub fn turns(snapshot: &Value) -> Result<Vec<Value>, String> {
    let blocks = snapshot.pointer("/session/blocks").and_then(Value::as_array).ok_or("Invalid host transcript")?;
    let mut turns: Vec<Value> = Vec::new();
    for block in blocks {
        if block["internal"] == true || block["draft"] == true { continue; }
        let id = block["id"].as_str().ok_or("Host block missing ID")?;
        let role = block["role"].as_str().unwrap_or("system");
        if role == "user" || turns.is_empty() {
            turns.push(json!({"id":id,"items":[]}));
        }
        let text = block["text"].as_str().unwrap_or("");
        let item = match role {
            "user" => {
                let mut item = json!({"type":"user","id":id,"text":text});
                if let Some(at) = block.get("startedAt") { item["at"] = at.clone(); }
                if let Some(a) = block["attachments"].as_array() { item["attachments"] = json!(a.len()); }
                item
            }
            "assistant" => json!({"type":"text","id":id,"text":text,"streaming":block["streaming"] == true}),
            "approval" => json!({"type":"notice","id":id,"text":format!("Approval requires another host client (unsupported on this host): {text}")}),
            _ => {
                let title = block.pointer("/tool/title").and_then(Value::as_str).unwrap_or(role);
                json!({"type":"notice","id":id,"text":format!("{title}: {text}"),"error":block["notice"] == "error"})
            }
        };
        if let Some(turn) = turns.last_mut() {
            if let Some(items) = turn["items"].as_array_mut() { items.push(item); }
            if role == "user" {
                if let Some(at) = block.get("startedAt") { turn["stampAt"] = at.clone(); }
                if let Some(metrics) = block.get("turnMetrics") { turn["metrics"] = metrics.clone(); }
                if let Some(duration) = block.get("durationMs") { turn["durationMs"] = duration.clone(); }
            }
        }
    }
    Ok(turns)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_turns_keep_streaming_and_unsupported_approval_visible() {
        let value = json!({"session":{"blocks":[
            {"id":"u","role":"user","text":"hello","startedAt":123},
            {"id":"a","role":"assistant","text":"world","streaming":true},
            {"id":"p","role":"approval","text":"run this?"},
            {"id":"hidden","role":"user","text":"internal","internal":true}
        ]}});
        let turns = turns(&value).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["id"], "u");
        assert_eq!(turns[0]["items"][1]["streaming"], true);
        assert!(turns[0]["items"][2]["text"].as_str().unwrap().contains("unsupported"));
        assert_eq!(turns[0]["items"].as_array().unwrap().len(), 3);
    }
}
