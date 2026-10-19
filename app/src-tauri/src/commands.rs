use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::{
    db::{Database, DbError},
    importer::{self, ImportError},
    models::{BookSummary, Chapter, ChapterContent, DailyQuote, ImportResult, ReadingAnchor},
    preferences::ReaderPreferences,
};

#[derive(Default)]
pub struct ImportManager {
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgressEvent {
    import_id: String,
    file_name: String,
    file_index: usize,
    total_files: usize,
    stage: String,
    message: String,
    result: Option<ImportResult>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportFinishedEvent {
    import_id: String,
    imported_count: usize,
    duplicate_count: usize,
    failed_count: usize,
    cancelled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyQuoteSelectionResult {
    quote: Option<DailyQuote>,
    ai_error: Option<String>,
}

#[tauri::command]
pub fn get_preferences(database: State<'_, Arc<Database>>) -> Result<ReaderPreferences, String> {
    let stored = database.preference("reader").map_err(display_error)?;
    Ok(stored
        .and_then(|json| serde_json::from_str::<ReaderPreferences>(&json).ok())
        .unwrap_or_default()
        .normalized())
}

#[tauri::command]
pub fn save_preferences(
    database: State<'_, Arc<Database>>,
    preferences: ReaderPreferences,
) -> Result<ReaderPreferences, String> {
    let normalized = preferences.normalized();
    let json = serde_json::to_string(&normalized).map_err(|error| error.to_string())?;
    database
        .save_preference("reader", &json)
        .map_err(display_error)?;
    Ok(normalized)
}

#[tauri::command]
pub fn get_library_count(database: State<'_, Arc<Database>>) -> Result<u64, String> {
    database.library_count().map_err(display_error)
}

#[tauri::command]
pub async fn list_books(
    app: AppHandle,
    database: State<'_, Arc<Database>>,
    sort: Option<String>,
) -> Result<Vec<BookSummary>, String> {
    let books_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("books");
    let database = database.inner().clone();
    tokio::task::spawn_blocking(move || {
        importer::repair_missing_authors(&database, &books_directory).map_err(display_error)?;
        database
            .list_books(sort.as_deref().unwrap_or("recent"))
            .map_err(display_error)
    })
    .await
    .map_err(|error| format!("书架元数据处理失败：{error}"))?
}

#[tauri::command]
pub async fn get_book(
    book_id: String,
    app: AppHandle,
    database: State<'_, Arc<Database>>,
) -> Result<Option<BookSummary>, String> {
    let books_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("books");
    let database = database.inner().clone();
    tokio::task::spawn_blocking(move || {
        importer::repair_missing_authors(&database, &books_directory).map_err(display_error)?;
        database.get_book(&book_id).map_err(display_error)
    })
    .await
    .map_err(|error| format!("书籍元数据处理失败：{error}"))?
}

#[tauri::command]
pub async fn list_chapters(
    book_id: String,
    app: AppHandle,
    database: State<'_, Arc<Database>>,
) -> Result<Vec<Chapter>, String> {
    let books_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("books");
    let database = database.inner().clone();
    tokio::task::spawn_blocking(move || {
        importer::repair_legacy_single_chapter_index(&database, &books_directory, &book_id)
            .map_err(display_error)?;
        database.list_chapters(&book_id).map_err(display_error)
    })
    .await
    .map_err(|error| format!("章节目录处理失败：{error}"))?
}

#[tauri::command]
pub fn get_chapter_content(
    chapter_id: String,
    database: State<'_, Arc<Database>>,
) -> Result<Option<ChapterContent>, String> {
    database.chapter_content(&chapter_id).map_err(display_error)
}

#[tauri::command]
pub fn touch_book(book_id: String, database: State<'_, Arc<Database>>) -> Result<(), String> {
    database.touch_book(&book_id).map_err(display_error)
}

#[tauri::command]
pub fn get_reading_position(
    book_id: String,
    database: State<'_, Arc<Database>>,
) -> Result<Option<ReadingAnchor>, String> {
    database
        .get_reading_position(&book_id)
        .map_err(display_error)
}

#[tauri::command]
pub fn save_reading_position(
    anchor: ReadingAnchor,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    database
        .save_reading_position(&anchor)
        .map_err(display_error)
}

#[tauri::command]
pub fn add_bookmark(
    book_id: String,
    paragraph_id: String,
    intra_utf16: usize,
    note: Option<String>,
    database: State<'_, Arc<Database>>,
) -> Result<bool, String> {
    database
        .add_bookmark(&book_id, &paragraph_id, intra_utf16, note.as_deref())
        .map_err(display_error)
}

#[tauri::command]
pub fn list_bookmarks(
    book_id: Option<String>,
    database: State<'_, Arc<Database>>,
) -> Result<Vec<crate::models::Bookmark>, String> {
    database
        .list_bookmarks(book_id.as_deref())
        .map_err(display_error)
}

#[tauri::command]
pub fn delete_bookmark(
    bookmark_id: String,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    database
        .delete_bookmark(&bookmark_id)
        .map_err(display_error)
}

#[tauri::command]
pub fn add_highlight(
    book_id: String,
    start_paragraph_id: String,
    start_intra_utf16: usize,
    end_paragraph_id: String,
    end_intra_utf16: usize,
    database: State<'_, Arc<Database>>,
) -> Result<Option<crate::models::Highlight>, String> {
    database
        .add_highlight(
            &book_id,
            &start_paragraph_id,
            start_intra_utf16,
            &end_paragraph_id,
            end_intra_utf16,
        )
        .map_err(display_error)
}

#[tauri::command]
pub fn list_highlights(
    book_id: Option<String>,
    database: State<'_, Arc<Database>>,
) -> Result<Vec<crate::models::Highlight>, String> {
    database
        .list_highlights(book_id.as_deref())
        .map_err(display_error)
}

#[tauri::command]
pub fn delete_highlight(
    highlight_id: String,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    database
        .delete_highlight(&highlight_id)
        .map_err(display_error)
}

#[tauri::command]
pub fn record_paragraph_view(
    book_id: String,
    paragraph_id: String,
    dwell_ms: u64,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    database
        .record_paragraph_view(&book_id, &paragraph_id, dwell_ms)
        .map_err(display_error)
}

#[tauri::command]
pub fn start_reading_session(
    book_id: String,
    anchor: Option<ReadingAnchor>,
    database: State<'_, Arc<Database>>,
) -> Result<String, String> {
    database
        .start_reading_session(&book_id, anchor.as_ref())
        .map_err(display_error)
}

#[tauri::command]
pub fn end_reading_session(
    session_id: String,
    anchor: Option<ReadingAnchor>,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    database
        .end_reading_session(&session_id, anchor.as_ref())
        .map_err(display_error)
}

#[tauri::command]
pub fn get_daily_quote(database: State<'_, Arc<Database>>) -> Result<Option<DailyQuote>, String> {
    let local_date = chrono::Local::now().date_naive().to_string();
    database
        .get_or_create_daily_quote(&local_date)
        .map_err(display_error)
}

#[tauri::command]
pub async fn select_ai_daily_quote(
    app: AppHandle,
    force_refresh: bool,
    database: State<'_, Arc<Database>>,
    auth_manager: State<'_, Arc<crate::siwc::ChatGptAuthManager>>,
    requests: State<'_, Arc<crate::ai_service::AiRequestManager>>,
) -> Result<DailyQuoteSelectionResult, String> {
    let database = database.inner().clone();
    let local_date = chrono::Local::now().date_naive().to_string();
    let cached = database
        .get_or_create_daily_quote(&local_date)
        .map_err(display_error)?;
    if cached
        .as_ref()
        .is_some_and(|quote| quote.selection_method == "ai")
        && !force_refresh
    {
        return Ok(DailyQuoteSelectionResult {
            quote: cached,
            ai_error: None,
        });
    }
    if cached
        .as_ref()
        .is_some_and(|quote| quote.selection_method == "ai_fallback" && !force_refresh)
    {
        return Ok(DailyQuoteSelectionResult {
            quote: cached,
            ai_error: None,
        });
    }

    let ai_enabled = crate::ai_service::daily_quote_ai_enabled(&database);
    if !ai_enabled {
        if force_refresh {
            let candidates = database
                .daily_quote_candidates(&local_date)
                .map_err(display_error)?;
            let candidates =
                crate::quotes::candidates_for_quote_refresh(&candidates, cached.as_ref());
            if let Some(candidate) = candidates.first() {
                if let Some(quote) = database
                    .save_daily_quote(&local_date, candidate, "local")
                    .map_err(display_error)?
                {
                    return Ok(DailyQuoteSelectionResult {
                        quote: Some(quote),
                        ai_error: None,
                    });
                }
            }
        }
        return Ok(DailyQuoteSelectionResult {
            quote: cached,
            ai_error: None,
        });
    }
    let candidates = database
        .daily_quote_candidates(&local_date)
        .map_err(display_error)?;
    let candidates = if force_refresh {
        crate::quotes::candidates_for_quote_refresh(&candidates, cached.as_ref())
    } else {
        candidates
    };
    if candidates.is_empty() {
        return Ok(DailyQuoteSelectionResult {
            quote: cached,
            ai_error: None,
        });
    }
    let mut ai_error = None;
    match crate::ai_service::select_chatgpt_daily_quote_candidate(
        &app,
        &database,
        auth_manager.inner(),
        requests.inner(),
        &candidates,
    )
    .await
    {
        Ok(Some(candidate_id)) => {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.candidate_id == candidate_id)
            {
                if let Some(quote) = database
                    .save_daily_quote(&local_date, candidate, "ai")
                    .map_err(display_error)?
                {
                    return Ok(DailyQuoteSelectionResult {
                        quote: Some(quote),
                        ai_error: None,
                    });
                }
            }
        }
        Ok(None) => {
            return Ok(DailyQuoteSelectionResult {
                quote: cached,
                ai_error: None,
            });
        }
        Err(error) => ai_error = Some(error),
    }
    if let Some(cached_quote) = cached.as_ref() {
        if let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.book_id == cached_quote.book_id
                && candidate.chapter_id == cached_quote.chapter_id
                && candidate.paragraph_id == cached_quote.paragraph_id
                && candidate.start_intra_utf16 == cached_quote.start_intra_utf16
                && candidate.text == cached_quote.text_snapshot
        }) {
            if let Some(quote) = database
                .save_daily_quote(&local_date, candidate, "ai_fallback")
                .map_err(display_error)?
            {
                return Ok(DailyQuoteSelectionResult {
                    quote: Some(quote),
                    ai_error,
                });
            }
        }
    }
    if force_refresh {
        if let Some(candidate) = candidates.first() {
            if let Some(quote) = database
                .save_daily_quote(&local_date, candidate, "ai_fallback")
                .map_err(display_error)?
            {
                return Ok(DailyQuoteSelectionResult {
                    quote: Some(quote),
                    ai_error,
                });
            }
        }
    }
    Ok(DailyQuoteSelectionResult {
        quote: cached,
        ai_error,
    })
}

