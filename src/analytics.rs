//! Read-only App Analytics usage/crash report tools. Reports are never
//! requested here: only an existing Apple analytics report request is read.
//! Instances are processed newest `processingDate` first and the latest
//! instance replaces the whole event-date partition (no cross-instance merge,
//! per Apple's "Data Completeness and Corrections"). Anything unreadable is
//! reported as `unavailable`, never as zero or partial totals.
mod download;
mod tsv;

use crate::{api_error::ApiError, api_tools::resource_segment, client::ApiClient};
use anyhow::{Result, bail};
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use url::Url;

const MAX_PAGES: usize = 10;
const MAX_REQUESTS: usize = 200;
const MAX_REPORTS: usize = 200;
const MAX_INSTANCES: usize = 2000;
const MAX_SEGMENTS_PER_INSTANCE: usize = 200;
const MAX_SEGMENTS: usize = 400;
const MAX_COMPRESSED: u64 = 32 * 1024 * 1024;
const MAX_EXPANDED: u64 = 64 * 1024 * 1024;
const MAX_PARSED_ROWS: usize = 25_000;
const MAX_RETURNED_ROWS: usize = 2_000;
const WORK_TIMEOUT: Duration = Duration::from_secs(120);

struct Kind {
    tool: &'static str,
    /// Accepted report names, most preferred first. Detailed variants are
    /// deliberately absent.
    names: &'static [&'static str],
    metric: &'static str,
    out: &'static str,
}
const USAGE: Kind = Kind {
    tool: "get_app_usage_report",
    names: &["App Sessions Standard", "App Sessions"],
    metric: "Sessions",
    out: "sessions",
};
const CRASH: Kind = Kind {
    tool: "get_app_crash_report",
    names: &["App Crashes"],
    metric: "Crashes",
    out: "crashes",
};

/// Why a report could not be produced. `Api` is propagated as a typed MCP error.
enum Stop {
    Pending(String),
    Unavailable(String),
    NoData(String),
    Denied(ApiError),
    Api(anyhow::Error),
}
impl From<anyhow::Error> for Stop {
    fn from(e: anyhow::Error) -> Self {
        match e.chain().find_map(|c| c.downcast_ref::<ApiError>()) {
            Some(api) if api.status == 403 => Stop::Denied(api.clone()),
            _ => Stop::Api(e),
        }
    }
}
fn unavailable(msg: &str) -> Stop {
    Stop::Unavailable(msg.into())
}

struct Args {
    app_id: String,
    report_date: Option<NaiveDate>,
    granularity: &'static str,
}
fn parse_args(args: &Value) -> Result<Args> {
    let Some(object) = args.as_object() else {
        bail!("arguments must be an object")
    };
    if let Some(key) = object
        .keys()
        .find(|k| !matches!(k.as_str(), "app_id" | "report_date" | "granularity"))
    {
        bail!(
            "unknown argument: {}",
            key.chars().take(40).collect::<String>()
        )
    }
    let app_id = match object.get("app_id") {
        Some(Value::String(s))
            if !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') =>
        {
            s.clone()
        }
        _ => bail!("app_id is required and must be a plain identifier string"),
    };
    let granularity = match object.get("granularity") {
        None => "DAILY",
        Some(Value::String(s)) => match s.as_str() {
            "DAILY" => "DAILY",
            "WEEKLY" => "WEEKLY",
            "MONTHLY" => "MONTHLY",
            _ => bail!("granularity must be DAILY, WEEKLY or MONTHLY"),
        },
        _ => bail!("granularity must be DAILY, WEEKLY or MONTHLY"),
    };
    let report_date = match object.get("report_date") {
        None => None,
        Some(Value::String(s)) => {
            let date = strict_date(s)
                .ok_or_else(|| anyhow::anyhow!("report_date must be a valid YYYY-MM-DD date"))?;
            if date > Utc::now().date_naive() {
                bail!("report_date must not be in the future")
            }
            if granularity == "WEEKLY" && date.weekday() != Weekday::Mon {
                bail!("report_date must be a Monday for WEEKLY reports")
            }
            if granularity == "MONTHLY" && date.day() != 1 {
                bail!("report_date must be the first day of the month for MONTHLY reports")
            }
            Some(date)
        }
        Some(_) => bail!("report_date must be a YYYY-MM-DD string"),
    };
    Ok(Args {
        app_id,
        report_date,
        granularity,
    })
}
/// `YYYY-MM-DD` only: no short forms, signs, whitespace or time parts.
fn strict_date(s: &str) -> Option<NaiveDate> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}
fn count(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
fn attr<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.pointer("/attributes")?.get(key)?.as_str()
}
fn id(item: &Value) -> Result<&str, Stop> {
    match item.get("id").and_then(Value::as_str) {
        Some(s)
            if !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')) =>
        {
            Ok(s)
        }
        _ => Err(unavailable("Apple returned a resource without a usable id")),
    }
}

