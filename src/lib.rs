//! TruyenMoe for Aidoku
//! Public read-only API: https://moe.suicaodex.com  (MoeTruyen v2 REST API)
#![no_std]

use aidoku::{
    alloc::{String, Vec, format, string::ToString},
    imports::net::Request,
    prelude::*,
    Chapter, Listing, ListingProvider, Manga, MangaPageResult, MangaStatus,
    Page, PageContent, Source, Result,
};
use serde_json::Value;

// ─── Constants ───────────────────────────────────────────────────────────────
const API_BASE: &str = "https://moe.suicaodex.com";
const SITE_BASE: &str = "https://truyen.moe";
const PAGE_SIZE: u32 = 24;

// ─── URL encode ──────────────────────────────────────────────────────────────
fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_char(b >> 4));
            out.push(hex_char(b & 0x0f));
        }
    }
    out
}

#[inline]
fn hex_char(n: u8) -> char {
    if n < 10 { (b'0' + n) as char } else { (b'A' + n - 10) as char }
}

// ─── HTTP helpers ─────────────────────────────────────────────────────────────
fn get_json(path: &str) -> Result<Value> {
    let url = format!("{}{}", API_BASE, path);
    let resp = Request::get(&url)?
        .header("Accept", "application/json")
        .header("User-Agent", "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15")
        .header("Origin", SITE_BASE)
        .header("Referer", "https://truyen.moe/")
        .send()?;
    let status = resp.status_code();
    let body = resp.get_string()?;
    if !(200..300).contains(&status) {
        let snippet: String = body.chars().take(200).collect();
        bail!("HTTP {} tại {}: {}", status, url, snippet);
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(v) => Ok(v),
        Err(_) => {
            let snippet: String = body.chars().take(200).collect();
            bail!("Không phải JSON từ {}: {}", url, snippet)
        }
    }
}

// ─── Small value helpers ──────────────────────────────────────────────────────
fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

/// Read a field that could be either a number or a numeric string → String
fn id_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| {
            if let Some(n) = x.as_i64() { return Some(n.to_string()); }
            x.as_str().filter(|s| !s.is_empty()).map(String::from)
        })
        .unwrap_or_default()
}

