mod commands;
use std::sync::Arc;

pub mod ai;
pub mod ai_context;
mod ai_service;
pub mod db;
pub mod discovery;
pub mod importer;
pub mod models;
pub mod parser;
pub mod preferences;
pub mod quotes;
mod siwc;
mod siwc_store;
use tauri::Manager;

fn repair_book_storage(database: &db::Database, app_data: &std::path::Path) {
    let books_dir = app_data.join("books");
    if let Ok(pending) = database.pending_deletions() {
        for (book_id, stored_path) in pending {
            let Ok(id) = uuid::Uuid::parse_str(&book_id) else {
                continue;
            };
            let expected = books_dir.join(format!("{id}.txt"));
            let stored = std::path::PathBuf::from(stored_path);
            if stored.parent() != Some(books_dir.as_path())
                || stored.file_name() != expected.file_name()
            {
                continue;
            }
            match std::fs::remove_file(&expected) {
                Ok(()) => {
                    let _ = database.finish_delete_book(&book_id);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let _ = database.finish_delete_book(&book_id);
                }
                Err(_) => {}
            }
        }
    }

    let referenced = database
        .referenced_book_files()
        .unwrap_or_default()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    if let Ok(entries) = std::fs::read_dir(&books_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_temporary = path.extension().is_some_and(|extension| extension == "tmp");
            let is_managed_book = path.extension().is_some_and(|extension| extension == "txt")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| uuid::Uuid::parse_str(stem).ok())
                    .is_some();
            if is_temporary
                || (is_managed_book && !referenced.contains(&path.to_string_lossy().to_string()))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            std::fs::create_dir_all(app_data.join("books"))?;
            let database = db::Database::open(&app_data.join("shuyu.sqlite3"))?;
            repair_book_storage(&database, &app_data);
            app.manage(Arc::new(database));
            app.manage(Arc::new(commands::ImportManager::default()));
            app.manage(Arc::new(ai_service::AiRequestManager::default()));
            app.manage(Arc::new(siwc::ChatGptAuthManager::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_preferences,
            commands::save_preferences,
            commands::get_library_count,
            commands::list_books,
            commands::get_book,
            commands::list_chapters,
            commands::get_chapter_content,
            commands::touch_book,
            commands::get_reading_position,
            commands::save_reading_position,
            commands::add_bookmark,
            commands::list_bookmarks,
            commands::delete_bookmark,
            commands::add_highlight,
            commands::list_highlights,
            commands::delete_highlight,
            commands::record_paragraph_view,
            commands::start_reading_session,
            commands::end_reading_session,
            commands::get_daily_quote,
            commands::select_ai_daily_quote,
            commands::update_book_metadata,
            commands::delete_book,
            commands::start_import,
            commands::cancel_import,
            ai_service::get_chatgpt_ai_settings,
            ai_service::save_chatgpt_ai_settings,
            ai_service::recommend_books,
            ai_service::start_chatgpt_ai_explanation,
            ai_service::cancel_ai_request,
            ai_service::get_chatgpt_models,
            siwc::get_chatgpt_connection_status,
            siwc::connect_chatgpt,
            siwc::cancel_chatgpt_sign_in,
            siwc::disconnect_chatgpt,
            siwc::forget_chatgpt_registration,
            siwc::open_chatgpt_usage,
            siwc::open_wikipedia_article,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
