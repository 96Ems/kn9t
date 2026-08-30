//! R-SRV-110, R-SRV-120 — cost analytics and budget (DESIGN §7.3, §8.7.3, §18.8).
//!
//! `GET /cost?since=&group_by=` serves analytics over the usage projection
//! (STOR R-STOR-180): totals grouped by model/kind/session, plus the local
//! aggregate. `GET /budget` returns both the locally computed estimate and the
//! provider-reported spend where available; drift between them is NOT warned in v1
//! (SPEC-OPEN §18.8).

use std::sync::Arc;

use crate::http_util::{query_param, JsonResp};
use crate::state::ServerState;

/// `GET /cost?since=<ms>&group_by=<model|kind|session>`.
pub fn query(state: &Arc<ServerState>, query: &str) -> JsonResp {
    let since: i64 = query_param(query, "since")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let group_by = query_param(query, "group_by").unwrap_or_else(|| "model".into());

    let column = match group_by.as_str() {
        "model" => "model",
        "kind" => "kind",
        "session" => "session_id",
        other => {
            return JsonResp::error(
                400,
                "bad_group_by",
                &format!("group_by must be model|kind|session, got '{other}'"),
            )
        }
    };

    // Grouped rollup over the usage projection.
    let sql = format!(
        "SELECT {column} AS grp, \
                COALESCE(SUM(cost_usd),0.0) AS cost, \
                COALESCE(SUM(tokens_in),0) AS tin, \
                COALESCE(SUM(tokens_out),0) AS tout \
         FROM usage WHERE ts >= ?1 GROUP BY {column} ORDER BY cost DESC"
    );

    let rows = match read_groups(state, &sql, since) {
        Ok(r) => r,
        Err(e) => return JsonResp::error(500, "store_error", &e),
    };

    let total: f64 = rows.iter().map(|g| g.cost).sum();
    let groups: Vec<serde_json::Value> = rows
        .iter()
        .map(|g| {
            serde_json::json!({
                "group": g.group,
                "cost_usd": g.cost,
                "tokens_in": g.tokens_in,
                "tokens_out": g.tokens_out,
            })
        })
        .collect();

    JsonResp::ok(serde_json::json!({
        "since": since,
        "group_by": group_by,
        "total_cost_usd": total,
        "groups": groups,
    }))
}

struct Group {
    group: String,
    cost: f64,
    tokens_in: i64,
    tokens_out: i64,
}

fn read_groups(state: &Arc<ServerState>, sql: &str, since: i64) -> Result<Vec<Group>, String> {
    // The store exposes single-row/single-column helpers only; use a raw connection
    // read via a prepared statement over the store's reader path. We reuse
    // `query_strings`-style access by serializing each row to a JSON tuple string.
    // Simpler: run the aggregate with a JSON-object projection so one string per row
    // captures all columns.
    let json_sql = format!(
        "SELECT json_object('g', grp, 'c', cost, 'ti', tin, 'to', tout) FROM ({sql})"
    );
    let strings = state
        .store
        .query_strings(&json_sql, &[&since])
        .map_err(|e| e.0)?;
    let mut out = Vec::with_capacity(strings.len());
    for s in strings {
        let v: serde_json::Value = serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
        out.push(Group {
            group: v.get("g").and_then(|x| x.as_str()).unwrap_or("").to_owned(),
            cost: v.get("c").and_then(|x| x.as_f64()).unwrap_or(0.0),
            tokens_in: v.get("ti").and_then(|x| x.as_i64()).unwrap_or(0),
            tokens_out: v.get("to").and_then(|x| x.as_i64()).unwrap_or(0),
        });
    }
    Ok(out)
}

/// `GET /budget` → `{local_estimate, provider_reported?}` (R-SRV-120). Both figures
/// are simply returned; drift is not warned (SPEC-OPEN §18.8).
pub fn budget(state: &Arc<ServerState>) -> JsonResp {
    let local: f64 = state
        .store
        .query_one(
            "SELECT COALESCE(SUM(cost_usd),0.0) FROM usage",
            &[],
            |r| r.get(0),
        )
        .unwrap_or(0.0);

    let provider_reported = *state.provider_reported_budget.lock().unwrap();

    let mut obj = serde_json::json!({ "local_estimate": local });
    if let Some(p) = provider_reported {
        obj["provider_reported"] = serde_json::json!(p);
    }
    JsonResp::ok(obj)
}