fn array_names(v: &Value, key: &str) -> Option<Vec<String>> {
    let arr = v.get(key)?.as_array()?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(|g| g.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

// ─── Pagination ───────────────────────────────────────────────────────────────
fn total_pages(v: &Value) -> i64 {
    // Try several shapes the API uses
    if let Some(meta) = v.get("meta").or_else(|| v.get("pagination")) {
        if let Some(n) = meta.get("totalPages").and_then(Value::as_i64) { return n; }
        if let Some(n) = meta.get("lastPage").and_then(Value::as_i64) { return n; }
        if let Some(b) = meta.get("hasNextPage").and_then(Value::as_bool) {
            return if b { i64::MAX } else { 0 };
        }
    }
    1
}

fn has_next(v: &Value, page: i32) -> bool {
    (page as i64) < total_pages(v)
}

// ─── Timestamp ────────────────────────────────────────────────────────────────
fn parse_u32(b: &[u8]) -> Option<i64> {
    let mut n: i64 = 0;
    for &c in b {
        if c < b'0' || c > b'9' { return None; }
        n = n * 10 + (c - b'0') as i64;
    }
    Some(n)
}

fn is_leap(y: i64) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

fn days_before_month(year: i64, month: i64) -> i64 {
    const D: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    D[month as usize] + if month > 2 && is_leap(year) { 1 } else { 0 }
}

/// ISO 8601 "YYYY-MM-DDTHH:MM:SS…" → Unix timestamp (seconds)
fn parse_timestamp(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 { return None; }
    let y  = parse_u32(&b[0..4])?;
    let mo = parse_u32(&b[5..7])?;
    let d  = parse_u32(&b[8..10])?;
    let hh = parse_u32(&b[11..13])?;
    let mm = parse_u32(&b[14..16])?;
    let ss = parse_u32(&b[17..19])?;
    let yy = y - 1;
    let days = yy * 365 + yy / 4 - yy / 100 + yy / 400
        + days_before_month(y, mo) + d - 1
        - 719_162; // offset epoch
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

// ─── Status ───────────────────────────────────────────────────────────────────
fn status_of(s: &str) -> MangaStatus {
    match s {
        "ongoing"   => MangaStatus::Ongoing,
        "completed" => MangaStatus::Completed,
        "hiatus"    => MangaStatus::Hiatus,
        "cancelled" => MangaStatus::Cancelled,
        _           => MangaStatus::Unknown,
    }
}

// ─── Manga mapping ────────────────────────────────────────────────────────────
fn cover_url(v: &Value) -> Option<String> {
    // Try multiple possible field names/shapes
    opt_str(v, "coverUrl")
        .or_else(|| opt_str(v, "cover_url"))
        .or_else(|| v.get("cover").and_then(|c| {
            if let Some(s) = c.as_str() { if !s.is_empty() { return Some(String::from(s)); } }
            c.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
        }))
}

fn manga_from(v: &Value) -> Manga {
    let id = id_field(v, "id");
    let mut authors = Vec::new();
    // author can be a string or an array of objects
    if let Some(a) = opt_str(v, "author") { authors.push(a); }
    if let Some(arr) = v.get("authors").and_then(Value::as_array) {
        for item in arr {
            if let Some(n) = item.get("name").and_then(Value::as_str) {
                let s = String::from(n);
                if !authors.contains(&s) { authors.push(s); }
            }
        }
    }
    Manga {
        key: id.clone(),
        title: String::from(str_of(v, "title")),
        cover: cover_url(v),
        authors: if authors.is_empty() { None } else { Some(authors) },
        tags: array_names(v, "genres").or_else(|| array_names(v, "tags")),
        url: if id.is_empty() { None } else {
            Some(format!("{}/manga/{}", SITE_BASE, id))
        },
        ..Default::default()
    }
}

fn data_array(v: &Value) -> &[Value] {
    v.get("data")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_else(|| v.as_array().map(|a| a.as_slice()).unwrap_or(&[]))
}

fn parse_manga_page(v: &Value, page: i32) -> MangaPageResult {
    MangaPageResult {
        entries: data_array(v).iter().map(manga_from).collect(),
        has_next_page: has_next(v, page),
    }
}

// ─── Chapter mapping ──────────────────────────────────────────────────────────
fn chapter_from(v: &Value) -> Chapter {
    let key = id_field(v, "id");
    let chapter_number = v.get("number")
        .and_then(|n| n.as_f64().or_else(|| n.as_str().and_then(|s| s.parse().ok())))
        .map(|n| n as f32);
    let volume_number = v.get("volume")
        .and_then(|n| n.as_f64().or_else(|| n.as_str().and_then(|s| s.parse().ok())))
        .map(|n| n as f32);
    let date_uploaded = opt_str(v, "publishedAt")
        .or_else(|| opt_str(v, "date"))
        .or_else(|| opt_str(v, "createdAt"))
        .and_then(|s| parse_timestamp(&s));
    let scanlators: Option<Vec<String>> = v.get("groups")
        .and_then(Value::as_array)
        .map(|arr| arr.iter()
            .filter_map(|g| g.get("name").and_then(Value::as_str).map(String::from))
            .collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .or_else(|| opt_str(v, "groupName").map(|g| { let mut a = Vec::new(); a.push(g); a }));
    Chapter {
        key,
        title: opt_str(v, "title").or_else(|| opt_str(v, "numberText")),
        chapter_number,
        volume_number,
        date_uploaded,
        scanlators,
        ..Default::default()
    }
}

fn parse_chapters(v: &Value) -> Vec<Chapter> {
    // Chapters can be in v["data"], v["chapters"], or directly as array
    let arr = v.get("chapters").and_then(Value::as_array)
        .or_else(|| v.get("data").and_then(Value::as_array))
        .or_else(|| v.as_array());
    arr.map(|a| a.iter().map(chapter_from).collect()).unwrap_or_default()
}

// ─── Source ───────────────────────────────────────────────────────────────────
struct TruyenMoe;

impl Source for TruyenMoe {
    fn new() -> Self { Self }

    fn get_search_manga_list(
        &self,
        query: Option<String>,
        page: i32,
        _filters: Vec<aidoku::FilterValue>,
    ) -> Result<MangaPageResult> {
        let page = page.max(1);
        let path = if let Some(q) = query.as_deref().filter(|s| !s.trim().is_empty()) {
            format!(
                "/v2/manga?page={}&limit={}&q={}&sort=updated_at&hasChapters=0&include=genres",
                page, PAGE_SIZE, encode_component(q.trim())
            )
        } else {
            format!(
                "/v2/manga?page={}&limit={}&sort=updated_at&hasChapters=0&include=genres",
                page, PAGE_SIZE
            )
        };
        let v = get_json(&path)?;
        Ok(parse_manga_page(&v, page))
    }

    fn get_manga_update(
        &self,
        mut manga: Manga,
        needs_details: bool,
        needs_chapters: bool,
    ) -> Result<Manga> {
        if needs_details {
            let v = get_json(&format!("/v2/manga/{}?include=genres,groups", manga.key))?;
            let d = v.get("data").unwrap_or(&v);
            manga.title = String::from(str_of(d, "title"));
            manga.description = opt_str(d, "description").or_else(|| opt_str(d, "synopsis"));
            // authors
            let mut authors = Vec::new();
            if let Some(a) = opt_str(d, "author") { authors.push(a); }
            if let Some(arr) = d.get("authors").and_then(Value::as_array) {
                for item in arr {
                    if let Some(n) = item.get("name").and_then(Value::as_str) {
                        let s = String::from(n);
                        if !authors.contains(&s) { authors.push(s); }
                    }
                }
            }
            manga.authors = if authors.is_empty() { None } else { Some(authors) };
            manga.status = status_of(str_of(d, "status"));
            manga.tags = array_names(d, "genres").or_else(|| array_names(d, "tags"));
            if let Some(c) = cover_url(d) { manga.cover = Some(c); }
            if !manga.key.is_empty() {
                manga.url = Some(format!("{}/manga/{}", SITE_BASE, manga.key));
            }
        }

        if needs_chapters {
            // Try chapters aggregate endpoint first, then chapters list
            let v = get_json(&format!("/v2/manga/{}/chapters/aggregate", manga.key))
                .or_else(|_| get_json(&format!(
                    "/v2/manga/{}/chapters?page=1&limit=500&sort=desc", manga.key
                )))?;
            manga.chapters = Some(parse_chapters(&v));
        }

        Ok(manga)
    }

    fn get_page_list(&self, _manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
        let v = get_json(&format!("/v2/chapters/{}", chapter.key))?;
        let d = v.get("data").unwrap_or(&v);

        // Try "pageUrls" (array of strings)
        if let Some(arr) = d.get("pageUrls").and_then(Value::as_array) {
            let pages: Vec<Page> = arr.iter()
                .filter_map(|u| u.as_str().filter(|s| !s.is_empty()))
                .map(|url| Page { content: PageContent::url(String::from(url)), ..Default::default() })
                .collect();
            if !pages.is_empty() { return Ok(pages); }
        }

        // Try "pages" (array of objects with url fields)
        if let Some(arr) = d.get("pages").and_then(Value::as_array) {
            let mut items: Vec<(i64, &str)> = arr.iter().filter_map(|item| {
                let pos = item.get("position").and_then(Value::as_i64).unwrap_or(0);
                let url = item.get("url")
                    .and_then(Value::as_str).filter(|s| !s.is_empty())
                    .or_else(|| item.get("optimizedUrl").and_then(Value::as_str).filter(|s| !s.is_empty()))
                    .or_else(|| item.get("imageUrl").and_then(Value::as_str).filter(|s| !s.is_empty()))?;
                Some((pos, url))
            }).collect();
            items.sort_by_key(|(p, _)| *p);
            let pages: Vec<Page> = items.iter()
                .map(|(_, url)| Page { content: PageContent::url(String::from(*url)), ..Default::default() })
                .collect();
            if !pages.is_empty() { return Ok(pages); }
        }

        bail!("Chương {} không có ảnh (chapter bị khóa hoặc API thay đổi)", chapter.key)
    }
}

// ─── Listing provider ─────────────────────────────────────────────────────────
impl ListingProvider for TruyenMoe {
    fn get_manga_list(&self, listing: Listing, page: i32) -> Result<MangaPageResult> {
        let page = page.max(1);
        let path = match listing.id.as_str() {
            "popular" => format!(
                "/v2/manga/top?page={}&limit={}&sort_by=views&time=all_time&include=genres",
                page, PAGE_SIZE
            ),
            _ => format!(
                "/v2/manga?page={}&limit={}&sort=updated_at&hasChapters=0&include=genres",
                page, PAGE_SIZE
            ),
        };
        let v = get_json(&path)?;
        Ok(parse_manga_page(&v, page))
    }
}

register_source!(TruyenMoe, ListingProvider);
