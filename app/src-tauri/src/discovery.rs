use crate::{
    db::Database,
    models::{BookDiscoveryResult, ExternalBook},
    preferences::ReaderPreferences,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use tauri::State;
use tokio::sync::Mutex as AsyncMutex;

const CACHE_TTL_HOURS: i64 = 24;
const MAX_RESULTS: usize = 6;
const SEARCH_ENDPOINT: &str = "https://openlibrary.org/search.json";
static LAST_OPEN_LIBRARY_REQUEST: OnceLock<AsyncMutex<Option<tokio::time::Instant>>> =
    OnceLock::new();

#[derive(Debug, Deserialize)]
struct OpenLibraryResponse {
    #[serde(default)]
    docs: Vec<OpenLibraryDocument>,
}

#[derive(Debug, Deserialize)]
struct OpenLibraryDocument {
    key: String,
    title: Option<String>,
    #[serde(default)]
    author_name: Vec<String>,
    first_publish_year: Option<i32>,
    edition_count: Option<u32>,
}

#[tauri::command]
pub async fn search_books(
    query: String,
    force_refresh: bool,
    database: State<'_, Arc<Database>>,
) -> Result<BookDiscoveryResult, String> {
    let enabled = database
        .preference("reader")
        .map_err(|_| "无法读取在线书籍发现设置。".to_string())?
        .and_then(|json| serde_json::from_str::<ReaderPreferences>(&json).ok())
        .unwrap_or_default()
        .normalized()
        .enable_online_discovery;
    if !enabled {
        return Err("请先在偏好设置中明确启用在线书籍发现。".into());
    }

    let query = validate_query(&query)?;
    let query_hash = query_hash(&query);
    let cached = database
        .discovery_search_cache(&query_hash)
        .map_err(|_| "无法读取书目缓存。".to_string())?
        .and_then(|(json, fetched_at)| {
            serde_json::from_str::<Vec<ExternalBook>>(&json)
                .ok()
                .map(|items| (sanitize_cached_items(items), fetched_at))
        });
    if !force_refresh {
        if let Some((items, fetched_at)) = &cached {
            if cache_is_fresh(fetched_at, Utc::now()) {
                return Ok(BookDiscoveryResult {
                    message: cache_message(items.len(), false),
                    items: items.clone(),
                    from_cache: true,
                    stale: false,
                });
            }
        }
    }

    wait_for_open_library_slot().await;
    match fetch_open_library(&query).await {
        Ok(items) => {
            let json =
                serde_json::to_string(&items).map_err(|_| "无法缓存书目结果。".to_string())?;
            database
                .save_discovery_search_cache(&query_hash, &query, &json, &Utc::now().to_rfc3339())
                .map_err(|_| "无法保存书目缓存。".to_string())?;
            Ok(BookDiscoveryResult {
                message: if items.is_empty() {
                    "Open Library没有找到匹配书目，可以修改关键词再试。".into()
                } else {
                    format!("从Open Library找到{}条书目。", items.len())
                },
                items,
                from_cache: false,
                stale: false,
            })
        }
        Err(error) => {
            if let Some((items, _)) = cached {
                return Ok(BookDiscoveryResult {
                    message: format!("{error} 当前显示的是本机上次缓存的结果。"),
                    items,
                    from_cache: true,
                    stale: true,
                });
            }
            Err(error)
        }
    }
}

async fn wait_for_open_library_slot() {
    let gate = LAST_OPEN_LIBRARY_REQUEST.get_or_init(|| AsyncMutex::new(None));
    let mut last_request = gate.lock().await;
    if let Some(previous) = *last_request {
        tokio::time::sleep_until(previous + std::time::Duration::from_secs(1)).await;
    }
    *last_request = Some(tokio::time::Instant::now());
}

fn validate_query(value: &str) -> Result<String, String> {
    let query = value.trim();
    let length = query.chars().count();
    if !(2..=120).contains(&length) {
        return Err("请输入2到120个字符的书名或作者关键词。".into());
    }
    Ok(query.to_string())
}

fn query_hash(query: &str) -> String {
    let normalized = query.to_lowercase();
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cache_is_fresh(value: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(value)
        .map(|fetched| {
            now.signed_duration_since(fetched.with_timezone(&Utc))
                <= Duration::hours(CACHE_TTL_HOURS)
        })
        .unwrap_or(false)
}

fn cache_message(count: usize, stale: bool) -> String {
    let suffix = if stale {
        "（离线缓存）"
    } else {
        "（本机缓存）"
    };
    if count == 0 {
        format!("上次查询没有匹配书目{suffix}。")
    } else {
        format!("显示{count}条Open Library书目{suffix}。")
    }
}

async fn fetch_open_library(query: &str) -> Result<Vec<ExternalBook>, String> {
    fetch_open_library_from(query, SEARCH_ENDPOINT).await
}

async fn fetch_open_library_from(query: &str, endpoint: &str) -> Result<Vec<ExternalBook>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(4))
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Shuyu/0.1 (Windows desktop book discovery)")
        .build()
        .map_err(|_| "无法初始化Open Library连接。".to_string())?;
    let mut url =
        reqwest::Url::parse(endpoint).map_err(|_| "无法初始化书目搜索地址。".to_string())?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &MAX_RESULTS.to_string())
        .append_pair(
            "fields",
            "key,title,author_name,first_publish_year,edition_count",
        );
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "无法连接Open Library，请检查网络后重试。".to_string())?;
    if response.status().as_u16() == 429 {
        return Err("Open Library请求较多，请稍后再试。".into());
    }
    if !response.status().is_success() {
        return Err(format!(
            "Open Library返回HTTP状态码{}。",
            response.status().as_u16()
        ));
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| "Open Library返回了无法读取的书目数据。".to_string())?;
    if body.len() > 1_000_000 {
        return Err("Open Library返回的数据超出大小限制。".into());
    }
    let response: OpenLibraryResponse = serde_json::from_slice(&body)
        .map_err(|_| "Open Library返回了无法识别的书目数据。".to_string())?;
    Ok(map_documents(response.docs))
}