#[tauri::command]
pub fn update_book_metadata(
    book_id: String,
    title: String,
    author: String,
    database: State<'_, Arc<Database>>,
) -> Result<BookSummary, String> {
    let updated = database
        .update_book_metadata(&book_id, &title, &author)
        .map_err(display_error)?;
    if !updated {
        return Err("书名不能为空，且书名最多160字、作者最多120字。".into());
    }
    database
        .get_book(&book_id)
        .map_err(display_error)?
        .ok_or_else(|| "书籍已移出书架。".into())
}

#[tauri::command]
pub fn delete_book(
    book_id: String,
    app: AppHandle,
    database: State<'_, Arc<Database>>,
) -> Result<(), String> {
    let id = Uuid::parse_str(&book_id).map_err(|_| "书籍编号无效。".to_string())?;
    let path = database
        .begin_delete_book(&book_id)
        .map_err(display_error)?
        .ok_or_else(|| "书籍不在书架中。".to_string())?;
    let books_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("books");
    let expected = books_dir.join(format!("{id}.txt"));
    let stored = PathBuf::from(&path);
    let permitted = stored.file_name() == expected.file_name()
        && stored.parent().is_some_and(|parent| parent == books_dir);
    if !permitted {
        let _ = database.restore_book_after_delete_failure(&book_id);
        return Err("托管文件路径校验失败，未执行删除。".into());
    }
    if let Err(error) = std::fs::remove_file(&expected) {
        if error.kind() != std::io::ErrorKind::NotFound {
            let _ = database.restore_book_after_delete_failure(&book_id);
            return Err(format!("未能移出书籍；数据仍保留在书架。{error}"));
        }
    }
    database.finish_delete_book(&book_id).map_err(display_error)
}