pub async fn handle(client: &ApiClient, name: &str, args: &Value) -> Result<Option<Value>> {
    let kind = match name {
        "get_app_usage_report" => &USAGE,
        "get_app_crash_report" => &CRASH,
        _ => return Ok(None),
    };
    let args = parse_args(args)?;
    let done = match tokio::time::timeout(WORK_TIMEOUT, run(client, kind, &args)).await {
        Ok(result) => result,
        Err(_) => Err(unavailable("the 120 second work time limit was exceeded")),
    };
    let mut out = json!({
        "status": "available",
        "tool": kind.tool,
        "app_id": args.app_id,
        "granularity": args.granularity,
        "rows": [],
        (format!("{}_total", kind.out)): null,
        "unique_devices_total": null,
        "limits": {"pages_per_collection":MAX_PAGES,"instances":MAX_INSTANCES,
            "compressed_bytes":MAX_COMPRESSED,"expanded_bytes":MAX_EXPANDED,
            "parsed_rows":MAX_PARSED_ROWS,"returned_rows":MAX_RETURNED_ROWS,
            "columns":tsv::MAX_COLUMNS,"cell_bytes":tsv::MAX_CELL_BYTES,"work_seconds":120},
    });
    match done {
        Ok(report) => {
            for (k, v) in report.as_object().into_iter().flatten() {
                out[k] = v.clone()
            }
        }
        Err(Stop::Api(e)) => return Err(e),
        Err(Stop::Pending(action)) => {
            out["status"] = json!("pending");
            out["action"] = json!(action);
        }
        Err(Stop::Unavailable(reason)) => {
            out["status"] = json!("unavailable");
            out["reason"] = json!(reason);
        }
        Err(Stop::NoData(message)) => {
            out["status"] = json!("no_data");
            out["message"] = json!(message);
        }
        Err(Stop::Denied(e)) => {
            out["status"] = json!("permission_denied");
            out["message"] = json!(
                "Apple denied access to analytics reports; the API key's role cannot read them."
            );
            out["error"] = e.to_json();
        }
    }
    Ok(Some(out))
}

const CREATE_HINT: &str = "No usable analytics report data exists yet. This tool never creates report requests; create an ONGOING analytics report request for the app (App Store Connect API POST /v1/analyticsReportRequests) and retry after Apple generates the first reports, usually within a day or two.";

struct Budget {
    segments: usize,
    compressed: u64,
    expanded: u64,
    rows: usize,
}
struct Instance {
    id: String,
    processing: NaiveDate,
}
struct Row {
    dimensions: Map<String, Value>,
    metric: u64,
    devices: u64,
    duration: Option<u64>,
}
struct Partition {
    date: NaiveDate,
    ambiguous: bool,
    checksum_verified: bool,
    processing: NaiveDate,
    instance: String,
    segments: Vec<String>,
    rows: Vec<Row>,
}

/// GET a collection and follow `links.next` within page/item/cycle limits.
/// Next links must stay on the same path (the client enforces the origin).
async fn collect(
    client: &ApiClient,
    path: &str,
    query: Vec<(String, String)>,
    max_items: usize,
) -> Result<Vec<Value>, Stop> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut response = client.request("GET", path, query, None).await?;
    for page_index in 0..MAX_PAGES {
        let Some(page) = response.get("data").and_then(Value::as_array) else {
            return Err(unavailable("Apple returned a malformed collection"));
        };
        if page.len() > max_items.saturating_sub(items.len()) {
            return Err(unavailable("the collection exceeds the item limit"));
        }
        items.extend(page.iter().cloned());
        let Some(next) = response.pointer("/links/next").and_then(Value::as_str) else {
            return Ok(items);
        };
        if page_index + 1 == MAX_PAGES {
            return Err(unavailable("the collection exceeds the page limit"));
        }
        let valid = Url::parse(next)
            .is_ok_and(|u| u.path() == path && u.fragment().is_none() && u.username().is_empty());
        if !valid || !seen.insert(next.to_owned()) {
            return Err(unavailable(
                "Apple returned an invalid or repeating pagination link",
            ));
        }
        response = client.request("GET", next, Vec::new(), None).await?;
    }
    Err(unavailable("the collection exceeds the page limit"))
}
fn q(key: &str, value: impl Into<String>) -> (String, String) {
    (key.into(), value.into())
}

