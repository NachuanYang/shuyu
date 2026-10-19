use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    db::{Database, DbError},
    models::ImportResult,
    parser::{self, ParseError},
};

const LARGE_FILE_CONFIRMATION_BYTES: u64 = 100 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("仅支持 .txt 文件。")]
    UnsupportedFile,
    #[error("读取文件失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("文件为空或只有空白内容。")]
    Empty,
    #[error("文件编码无法可靠识别，请选择 UTF-8、GBK/GB18030 或 UTF-16 后重试。")]
    InvalidEncoding,
    #[error("文件超过 512 MiB，当前版本无法安全导入。")]
    TooLarge,
    #[error("文件超过 100 MiB，请确认后再导入。")]
    LargeFileNeedsConfirmation,
    #[error("导入已取消。")]
    Cancelled,
    #[error("本地资料库写入失败：{0}")]
    Database(String),
}

impl From<ParseError> for ImportError {
    fn from(error: ParseError) -> Self {
        match error {
            ParseError::Empty => Self::Empty,
            ParseError::InvalidEncoding => Self::InvalidEncoding,
            ParseError::Cancelled => Self::Cancelled,
        }
    }
}

fn database_error(error: DbError) -> ImportError {
    ImportError::Database(error.to_string())
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), ImportError> {
    if cancelled.load(Ordering::Relaxed) {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn extract_author_from_header(text: &str) -> Option<String> {
    for line in text.lines().take(80) {
        let line = line.trim().trim_start_matches('\u{feff}').trim();
        let Some(value) = line
            .strip_prefix("作者：")
            .or_else(|| line.strip_prefix("作者:"))
            .or_else(|| {
                line.get(..7)
                    .filter(|prefix| prefix.eq_ignore_ascii_case("author:"))
                    .map(|_| &line[7..])
            })
        else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() && value.chars().count() <= 120 {
            return Some(value.to_owned());
        }
    }
    None
}

pub fn repair_missing_authors(database: &Database, books_directory: &Path) -> Result<(), DbError> {
    for (book_id, stored_path, expected_hash) in database.books_pending_author_extraction()? {
        let Ok(id) = Uuid::parse_str(&book_id) else {
            continue;
        };
        let expected_path = books_directory.join(format!("{id}.txt"));
        if Path::new(&stored_path) != expected_path {
            database.finish_author_extraction(&book_id, None)?;
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&expected_path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            database.finish_author_extraction(&book_id, None)?;
            continue;
        }
        let (Ok(canonical_directory), Ok(canonical_file)) = (
            fs::canonicalize(books_directory),
            fs::canonicalize(&expected_path),
        ) else {
            continue;
        };
        if canonical_file.parent() != Some(canonical_directory.as_path()) {
            database.finish_author_extraction(&book_id, None)?;
            continue;
        }
        let Ok(bytes) = fs::read(&canonical_file) else {
            continue;
        };
        if hash(&bytes) != expected_hash {
            database.finish_author_extraction(&book_id, None)?;
            continue;
        }
        let author = parser::decode_txt(&bytes, None)
            .ok()
            .and_then(|text| extract_author_from_header(&text));
        database.finish_author_extraction(&book_id, author.as_deref())?;
    }
    Ok(())
}

pub fn import_txt_file(
    database: &Database,
    books_directory: &Path,
    source_path: &Path,
    encoding: Option<&str>,
    confirmed_large_file: bool,
    cancelled: &AtomicBool,
    mut on_stage: impl FnMut(&str),
) -> Result<ImportResult, ImportError> {
    if !source_path
        .extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("txt"))
    {
        return Err(ImportError::UnsupportedFile);
    }
    check_cancelled(cancelled)?;
    on_stage("reading");
    let metadata = fs::metadata(source_path)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    if metadata.len() > LARGE_FILE_CONFIRMATION_BYTES && !confirmed_large_file {
        return Err(ImportError::LargeFileNeedsConfirmation);
    }
    let bytes = fs::read(source_path)?;
    if bytes.is_empty() {
        return Err(ImportError::Empty);
    }
    check_cancelled(cancelled)?;
    let raw_hash = hash(&bytes);
    if let Some(existing_id) = database
        .find_book_id_by_hash(&raw_hash)
        .map_err(database_error)?
    {
        let title = database
            .list_books("recent")
            .map_err(database_error)?
            .into_iter()
            .find(|book| book.id == existing_id)
            .map(|book| book.title)
            .unwrap_or_else(|| "已在书架".into());
        return Ok(ImportResult {
            book_id: existing_id,
            title,
            chapter_count: 0,
            total_utf16: 0,
            status: "duplicate".into(),
        });
    }

    on_stage("decoding");
    let normalized_text = parser::decode_txt(&bytes, encoding)?;
    check_cancelled(cancelled)?;
    on_stage("parsing");
    let id = Uuid::new_v4().to_string();
    let parsed = parser::parse_book_cancellable(&id, &normalized_text, Some(cancelled))?;
    check_cancelled(cancelled)?;

    let import_name = source_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名.txt".into());
    let title = source_path
        .file_stem()
        .map(|name| name.to_string_lossy().trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "未命名".into());
    let file_path = books_directory.join(format!("{id}.txt"));
    let temporary_path = books_directory.join(format!(".{id}.tmp"));
    fs::create_dir_all(books_directory)?;

    on_stage("saving");
    let write_result = write_managed_copy(&temporary_path, &file_path, &bytes, cancelled);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    let author = extract_author_from_header(&normalized_text).unwrap_or_else(|| "未知作者".into());
    let inserted = database
        .insert_imported_book(
            &id,
            &raw_hash,
            &title,
            &author,
            &import_name,
            &file_path.to_string_lossy(),
            &parsed,
        )
        .map_err(database_error);
    match inserted {
        Ok(true) => Ok(ImportResult {
            book_id: id,
            title,
            chapter_count: parsed.chapters.len(),
            total_utf16: parsed.total_utf16,
            status: "imported".into(),
        }),
        Ok(false) => {
            let _ = fs::remove_file(&file_path);
            let existing = database
                .find_book_id_by_hash(&raw_hash)
                .map_err(database_error)?
                .unwrap_or_default();
            let existing_title = database
                .list_books("recent")
                .map_err(database_error)?
                .into_iter()
                .find(|book| book.id == existing)
                .map(|book| book.title)
                .unwrap_or(title);
            Ok(ImportResult {
                book_id: existing,
                title: existing_title,
                chapter_count: 0,
                total_utf16: 0,
                status: "duplicate".into(),
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&file_path);
            Err(error)
        }
    }
}

pub fn repair_legacy_single_chapter_index(
    database: &Database,
    books_directory: &Path,
    book_id: &str,
) -> Result<bool, DbError> {
    let Some((stored_path, expected_hash, expected_normalized_hash)) =
        database.legacy_single_chapter_book_source(book_id)?
    else {
        return Ok(false);
    };
    let Ok(id) = Uuid::parse_str(book_id) else {
        return Ok(false);
    };
    let expected_path = books_directory.join(format!("{id}.txt"));
    if Path::new(&stored_path) != expected_path {
        return Ok(false);
    }
    let Ok(bytes) = fs::read(&expected_path) else {
        return Ok(false);
    };
    if hash(&bytes) != expected_hash {
        return Ok(false);
    }
    let Ok(text) = parser::decode_txt(&bytes, None) else {
        return Ok(false);
    };
    let Ok(parsed) = parser::parse_book(book_id, &text) else {
        return Ok(false);
    };
    if parsed.normalized_sha256 != expected_normalized_hash {
        return Ok(false);
    }
    if !parsed
        .chapters
        .iter()
        .any(|chapter| !chapter.paragraphs.is_empty())
    {
        return Ok(false);
    }
    database.repair_legacy_chapter_structure(book_id, &parsed)
}

fn write_managed_copy(
    temporary_path: &PathBuf,
    final_path: &PathBuf,
    bytes: &[u8],
    cancelled: &AtomicBool,
) -> Result<(), ImportError> {
    let mut temporary = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_path)?;
    for chunk in bytes.chunks(1024 * 1024) {
        check_cancelled(cancelled)?;
        temporary.write_all(chunk)?;
    }
    temporary.sync_all()?;
    drop(temporary);
    fs::rename(temporary_path, final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::atomic::AtomicBool};

    use sha2::{Digest, Sha256};

    use super::import_txt_file;
    use crate::db::Database;

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn imports_a_read_only_copy_and_deduplicates_identical_content() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("故事.txt");
        let books = directory.path().join("app-data/books");
        let database = Database::open(&directory.path().join("app-data/shuyu.sqlite3")).unwrap();
        let content = "第一章 开始\n\n风吹过窗边。\n\n第二章 后来\n\n故事继续。";
        fs::write(&source, content).unwrap();
        let source_hash_before = sha(&fs::read(&source).unwrap());

        let first = import_txt_file(
            &database,
            &books,
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(first.status, "imported");
        assert_eq!(first.chapter_count, 2);
        assert_eq!(sha(&fs::read(&source).unwrap()), source_hash_before);
        assert_eq!(
            sha(&fs::read(books.join(format!("{}.txt", first.book_id))).unwrap()),
            source_hash_before
        );

        let second = import_txt_file(
            &database,
            &books,
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(second.status, "duplicate");
        assert_eq!(second.book_id, first.book_id);
        assert_eq!(database.library_count().unwrap(), 1);
        assert_eq!(database.list_books("recent").unwrap().len(), 1);
    }

    #[test]
    fn reads_author_from_txt_header_during_import() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("故事.txt");
        let books = directory.path().join("app-data/books");
        let database = Database::open(&directory.path().join("app-data/shuyu.sqlite3")).unwrap();
        fs::write(
            &source,
            "《故事》\n作者：超级大坦克科比\n简介：一段简介。\n\n第一章 开始\n\n正文。",
        )
        .unwrap();

        let imported = import_txt_file(
            &database,
            &books,
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(
            database
                .get_book(&imported.book_id)
                .unwrap()
                .unwrap()
                .author,
            "超级大坦克科比"
        );
    }

    #[test]
    fn extracts_only_a_short_author_field_from_the_book_header() {
        assert_eq!(
            super::extract_author_from_header("标题\n作者: 甲乙\n简介：简介"),
            Some("甲乙".into())
        );
        assert_eq!(
            super::extract_author_from_header("正文里提到了作者：甲乙"),
            None
        );
        assert_eq!(
            super::extract_author_from_header(&format!("作者：{}", "甲".repeat(121))),
            None
        );
    }

    #[test]
    fn upgrades_and_repairs_author_metadata_for_an_existing_managed_book() {
        let directory = tempfile::tempdir().unwrap();
        let books = directory.path().join("app-data/books");
        fs::create_dir_all(&books).unwrap();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let source = "《故事》\n作者：旧书作者\n简介：一段简介。\n\n第一章 开始\n\n正文。";
        let source_path = books.join(format!("{id}.txt"));
        fs::write(&source_path, source).unwrap();
        let path = directory.path().join("app-data/shuyu.sqlite3");
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch(include_str!("../migrations/001_initial.sql"))
                .unwrap();
            connection
                .execute_batch(include_str!("../migrations/002_book_discovery_cache.sql"))
                .unwrap();
            connection
                .execute_batch(include_str!("../migrations/003_ai_daily_quotes.sql"))
                .unwrap();
            connection
                .execute_batch(include_str!("../migrations/004_chatgpt_plan_ai.sql"))
                .unwrap();
            connection.execute(
                "INSERT INTO books(id,sha256,normalized_sha256,title,author,import_name,file_path,total_utf16,chapter_count,created_at) VALUES(?1,?2,'normalized','故事','未知作者','故事.txt',?3,30,1,'2026-10-09T00:00:00Z')",
                rusqlite::params![id, sha(source.as_bytes()), source_path.to_string_lossy()],
            ).unwrap();
            connection.pragma_update(None, "user_version", 4).unwrap();
        }

        let database = Database::open(&path).unwrap();
        super::repair_missing_authors(&database, &books).unwrap();

        assert_eq!(database.schema_version().unwrap(), 5);
        assert_eq!(database.get_book(id).unwrap().unwrap().author, "旧书作者");
        assert!(database
            .books_pending_author_extraction()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_encoding_does_not_leave_a_book_or_managed_copy() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("broken.txt");
        let books = directory.path().join("books");
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();
        fs::write(&source, [0xFF, 0xFE, 0x00]).unwrap();

        assert!(import_txt_file(
            &database,
            &books,
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        assert_eq!(database.library_count().unwrap(), 0);
        assert!(!books.exists());
    }
}