#[tauri::command]
pub async fn start_import(
    app: AppHandle,
    database: State<'_, Arc<Database>>,
    manager: State<'_, Arc<ImportManager>>,
    paths: Vec<String>,
    encoding: Option<String>,
    confirmed_large_file: Option<bool>,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("没有选择 TXT 文件。".into());
    }
    if paths.len() > 40 {
        return Err("一次最多导入 40 本 TXT。".into());
    }
    for path in &paths {
        if !PathBuf::from(path)
            .extension()
            .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("txt"))
        {
            return Err(format!(
                "仅支持 TXT 文件：{}",
                PathBuf::from(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
        }
    }

    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let import_id = Uuid::new_v4().to_string();
    let cancelled = Arc::new(AtomicBool::new(false));
    manager
        .active
        .lock()
        .map_err(|_| "导入任务状态不可用。".to_string())?
        .insert(import_id.clone(), cancelled.clone());
    let database = database.inner().clone();
    let manager = manager.inner().clone();
    let event_import_id = import_id.clone();
    let allow_large = confirmed_large_file.unwrap_or(false);

    tauri::async_runtime::spawn_blocking(move || {
        let mut imported_count = 0;
        let mut duplicate_count = 0;
        let mut failed_count = 0;
        let total_files = paths.len();
        let mut batch_cancelled = false;

        for (file_index, path) in paths.into_iter().enumerate() {
            if cancelled.load(Ordering::Relaxed) {
                batch_cancelled = true;
                break;
            }
            let source_path = PathBuf::from(path);
            let file_name = source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let _ = app.emit(
                "book-import-progress",
                ImportProgressEvent {
                    import_id: event_import_id.clone(),
                    file_name: file_name.clone(),
                    file_index,
                    total_files,
                    stage: "queued".into(),
                    message: "等待导入".into(),
                    result: None,
                },
            );

            let app_for_progress = app.clone();
            let id_for_progress = event_import_id.clone();
            let name_for_progress = file_name.clone();
            let result = importer::import_txt_file(
                &database,
                &app_data.join("books"),
                &source_path,
                encoding.as_deref(),
                allow_large,
                &cancelled,
                |stage| {
                    let message = match stage {
                        "reading" => "正在安全读取原文件".to_string(),
                        "decoding" => "正在识别文本编码".to_string(),
                        "parsing" => "正在识别章节与段落".to_string(),
                        "saving" => "正在保存到本地书架".to_string(),
                        _ => "正在导入".to_string(),
                    };
                    let _ = app_for_progress.emit(
                        "book-import-progress",
                        ImportProgressEvent {
                            import_id: id_for_progress.clone(),
                            file_name: name_for_progress.clone(),
                            file_index,
                            total_files,
                            stage: stage.into(),
                            message,
                            result: None,
                        },
                    );
                },
            );

            match result {
                Ok(import_result) => {
                    match import_result.status.as_str() {
                        "imported" => imported_count += 1,
                        "duplicate" => duplicate_count += 1,
                        _ => {}
                    }
                    let _ = app.emit(
                        "book-import-progress",
                        ImportProgressEvent {
                            import_id: event_import_id.clone(),
                            file_name,
                            file_index,
                            total_files,
                            stage: "completed".into(),
                            message: if import_result.status == "duplicate" {
                                "这本书已在书架".into()
                            } else {
                                "导入完成".into()
                            },
                            result: Some(import_result),
                        },
                    );
                }
                Err(ImportError::LargeFileNeedsConfirmation) => {
                    failed_count += 1;
                    let _ = app.emit(
                        "book-import-progress",
                        ImportProgressEvent {
                            import_id: event_import_id.clone(),
                            file_name,
                            file_index,
                            total_files,
                            stage: "needs-confirmation".into(),
                            message: "文件超过 100 MiB，请确认是否继续导入。".into(),
                            result: None,
                        },
                    );
                }
                Err(ImportError::Cancelled) => {
                    batch_cancelled = true;
                    let _ = app.emit(
                        "book-import-progress",
                        ImportProgressEvent {
                            import_id: event_import_id.clone(),
                            file_name,
                            file_index,
                            total_files,
                            stage: "cancelled".into(),
                            message: "已取消导入".into(),
                            result: None,
                        },
                    );
                    break;
                }
                Err(error) => {
                    failed_count += 1;
                    let _ = app.emit(
                        "book-import-progress",
                        ImportProgressEvent {
                            import_id: event_import_id.clone(),
                            file_name,
                            file_index,
                            total_files,
                            stage: "failed".into(),
                            message: error.to_string(),
                            result: None,
                        },
                    );
                }
            }
        }

        let _ = app.emit(
            "book-import-finished",
            ImportFinishedEvent {
                import_id: event_import_id.clone(),
                imported_count,
                duplicate_count,
                failed_count,
                cancelled: batch_cancelled,
            },
        );
        if let Ok(mut active) = manager.active.lock() {
            active.remove(&event_import_id);
        }
    });

    Ok(import_id)
}

#[tauri::command]
pub fn cancel_import(
    import_id: String,
    manager: State<'_, Arc<ImportManager>>,
) -> Result<(), String> {
    let active = manager
        .active
        .lock()
        .map_err(|_| "导入任务状态不可用。".to_string())?;
    if let Some(cancelled) = active.get(&import_id) {
        cancelled.store(true, Ordering::Relaxed);
    }
    Ok(())
}

fn display_error(error: DbError) -> String {
    error.to_string()
}