fn map_documents(documents: Vec<OpenLibraryDocument>) -> Vec<ExternalBook> {
    let mut books = Vec::new();
    for document in documents {
        if books.len() >= MAX_RESULTS {
            break;
        }
        let Some(title) = document
            .title
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
        else {
            continue;
        };
        let work_id = document
            .key
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        if !valid_work_id(&work_id)
            || books
                .iter()
                .any(|book: &ExternalBook| book.work_id == work_id)
        {
            continue;
        }
        let author = document
            .author_name
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .take(2)
            .collect::<Vec<_>>()
            .join("、")
            .chars()
            .take(120)
            .collect::<String>();
        books.push(ExternalBook {
            source_url: format!("https://openlibrary.org/works/{work_id}"),
            work_id,
            title: title.chars().take(200).collect(),
            author: if author.is_empty() {
                "作者信息暂缺".into()
            } else {
                author
            },
            first_publish_year: document
                .first_publish_year
                .filter(|year| (0..=2100).contains(year)),
            edition_count: document.edition_count.filter(|count| *count > 0),
        });
    }
    books
}

fn sanitize_cached_items(items: Vec<ExternalBook>) -> Vec<ExternalBook> {
    items
        .into_iter()
        .filter_map(|mut item| {
            if !valid_work_id(&item.work_id) || item.title.trim().is_empty() {
                return None;
            }
            item.title = item.title.chars().take(200).collect();
            item.author = item.author.chars().take(120).collect();
            item.source_url = format!("https://openlibrary.org/works/{}", item.work_id);
            Some(item)
        })
        .take(MAX_RESULTS)
        .collect()
}

fn valid_work_id(value: &str) -> bool {
    let Some(digits) = value
        .strip_prefix("OL")
        .and_then(|value| value.strip_suffix('W'))
    else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{
        cache_is_fresh, map_documents, query_hash, sanitize_cached_items, validate_query,
        ExternalBook, OpenLibraryDocument,
    };
    use chrono::{Duration, Utc};

    #[test]
    fn discovery_query_is_bounded_and_trimmed() {
        assert_eq!(validate_query("  三体  ").unwrap(), "三体");
        assert!(validate_query(" ").is_err());
        assert!(validate_query(&"书".repeat(121)).is_err());
        assert_eq!(query_hash("科幻"), query_hash("科幻"));
    }

    #[test]
    fn maps_only_real_open_library_work_ids_and_metadata() {
        let items = map_documents(vec![
            OpenLibraryDocument {
                key: "/works/OL123W".into(),
                title: Some(" 示例书目 ".into()),
                author_name: vec!["作者甲".into(), "作者乙".into(), "其他".into()],
                first_publish_year: Some(2001),
                edition_count: Some(4),
            },
            OpenLibraryDocument {
                key: "/works/OL123W".into(),
                title: Some("重复项".into()),
                author_name: vec![],
                first_publish_year: None,
                edition_count: None,
            },
            OpenLibraryDocument {
                key: "javascript:alert(1)".into(),
                title: Some("无效来源".into()),
                author_name: vec![],
                first_publish_year: None,
                edition_count: None,
            },
        ]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "示例书目");
        assert_eq!(items[0].author, "作者甲、作者乙");
        assert_eq!(items[0].source_url, "https://openlibrary.org/works/OL123W");
    }

    #[test]
    fn discovery_cache_expires_after_one_day() {
        let now = Utc::now();
        assert!(cache_is_fresh(&now.to_rfc3339(), now));
        assert!(!cache_is_fresh(
            &(now - Duration::hours(25)).to_rfc3339(),
            now
        ));
        assert!(!cache_is_fresh("invalid timestamp", now));
    }

    #[test]
    fn cached_source_links_are_rebuilt_from_validated_work_ids() {
        let mut item = map_documents(vec![OpenLibraryDocument {
            key: "/works/OL456W".into(),
            title: Some("书名".into()),
            author_name: vec![],
            first_publish_year: None,
            edition_count: None,
        }])
        .remove(0);
        item.source_url = "javascript:alert(1)".into();
        let cached = sanitize_cached_items(vec![item]);
        assert_eq!(cached[0].source_url, "https://openlibrary.org/works/OL456W");
        assert!(sanitize_cached_items(vec![ExternalBook {
            work_id: "bad".into(),
            title: "书名".into(),
            author: "作者信息暂缺".into(),
            first_publish_year: None,
            edition_count: None,
            source_url: "https://example.com".into(),
        }])
        .is_empty());
    }

    fn mock_open_library(status: &'static str, body: String) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}/search.json")
    }

    #[tokio::test]
    async fn search_adapter_maps_provider_response_and_handles_rate_limit() {
        let endpoint = mock_open_library(
            "200 OK",
            r#"{"docs":[{"key":"/works/OL789W","title":"测试书","author_name":["测试作者"],"first_publish_year":2020,"edition_count":3}]}"#.into(),
        );
        let items = super::fetch_open_library_from("测试书", &endpoint)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].work_id, "OL789W");
        assert_eq!(items[0].source_url, "https://openlibrary.org/works/OL789W");
        assert_eq!(items[0].first_publish_year, Some(2020));

        let endpoint = mock_open_library("429 Too Many Requests", "{}".into());
        let error = super::fetch_open_library_from("测试书", &endpoint)
            .await
            .unwrap_err();
        assert!(error.contains("请求较多"));
    }
}
