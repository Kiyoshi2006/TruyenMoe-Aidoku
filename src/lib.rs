//! TruyenMoe for Aidoku — scrapes the public HTML of https://truyen.moe
//!
//! Browse / search / details / chapter list are supported.
//! Chapters whose page images are served through the site's protected IMGX
//! reader are NOT decoded; for those a clear error with the web link is shown.
#![no_std]

use aidoku::{
    alloc::{format, String, Vec},
    imports::{
        html::{Document, Element},
        net::Request,
    },
    prelude::*,
    Chapter, Listing, ListingProvider, Manga, MangaPageResult, MangaStatus, Page, PageContent,
    Result, Source,
};

const SITE: &str = "https://truyen.moe";
const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

// ─── helpers ──────────────────────────────────────────────────────────────────
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

fn hex_char(n: u8) -> char {
    if n < 10 { (b'0' + n) as char } else { (b'A' + n - 10) as char }
}

fn get_html(url: &str) -> Result<Document> {
    let resp = Request::get(url)?
        .header("User-Agent", UA)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "vi,en;q=0.8")
        .header("Referer", "https://truyen.moe/")
        .send()?;
    let status = resp.status_code();
    if !(200..300).contains(&status) {
        bail!("HTTP {} tại {}", status, url);
    }
    Ok(resp.get_html()?)
}

fn abs_url(href: &str) -> String {
    if href.starts_with("http") {
        String::from(href)
    } else if href.starts_with('/') {
        format!("{}{}", SITE, href)
    } else {
        format!("{}/{}", SITE, href)
    }
}

/// strip "#media-fallback=…" etc.
fn clean_img(src: &str) -> String {
    match src.find('#') {
        Some(i) => String::from(&src[..i]),
        None => String::from(src),
    }
}

fn key_from_href(href: &str) -> Option<String> {
    let i = href.find("/manga/")?;
    let rest = &href[i + 7..];
    let end = rest.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(rest.len());
    let key = &rest[..end];
    if key.is_empty() { None } else { Some(String::from(key)) }
}

fn text_of(el: &Element) -> String {
    el.text().map(|t| String::from(t.trim())).unwrap_or_default()
}

fn status_from(s: &str) -> MangaStatus {
    match s.trim() {
        "Còn tiếp" => MangaStatus::Ongoing,
        "Hoàn thành" => MangaStatus::Completed,
        "Tạm dừng" => MangaStatus::Hiatus,
        _ => MangaStatus::Unknown,
    }
}

fn parse_num(s: &str) -> Option<i64> {
    let mut n: i64 = 0;
    if s.is_empty() { return None; }
    for c in s.bytes() {
        if !c.is_ascii_digit() { return None; }
        n = n * 10 + (c - b'0') as i64;
    }
    Some(n)
}

fn is_leap(y: i64) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

/// "dd/mm/yyyy" → unix seconds (UTC midnight)
fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut it = s.split('/');
    let d = parse_num(it.next()?)?;
    let m = parse_num(it.next()?)?;
    let y = parse_num(it.next()?.get(0..4)?)?;
    if !(1..=12).contains(&m) { return None; }
    const D: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let yy = y - 1;
    let days = yy * 365 + yy / 4 - yy / 100 + yy / 400
        + D[m as usize] + if m > 2 && is_leap(y) { 1 } else { 0 }
        + d - 1 - 719_162;
    Some(days * 86_400)
}

// ─── listing ──────────────────────────────────────────────────────────────────
fn parse_listing(doc: &Document) -> MangaPageResult {
    let mut entries: Vec<Manga> = Vec::new();
    if let Some(cards) = doc.select("article.manga-card") {
        for card in cards {
            let Some(a) = card.select_first("a[href^=\"/manga/\"]") else { continue };
            let Some(href) = a.attr("href") else { continue };
            let Some(key) = key_from_href(&href) else { continue };
            let title = card
                .select_first("h3")
                .map(|h| h.attr("title").unwrap_or_else(|| text_of(&h)))
                .unwrap_or_default();
            let cover = card
                .select_first("img")
                .and_then(|i| i.attr("src"))
                .map(|s| clean_img(&s))
                .filter(|s| s.starts_with("http"));
            entries.push(Manga {
                url: Some(format!("{}/manga/{}", SITE, key)),
                key,
                title,
                cover,
                ..Default::default()
            });
        }
    }
    let has_next_page = doc
        .select_first("a[aria-label=\"Trang sau\"]:not(.is-disabled)")
        .is_some();
    MangaPageResult { entries, has_next_page }
}

fn fetch_listing(page: i32, sort: &str, query: Option<&str>) -> Result<MangaPageResult> {
    let mut url = format!("{}/manga?page={}&sort={}", SITE, page.max(1), sort);
    if let Some(q) = query {
        url.push_str("&q=");
        url.push_str(&encode_component(q));
    }
    let doc = get_html(&url)?;
    Ok(parse_listing(&doc))
}

