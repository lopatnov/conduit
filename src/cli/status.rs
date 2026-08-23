use std::process;

use super::admin_client::http_get;

// ── conduit status --upstream ──────────────────────────────────────────────

/// Fetch upstream health from the Admin API and print a formatted table.
pub fn print_upstream_table(addr: &str) {
    let body = match http_get("upstreams", addr) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error parsing response: {e}");
            process::exit(1);
        }
    };
    let flat = match json.get("upstreams").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => {
            // Fallback: the whole response might be an array.
            match json.as_array() {
                Some(arr) => arr.clone(),
                None => {
                    println!("{body}");
                    return;
                }
            }
        }
    };

    if flat.is_empty() {
        println!("No upstreams registered.");
        return;
    }

    // Compute column widths.
    let url_w = flat
        .iter()
        .filter_map(|e| e.get("url").and_then(|v| v.as_str()))
        .map(|s| s.len())
        .max()
        .unwrap_or(3)
        .max(3);

    println!(
        "{:<url_w$}  {:<7}  {:<10}  {:<7}  5xx",
        "URL", "Healthy", "Latency", "Ejected"
    );
    println!("{}", "─".repeat(url_w + 42)); // println! used to avoid format! overhead

    for entry in &flat {
        let url = entry.get("url").and_then(|v| v.as_str()).unwrap_or("?");
        let healthy = entry
            .get("healthy")
            .and_then(|v| v.as_bool())
            .map(|b| if b { "✓" } else { "✗" })
            .unwrap_or("?");
        let latency = entry
            .get("latency_ms")
            .and_then(|v| v.as_u64())
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "—".to_owned());
        let ejected = entry
            .get("ejected")
            .and_then(|v| v.as_bool())
            .map(|b| if b { "yes" } else { "no" })
            .unwrap_or("—");
        let consec_5xx = entry
            .get("consecutive_5xx")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "0".to_owned());
        println!(
            "{:<url_w$}  {:<7}  {:<10}  {:<7}  {}",
            url, healthy, latency, ejected, consec_5xx
        );
    }
    println!();
    let healthy_count = flat
        .iter()
        .filter(|e| e.get("healthy").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();
    println!("{}/{} upstreams healthy", healthy_count, flat.len());
}