async fn run(client: &ApiClient, kind: &Kind, args: &Args) -> Result<Value, Stop> {
    let app = resource_segment(&args.app_id)?;
    let requests = collect(
        client,
        &format!("/v1/apps/{app}/analyticsReportRequests"),
        vec![q("limit", "200")],
        MAX_REQUESTS,
    )
    .await?;
    let access = |r: &Value| attr(r, "accessType").unwrap_or("").to_owned();
    let stopped = |r: &Value| r.pointer("/attributes/stoppedDueToInactivity") == Some(&json!(true));
    let ongoing: Vec<_> = requests.iter().filter(|r| access(r) == "ONGOING").collect();
    let active: Vec<_> = ongoing.iter().copied().filter(|r| !stopped(r)).collect();
    let snapshots: Vec<_> = requests
        .iter()
        .filter(|r| access(r) == "ONE_TIME_SNAPSHOT" && !stopped(r))
        .collect();
    let request = match (active.as_slice(), ongoing.is_empty(), snapshots.as_slice()) {
        ([one], _, _) => *one,
        ([], _, [one]) => *one,
        ([], true, []) => return Err(Stop::Pending(CREATE_HINT.into())),
        ([], false, []) => {
            return Err(Stop::Pending(format!(
                "The ONGOING analytics report request was stopped due to inactivity. {CREATE_HINT}"
            )));
        }
        _ => {
            return Err(unavailable(
                "several analytics report requests match; the request to read is ambiguous",
            ));
        }
    };
    let request_id = id(request)?.to_owned();

    let reports = collect(
        client,
        &format!(
            "/v1/analyticsReportRequests/{}/reports",
            resource_segment(&request_id)?
        ),
        vec![q("filter[category]", "APP_USAGE"), q("limit", "200")],
        MAX_REPORTS,
    )
    .await?;
    let mut chosen = None;
    for name in kind.names {
        let found: Vec<_> = reports
            .iter()
            .filter(|r| attr(r, "name") == Some(*name) && attr(r, "category") == Some("APP_USAGE"))
            .collect();
        match found.as_slice() {
            [] => continue,
            [one] => chosen = Some((*name, *one)),
            _ => return Err(unavailable("several reports share the requested name")),
        }
        break;
    }
    let Some((report_name, report)) = chosen else {
        return Err(Stop::Pending(format!(
            "Apple has not generated the {} report for this request yet. {CREATE_HINT}",
            kind.names[0]
        )));
    };
    let report_id = id(report)?.to_owned();

    let raw = collect(
        client,
        &format!(
            "/v1/analyticsReports/{}/instances",
            resource_segment(&report_id)?
        ),
        vec![
            q("filter[granularity]", args.granularity),
            q("limit", "200"),
        ],
        MAX_INSTANCES,
    )
    .await?;
    let mut instances = Vec::new();
    for item in raw
        .iter()
        .filter(|i| attr(i, "granularity") == Some(args.granularity))
    {
        let processing = attr(item, "processingDate")
            .and_then(|s| strict_date(s.get(..10).unwrap_or("")))
            .ok_or_else(|| unavailable("an instance has no valid processingDate"))?;
        instances.push(Instance {
            id: id(item)?.to_owned(),
            processing,
        });
    }
    if instances.is_empty() {
        return Err(Stop::Pending(format!(
            "No {} instances exist yet for this report. Apple publishes instances after the report request is processed.",
            args.granularity
        )));
    }
    // The API has no sort parameter for instances: newest first, locally.
    instances.sort_by(|a, b| {
        b.processing
            .cmp(&a.processing)
            .then_with(|| b.id.cmp(&a.id))
    });

    let requested = args.report_date;
    if requested.is_some_and(|d| d > instances[0].processing) {
        return Err(Stop::NoData(format!(
            "No {} data exists for event date {}: it is later than every instance processingDate.",
            args.granularity,
            requested.map(|d| d.to_string()).unwrap_or_default()
        )));
    }
    let http = download::client().map_err(unavailable)?;
    let mut budget = Budget {
        segments: 0,
        compressed: 0,
        expanded: 0,
        rows: 0,
    };
    // Only the one partition that can still win is retained. An instance
    // processed on day P cannot hold event dates after P, which bounds the search.
    let mut best: Option<Partition> = None;
    for instance in &instances {
        let stop = match (requested, &best) {
            (Some(d), b) => {
                instance.processing < d
                    || b.as_ref()
                        .is_some_and(|b| instance.processing < b.processing)
            }
            (None, Some(b)) => {
                instance.processing < b.date
                    || (instance.processing == b.date && instance.processing < b.processing)
            }
            (None, None) => false,
        };
        if stop {
            break;
        }
        let (mut by_date, segments, verified) =
            load_instance(client, &http, kind, args, instance, &mut budget).await?;
        if by_date
            .keys()
            .next_back()
            .is_some_and(|d| *d > instance.processing)
        {
            return Err(unavailable(
                "a row has an event Date after its instance processingDate",
            ));
        }
        // Pick this instance's single candidate before comparing, so overlaps
        // on older dates never matter.
        let picked = match requested {
            Some(d) => by_date.remove(&d).map(|rows| (d, rows)),
            None => by_date.into_iter().next_back(),
        };
        let Some((date, rows)) = picked else { continue };
        match &mut best {
            Some(b) if date == b.date && instance.processing == b.processing => {
                if requested.is_some() {
                    return Err(unavailable(
                        "two instances with the same processingDate both contain the event date",
                    ));
                }
                b.ambiguous = true;
            }
            Some(b) if date <= b.date => {}
            slot => {
                *slot = Some(Partition {
                    date,
                    ambiguous: false,
                    checksum_verified: verified,
                    processing: instance.processing,
                    instance: instance.id.clone(),
                    segments,
                    rows,
                });
            }
        }
    }
    let Some(part) = best else {
        return Err(match requested {
            Some(d) => Stop::NoData(format!(
                "No {} data exists for event date {d}; it may be outside Apple's history or not generated yet.",
                args.granularity
            )),
            None => Stop::NoData("The report instances contain no rows.".into()),
        });
    };
    if part.ambiguous {
        return Err(unavailable(
            "two instances with the same processingDate both contain the event date",
        ));
    }
    let wanted = part.date;

    let mut total = 0u64;
    for row in &part.rows {
        total = total
            .checked_add(row.metric)
            .ok_or_else(|| unavailable("the report total overflows"))?;
    }
    let shown = part.rows.len().min(MAX_RETURNED_ROWS);
    let rows: Vec<_> = part.rows[..shown]
        .iter()
        .map(|r| {
            let mut row = json!({
                "dimensions": r.dimensions,
                (kind.out): r.metric,
                "unique_devices": r.devices,
            });
            if let Some(d) = r.duration {
                row["total_session_duration"] = json!(d)
            }
            row
        })
        .collect();
    let age = (Utc::now().date_naive() - wanted).num_days();
    let mut warnings = vec![
        "Opt-in: only users who agreed to share data with Apple and developers are included.",
        "Privacy: Apple provides data only when events exist from at least five users, so small apps may see gaps.",
        "unique_devices is not additive across rows, so unique_devices_total is null.",
    ];
    if args.granularity == "DAILY" && age < 5 {
        warnings.push("Completeness: daily data is complete within five days; later corrections can replace this date.");
    }
    if !part.checksum_verified {
        warnings.push("Checksum: a selected segment had no checksum, so its download integrity was not verified (gzip CRC and size were still checked).");
    }
    if shown < part.rows.len() {
        warnings.push("rows are truncated; totals cover every row of the event date.");
    }
    Ok(json!({
        "status": "available",
        "report_name": report_name,
        "report_date": wanted.to_string(),
        "processing_date": part.processing.to_string(),
        "rows": rows,
        "row_count": part.rows.len(),
        "rows_truncated": shown < part.rows.len(),
        (format!("{}_total", kind.out)): total,
        "unique_devices_total": null,
        "provenance": {
            "request_id": request_id,
            "access_type": access(request),
            "report_id": report_id,
            "instance_id": part.instance,
            "segment_ids": part.segments,
            "checksum_verified": part.checksum_verified,
        },
        "warnings": warnings,
    }))
}