// ─── chapters ─────────────────────────────────────────────────────────────────
fn parse_chapters(doc: &Document, key: &str, out: &mut Vec<Chapter>) {
    let Some(items) = doc.select("li.chapter") else { return };
    for li in items {
        let Some(a) = li.select_first("a.chapter-link") else { continue };
        let num_s = a.attr("data-chapter-number").unwrap_or_default();
        if num_s.is_empty() { continue; }
        let number = num_s.parse::<f32>().ok();
        let title = li
            .select_first(".chapter-title")
            .map(|t| text_of(&t))
            .filter(|t| !t.is_empty());
        let group = li
            .select_first(".chapter-sub-text")
            .map(|t| text_of(&t))
            .filter(|t| !t.is_empty());
        let date = li
            .select_first(".chapter-time")
            .and_then(|t| t.attr("title"))
            .and_then(|t| {
                let d = t.trim().rsplit(' ').next().map(String::from)?;
                parse_date(&d)
            });
        let mut scanlators = None;
        if let Some(g) = group {
            let mut v = Vec::new();
            v.push(g);
            scanlators = Some(v);
        }
        out.push(Chapter {
            url: Some(format!("{}/manga/{}/chapters/{}", SITE, key, num_s)),
            key: num_s,
            title,
            chapter_number: number,
            date_uploaded: date,
            scanlators,
            language: Some(String::from("vi")),
            ..Default::default()
        });
    }
}

// ─── source ───────────────────────────────────────────────────────────────────
struct TruyenMoe;

impl Source for TruyenMoe {
    fn new() -> Self { Self }

    fn get_search_manga_list(
        &self,
        query: Option<String>,
        page: i32,
        _filters: Vec<aidoku::FilterValue>,
    ) -> Result<MangaPageResult> {
        let q = query.as_deref().map(str::trim).filter(|s| !s.is_empty());
        fetch_listing(page, "updated_desc", q)
    }

    fn get_manga_update(
        &self,
        mut manga: Manga,
        needs_details: bool,
        needs_chapters: bool,
    ) -> Result<Manga> {
        let url = format!("{}/manga/{}", SITE, manga.key);
        let doc = get_html(&url)?;

        if needs_details {
            if let Some(t) = doc.select_first("h1.manga-detail-title") {
                let t = text_of(&t);
                if !t.is_empty() { manga.title = t; }
            }
            if let Some(i) = doc.select_first(".cover--detail img").and_then(|i| i.attr("src")) {
                let c = clean_img(&i);
                if c.starts_with("http") { manga.cover = Some(c); }
            }
            let mut authors = Vec::new();
            if let Some(list) = doc.select(".manga-detail-meta-line--author a") {
                for a in list {
                    let t = text_of(&a);
                    if !t.is_empty() { authors.push(t); }
                }
            }
            manga.authors = if authors.is_empty() { None } else { Some(authors) };
            let mut tags = Vec::new();
            if let Some(list) = doc.select(".manga-detail-genre-chips a.chip") {
                for a in list {
                    let t = text_of(&a);
                    if !t.is_empty() { tags.push(t); }
                }
            }
            manga.tags = if tags.is_empty() { None } else { Some(tags) };
            manga.description = doc
                .select_first("[data-description-content]")
                .map(|d| text_of(&d))
                .filter(|d| !d.is_empty());
            if let Some(s) = doc.select_first(".manga-status-pill") {
                manga.status = status_from(&text_of(&s));
            }
            manga.url = Some(url.clone());
        }

        if needs_chapters {
            let mut chapters: Vec<Chapter> = Vec::new();
            parse_chapters(&doc, &manga.key, &mut chapters);
            // follow chapter pagination if present
            let mut next = doc
                .select_first(".chapter-list-pagination a[aria-label=\"Trang sau\"]:not(.is-disabled)")
                .and_then(|a| a.attr("href"));
            let mut guard = 0;
            while let Some(href) = next.take() {
                guard += 1;
                if guard > 40 || href == "#" { break; }
                let d = get_html(&abs_url(&href))?;
                let before = chapters.len();
                parse_chapters(&d, &manga.key, &mut chapters);
                if chapters.len() == before { break; }
                next = d
                    .select_first(".chapter-list-pagination a[aria-label=\"Trang sau\"]:not(.is-disabled)")
                    .and_then(|a| a.attr("href"));
            }
            manga.chapters = Some(chapters);
        }
        Ok(manga)
    }

    fn get_page_list(&self, manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
        let url = format!("{}/manga/{}/chapters/{}", SITE, manga.key, chapter.key);
        let doc = get_html(&url)?;
        let mut pages: Vec<Page> = Vec::new();
        if let Some(imgs) = doc.select("img[data-page-index]") {
            for img in imgs {
                let src = img
                    .attr("data-src")
                    .or_else(|| img.attr("src"))
                    .unwrap_or_default();
                if src.starts_with("http") {
                    pages.push(Page {
                        content: PageContent::url(clean_img(&src)),
                        ..Default::default()
                    });
                }
            }
        }
        if pages.is_empty() {
            bail!(
                "Chương này dùng IMGX (ảnh mã hóa), Aidoku không đọc được. Mở trên web: {}",
                url
            );
        }
        Ok(pages)
    }
}

impl ListingProvider for TruyenMoe {
    fn get_manga_list(&self, listing: Listing, page: i32) -> Result<MangaPageResult> {
        let sort = match listing.id.as_str() {
            "popular" => "views_desc",
            _ => "updated_desc",
        };
        fetch_listing(page, sort, None)
    }
}

register_source!(TruyenMoe, ListingProvider);