/// Download and parse every segment of one instance. All-or-nothing: any
/// failure makes the whole call unavailable.
async fn load_instance(
    client: &ApiClient,
    http: &reqwest::Client,
    kind: &Kind,
    args: &Args,
    instance: &Instance,
    budget: &mut Budget,
) -> Result<(BTreeMap<NaiveDate, Vec<Row>>, Vec<String>, bool), Stop> {
    let segments = collect(
        client,
        &format!(
            "/v1/analyticsReportInstances/{}/segments",
            resource_segment(&instance.id)?
        ),
        vec![q("limit", "200")],
        MAX_SEGMENTS_PER_INSTANCE,
    )
    .await?;
    if segments.is_empty() {
        return Err(unavailable("an instance has no segments"));
    }
    let mut by_date: BTreeMap<NaiveDate, Vec<Row>> = BTreeMap::new();
    let mut ids = Vec::new();
    let mut verified = true;
    for segment in &segments {
        budget.segments += 1;
        if budget.segments > MAX_SEGMENTS {
            return Err(unavailable("the segment limit was exceeded"));
        }
        ids.push(id(segment)?.to_owned());
        let size = segment
            .pointer("/attributes/sizeInBytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| unavailable("a segment has no valid sizeInBytes"))?;
        budget.compressed = budget.compressed.saturating_add(size);
        if budget.compressed > MAX_COMPRESSED {
            return Err(unavailable("the compressed download limit was exceeded"));
        }
        let url =
            attr(segment, "url").ok_or_else(|| unavailable("a segment has no download URL"))?;
        let url = download::validate_url(url, client.offline_origin()).map_err(unavailable)?;
        let bytes = download::fetch(http, url, size)
            .await
            .map_err(unavailable)?;
        match attr(segment, "checksum").filter(|s| !s.is_empty()) {
            Some(sum) => download::verify_checksum(&bytes, sum).map_err(unavailable)?,
            None => verified = false,
        }
        let plain =
            download::gunzip(&bytes, MAX_EXPANDED - budget.expanded).map_err(unavailable)?;
        budget.expanded += plain.len() as u64;
        let text =
            String::from_utf8(plain).map_err(|_| unavailable("a segment is not valid UTF-8"))?;
        parse_segment(&text, kind, &args.app_id, &mut by_date, &mut budget.rows)
            .map_err(|e| Stop::Unavailable(e.to_string()))?;
    }
    Ok((by_date, ids, verified))
}

fn parse_segment(
    text: &str,
    kind: &Kind,
    app_id: &str,
    out: &mut BTreeMap<NaiveDate, Vec<Row>>,
    rows_seen: &mut usize,
) -> Result<()> {
    let mut reader = tsv::Reader::new(text);
    let Some(header) = reader.next_record()? else {
        bail!("a segment is empty")
    };
    let mut seen = HashSet::new();
    if !header
        .iter()
        .all(|h| !h.is_empty() && h.len() <= 128 && seen.insert(h.as_str()))
    {
        bail!("a segment has an empty or duplicate column header")
    }
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(date_col), Some(app_col), Some(device_col), Some(metric_col), Some(devices_col)) = (
        col("Date"),
        col("App Apple Identifier"),
        col("Device"),
        col(kind.metric),
        col("Unique Devices"),
    ) else {
        bail!("a segment lacks a required column")
    };
    let duration_col = col("Total Session Duration").filter(|_| kind.metric == "Sessions");
    while let Some(record) = reader.next_record()? {
        if record.len() == 1 && record[0].is_empty() {
            continue;
        }
        if record.len() != header.len() {
            bail!("a segment row has the wrong number of columns")
        }
        *rows_seen += 1;
        if *rows_seen > MAX_PARSED_ROWS {
            bail!("the row limit was exceeded")
        }
        let Some(day) = strict_date(&record[date_col]) else {
            bail!("a segment row has an invalid Date")
        };
        if record[app_col] != app_id {
            bail!("a segment row belongs to a different app")
        }
        if record[device_col].is_empty() {
            bail!("a segment row has an empty Device")
        }
        let (Some(metric), Some(devices)) =
            (count(&record[metric_col]), count(&record[devices_col]))
        else {
            bail!("a segment row has an invalid count")
        };
        let duration = match duration_col {
            Some(i) if !record[i].is_empty() => Some(
                count(&record[i])
                    .ok_or_else(|| anyhow::anyhow!("a segment row has an invalid duration"))?,
            ),
            _ => None,
        };
        let dimensions = header
            .iter()
            .enumerate()
            .filter(|(i, _)| ![metric_col, devices_col].contains(i) && Some(*i) != duration_col)
            .map(|(i, h)| {
                let v = &record[i];
                (h.clone(), if v.is_empty() { Value::Null } else { json!(v) })
            })
            .collect();
        out.entry(day).or_default().push(Row {
            dimensions,
            metric,
            devices,
            duration,
        });
    }
    Ok(())
}
