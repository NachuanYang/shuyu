use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

use crate::{
    models::{
        AiQuestionContext, AiQuestionParagraph, BookSummary, Bookmark, Chapter, ChapterContent,
        DailyQuote, DailyQuoteCandidate, Highlight, Paragraph, QuoteSourceParagraph, ReadingAnchor,
    },
    parser::ParsedBook,
};

struct NewParagraphLocation<'a> {
    chapter_id: &'a str,
    paragraph: &'a crate::parser::ParsedParagraph,
}

#[derive(Clone)]
struct MappedAnchor {
    chapter_id: String,
    paragraph_id: String,
    intra_utf16: usize,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database lock was poisoned")]
    LockPoisoned,
}

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let current_version: i32 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current_version < 1 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(include_str!("../migrations/001_initial.sql"))?;
            transaction.pragma_update(None, "user_version", 1)?;
            transaction.commit()?;
        }
        if current_version < 2 {
            let transaction = connection.transaction()?;
            transaction
                .execute_batch(include_str!("../migrations/002_book_discovery_cache.sql"))?;
            transaction.pragma_update(None, "user_version", 2)?;
            transaction.commit()?;
        }
        if current_version < 3 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(include_str!("../migrations/003_ai_daily_quotes.sql"))?;
            transaction.pragma_update(None, "user_version", 3)?;
            transaction.commit()?;
        }
        if current_version < 4 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(include_str!("../migrations/004_chatgpt_plan_ai.sql"))?;
            transaction.pragma_update(None, "user_version", 4)?;
            transaction.commit()?;
        }
        if current_version < 5 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(include_str!("../migrations/005_author_metadata.sql"))?;
            transaction.pragma_update(None, "user_version", 5)?;
            transaction.commit()?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn schema_version(&self) -> Result<i32, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn has_table(&self, table: &str) -> Result<bool, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?)
    }

    pub fn preference(&self, key: &str) -> Result<Option<String>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection
            .query_row(
                "SELECT json_value FROM preferences WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn save_preference(&self, key: &str, value: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute(
            "INSERT INTO preferences(key,json_value,updated_at) VALUES(?1,?2,datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value, updated_at=excluded.updated_at",
            [key, value],
        )?;
        Ok(())
    }

    pub fn discovery_search_cache(
        &self,
        query_hash: &str,
    ) -> Result<Option<(String, String)>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection
            .query_row(
                "SELECT result_json,fetched_at FROM discovery_search_cache WHERE query_hash=?1",
                [query_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn save_discovery_search_cache(
        &self,
        query_hash: &str,
        query: &str,
        result_json: &str,
        fetched_at: &str,
    ) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute(
            "INSERT INTO discovery_search_cache(query_hash,query,result_json,fetched_at) VALUES(?1,?2,?3,?4) \
             ON CONFLICT(query_hash) DO UPDATE SET query=excluded.query,result_json=excluded.result_json,fetched_at=excluded.fetched_at",
            rusqlite::params![query_hash, query, result_json, fetched_at],
        )?;
        connection.execute(
            "DELETE FROM discovery_search_cache WHERE fetched_at < datetime('now','-90 days')",
            [],
        )?;
        Ok(())
    }

    pub fn library_count(&self) -> Result<u64, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(
            connection.query_row("SELECT COUNT(*) FROM books WHERE is_deleted=0", [], |row| {
                row.get(0)
            })?,
        )
    }

    pub fn find_book_id_by_hash(&self, sha256: &str) -> Result<Option<String>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection
            .query_row(
                "SELECT id FROM books WHERE sha256=?1 AND is_deleted=0",
                [sha256],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn legacy_single_chapter_book_source(
        &self,
        book_id: &str,
    ) -> Result<Option<(String, String, String)>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection
            .query_row(
                "SELECT file_path,sha256,normalized_sha256 FROM books WHERE id=?1 AND is_deleted=0 AND chapter_count=1 AND parser_version<2",
                [book_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    pub fn repair_legacy_chapter_structure(
        &self,
        book_id: &str,
        parsed: &ParsedBook,
    ) -> Result<bool, DbError> {
        let mut connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let legacy: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT parser_version,chapter_count FROM books WHERE id=?1 AND is_deleted=0",
                [book_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((parser_version, chapter_count)) = legacy else {
            transaction.rollback()?;
            return Ok(false);
        };
        if parser_version >= 2 || chapter_count != 1 {
            transaction.rollback()?;
            return Ok(false);
        }
        if parsed.chapters.len() <= 1 {
            transaction.execute("UPDATE books SET parser_version=2 WHERE id=?1", [book_id])?;
            transaction.commit()?;
            return Ok(false);
        }
        let old_chapter_ids = {
            let mut statement = transaction.prepare("SELECT id FROM chapters WHERE book_id=?1")?;
            let rows = statement.query_map([book_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        transaction.execute(
            "UPDATE chapters SET ordinal=ordinal+1000000 WHERE book_id=?1",
            [book_id],
        )?;
        transaction.execute(
            "UPDATE paragraphs SET ordinal=ordinal+1000000 WHERE book_id=?1",
            [book_id],
        )?;

        let mut old_paragraphs = std::collections::HashMap::new();
        {
            let mut statement = transaction.prepare(
                "SELECT id,start_utf16,end_utf16 FROM paragraphs WHERE book_id=?1 ORDER BY ordinal",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as usize,
                    row.get::<_, i64>(2)?.max(0) as usize,
                ))
            })?;
            for row in rows {
                let (id, start, end) = row?;
                old_paragraphs.insert(id, (start, end));
            }
        }

        for chapter in &parsed.chapters {
            transaction.execute(
                "INSERT INTO chapters(id,book_id,ordinal,title,start_utf16,end_utf16) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![chapter.id, book_id, chapter.ordinal as i64, chapter.title, chapter.start_utf16 as i64, chapter.end_utf16 as i64],
            )?;
            for paragraph in &chapter.paragraphs {
                transaction.execute(
                    "INSERT INTO paragraphs(id,book_id,chapter_id,ordinal,text,start_utf16,end_utf16) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![paragraph.id, book_id, chapter.id, paragraph.ordinal as i64, paragraph.text, paragraph.start_utf16 as i64, paragraph.end_utf16 as i64],
                )?;
            }
        }
        let new_paragraphs = parsed
            .chapters
            .iter()
            .flat_map(|chapter| {
                chapter
                    .paragraphs
                    .iter()
                    .map(move |paragraph| NewParagraphLocation {
                        chapter_id: &chapter.id,
                        paragraph,
                    })
            })
            .collect::<Vec<_>>();

        let position: Option<usize> = transaction
            .query_row(
                "SELECT absolute_utf16 FROM reading_positions WHERE book_id=?1",
                [book_id],
                |row| Ok(row.get::<_, i64>(0)?.max(0) as usize),
            )
            .optional()?;
        if let Some(absolute) = position {
            if let Some(anchor) = map_absolute_offset(&new_paragraphs, absolute) {
                transaction.execute(
                    "UPDATE reading_positions SET chapter_id=?2,paragraph_id=?3,intra_utf16=?4 WHERE book_id=?1",
                    rusqlite::params![book_id, anchor.chapter_id, anchor.paragraph_id, anchor.intra_utf16 as i64],
                )?;
            }
        }

        let bookmarks = {
            let mut statement = transaction.prepare(
                "SELECT bm.id,bm.paragraph_id,bm.intra_utf16 FROM bookmarks bm WHERE bm.book_id=?1",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?.max(0) as usize,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (bookmark_id, paragraph_id, intra) in bookmarks {
            if let Some((start, _)) = old_paragraphs.get(&paragraph_id) {
                if let Some(anchor) = map_absolute_offset(&new_paragraphs, start + intra) {
                    transaction.execute(
                        "UPDATE bookmarks SET paragraph_id=?2,intra_utf16=?3 WHERE id=?1",
                        rusqlite::params![
                            bookmark_id,
                            anchor.paragraph_id,
                            anchor.intra_utf16 as i64
                        ],
                    )?;
                }
            }
        }

        let highlights = {
            let mut statement = transaction.prepare(
                "SELECT id,start_paragraph_id,start_intra_utf16,end_paragraph_id,end_intra_utf16 FROM highlights WHERE book_id=?1",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?.max(0) as usize,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?.max(0) as usize,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (id, start_id, start_intra, end_id, end_intra) in highlights {
            let start = old_paragraphs
                .get(&start_id)
                .and_then(|range| map_absolute_offset(&new_paragraphs, range.0 + start_intra));
            let end = old_paragraphs
                .get(&end_id)
                .and_then(|range| map_absolute_offset(&new_paragraphs, range.0 + end_intra));
            if let (Some(start), Some(end)) = (start, end) {
                transaction.execute(
                    "UPDATE highlights SET start_paragraph_id=?2,start_intra_utf16=?3,end_paragraph_id=?4,end_intra_utf16=?5 WHERE id=?1",
                    rusqlite::params![id, start.paragraph_id, start.intra_utf16 as i64, end.paragraph_id, end.intra_utf16 as i64],
                )?;
            }
        }

        let views = {
            let mut statement = transaction.prepare(
                "SELECT id,paragraph_id,viewed_at,dwell_ms FROM paragraph_views WHERE book_id=?1",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?.max(0) as u64,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (view_id, paragraph_id, viewed_at, dwell_ms) in views {
            let Some((old_start, old_end)) = old_paragraphs.get(&paragraph_id) else {
                continue;
            };
            let mut matches = new_paragraphs
                .iter()
                .filter(|location| {
                    location.paragraph.start_utf16 < *old_end
                        && location.paragraph.end_utf16 > *old_start
                })
                .collect::<Vec<_>>();
            if matches.is_empty() {
                if let Some(anchor) = map_absolute_offset(&new_paragraphs, *old_start) {
                    transaction.execute(
                        "UPDATE paragraph_views SET paragraph_id=?2 WHERE id=?1",
                        rusqlite::params![view_id, anchor.paragraph_id],
                    )?;
                }
                continue;
            }
            let piece_count = matches.len() as u64;
            let base_dwell = dwell_ms / piece_count;
            let extra_dwell = dwell_ms % piece_count;
            if let Some(first) = matches.first() {
                transaction.execute(
                    "UPDATE paragraph_views SET paragraph_id=?2,dwell_ms=?3 WHERE id=?1",
                    rusqlite::params![view_id, first.paragraph.id, base_dwell + extra_dwell],
                )?;
            }
            for location in matches.drain(1..) {
                transaction.execute(
                    "INSERT INTO paragraph_views(id,book_id,paragraph_id,viewed_at,dwell_ms) VALUES(?1,?2,?3,?4,?5)",
                    rusqlite::params![uuid::Uuid::new_v4().to_string(), book_id, location.paragraph.id, viewed_at, base_dwell],
                )?;
            }
        }

        let quotes = {
            let mut statement = transaction.prepare(
                "SELECT local_date,paragraph_id,start_intra_utf16,end_intra_utf16,text_snapshot FROM daily_quotes WHERE book_id=?1",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?.max(0) as usize,
                    row.get::<_, i64>(3)?.max(0) as usize,
                    row.get::<_, String>(4)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (local_date, paragraph_id, start_intra, end_intra, snapshot) in quotes {
            let mapped = old_paragraphs
                .get(&paragraph_id)
                .and_then(|(old_start, _)| {
                    map_source_range(
                        &new_paragraphs,
                        old_start + start_intra,
                        old_start + end_intra,
                        &snapshot,
                    )
                });
            if let Some((anchor, end_intra)) = mapped {
                transaction.execute(
                    "UPDATE daily_quotes SET chapter_id=?2,paragraph_id=?3,start_intra_utf16=?4,end_intra_utf16=?5 WHERE local_date=?1",
                    rusqlite::params![local_date, anchor.chapter_id, anchor.paragraph_id, anchor.intra_utf16 as i64, end_intra as i64],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM daily_quotes WHERE local_date=?1",
                    [&local_date],
                )?;
            }
        }

        let sessions = {
            let mut statement = transaction.prepare(
                "SELECT id,first_anchor,last_anchor FROM reading_sessions WHERE book_id=?1",
            )?;
            let rows = statement.query_map([book_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (session_id, first, last) in sessions {
            let first = first.and_then(|anchor| remap_session_anchor(&anchor, &new_paragraphs));
            let last = last.and_then(|anchor| remap_session_anchor(&anchor, &new_paragraphs));
            transaction.execute(
                "UPDATE reading_sessions SET first_anchor=?2,last_anchor=?3 WHERE id=?1",
                rusqlite::params![session_id, first, last],
            )?;
        }

        for chapter_id in old_chapter_ids {
            transaction.execute(
                "DELETE FROM chapters WHERE id=?1 AND book_id=?2",
                rusqlite::params![chapter_id, book_id],
            )?;
        }
        transaction.execute(
            "UPDATE books SET normalized_sha256=?2,total_utf16=?3,chapter_count=?4,parser_version=2 WHERE id=?1",
            rusqlite::params![book_id, parsed.normalized_sha256, parsed.total_utf16 as i64, parsed.chapters.len() as i64],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn insert_imported_book(
        &self,
        id: &str,
        sha256: &str,
        title: &str,
        author: &str,
        import_name: &str,
        file_path: &str,
        parsed: &ParsedBook,
    ) -> Result<bool, DbError> {
        let mut connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE sha256=?1 AND is_deleted=0)",
            [sha256],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(false);
        }

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        transaction.execute(
            "INSERT INTO books(id,sha256,normalized_sha256,title,author,import_name,file_path,total_utf16,chapter_count,created_at,parser_version,author_metadata_checked) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,2,1)",
            rusqlite::params![id, sha256, parsed.normalized_sha256, title, author, import_name, file_path, parsed.total_utf16 as i64, parsed.chapters.len() as i64, now],
        )?;
        for chapter in &parsed.chapters {
            transaction.execute(
                "INSERT INTO chapters(id,book_id,ordinal,title,start_utf16,end_utf16) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![chapter.id, id, chapter.ordinal as i64, chapter.title, chapter.start_utf16 as i64, chapter.end_utf16 as i64],
            )?;
            for paragraph in &chapter.paragraphs {
                transaction.execute(
                    "INSERT INTO paragraphs(id,book_id,chapter_id,ordinal,text,start_utf16,end_utf16) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![paragraph.id, id, chapter.id, paragraph.ordinal as i64, paragraph.text, paragraph.start_utf16 as i64, paragraph.end_utf16 as i64],
                )?;
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn list_books(&self, sort: &str) -> Result<Vec<BookSummary>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let order_by = match sort {
            "title" => "b.title COLLATE NOCASE ASC,b.created_at DESC",
            "created" => "b.created_at DESC",
            _ => "b.last_opened_at DESC,b.created_at DESC",
        };
        let sql = format!(
            "SELECT b.id,b.title,b.author,b.import_name,b.chapter_count,b.total_utf16,b.created_at,b.last_opened_at, \
             COALESCE(p.progress_ratio,0) \
             FROM books b LEFT JOIN reading_positions p ON p.book_id=b.id \
             WHERE b.is_deleted=0 ORDER BY {order_by}"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            Ok(BookSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                import_name: row.get(3)?,
                chapter_count: row.get::<_, i64>(4)?.max(0) as usize,
                total_utf16: row.get::<_, i64>(5)?.max(0) as usize,
                created_at: row.get(6)?,
                last_opened_at: row.get(7)?,
                progress_ratio: row.get::<_, f64>(8)?.clamp(0.0, 1.0),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_book_metadata(
        &self,
        id: &str,
        title: &str,
        author: &str,
    ) -> Result<bool, DbError> {
        let title = title.trim();
        let author = author.trim();
        if title.is_empty() || title.chars().count() > 160 || author.chars().count() > 120 {
            return Ok(false);
        }
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection.execute(
            "UPDATE books SET title=?2,author=?3,author_metadata_checked=1 WHERE id=?1 AND is_deleted=0",
            rusqlite::params![
                id,
                title,
                if author.is_empty() {
                    "未知作者"
                } else {
                    author
                }
            ],
        )? == 1)
    }

    pub fn books_pending_author_extraction(
        &self,
    ) -> Result<Vec<(String, String, String)>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement = connection.prepare(
            "SELECT id,file_path,sha256 FROM books WHERE is_deleted=0 AND author='未知作者' AND author_metadata_checked=0",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn finish_author_extraction(
        &self,
        id: &str,
        extracted_author: Option<&str>,
    ) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute(
            "UPDATE books SET author=CASE WHEN author='未知作者' AND ?2 IS NOT NULL AND length(?2)>0 THEN ?2 ELSE author END,author_metadata_checked=1 WHERE id=?1 AND is_deleted=0",
            rusqlite::params![id, extracted_author],
        )?;
        Ok(())
    }

    pub fn book_file_path(&self, id: &str) -> Result<Option<String>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection
            .query_row(
                "SELECT file_path FROM books WHERE id=?1 AND is_deleted=0",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn begin_delete_book(&self, id: &str) -> Result<Option<String>, DbError> {
        let mut connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let file_path: Option<String> = transaction
            .query_row(
                "SELECT file_path FROM books WHERE id=?1 AND is_deleted=0",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if file_path.is_some() {
            transaction.execute("UPDATE books SET is_deleted=1 WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(file_path)
    }

    pub fn finish_delete_book(&self, id: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute("DELETE FROM books WHERE id=?1 AND is_deleted=1", [id])?;
        Ok(())
    }

    pub fn restore_book_after_delete_failure(&self, id: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute("UPDATE books SET is_deleted=0 WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn pending_deletions(&self) -> Result<Vec<(String, String)>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement =
            connection.prepare("SELECT id,file_path FROM books WHERE is_deleted=1")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn referenced_book_files(&self) -> Result<Vec<String>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement = connection.prepare("SELECT file_path FROM books WHERE is_deleted=0")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_book(&self, id: &str) -> Result<Option<BookSummary>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection.query_row(
            "SELECT b.id,b.title,b.author,b.import_name,b.chapter_count,b.total_utf16,b.created_at,b.last_opened_at,COALESCE(p.progress_ratio,0) \
             FROM books b LEFT JOIN reading_positions p ON p.book_id=b.id WHERE b.id=?1 AND b.is_deleted=0",
            [id],
            |row| Ok(BookSummary { id: row.get(0)?, title: row.get(1)?, author: row.get(2)?, import_name: row.get(3)?, chapter_count: row.get::<_, i64>(4)?.max(0) as usize, total_utf16: row.get::<_, i64>(5)?.max(0) as usize, created_at: row.get(6)?, last_opened_at: row.get(7)?, progress_ratio: row.get::<_, f64>(8)?.clamp(0.0, 1.0) }),
        ).optional()?)
    }

    pub fn list_chapters(&self, book_id: &str) -> Result<Vec<Chapter>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement = connection.prepare("SELECT c.id,c.book_id,c.ordinal,c.title,c.start_utf16,c.end_utf16 FROM chapters c JOIN books b ON b.id=c.book_id WHERE c.book_id=?1 AND b.is_deleted=0 ORDER BY c.ordinal")?;
        let rows = statement.query_map([book_id], |row| {
            Ok(Chapter {
                id: row.get(0)?,
                book_id: row.get(1)?,
                ordinal: row.get::<_, i64>(2)?.max(0) as usize,
                title: row.get(3)?,
                start_utf16: row.get::<_, i64>(4)?.max(0) as usize,
                end_utf16: row.get::<_, i64>(5)?.max(0) as usize,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn chapter_content(&self, chapter_id: &str) -> Result<Option<ChapterContent>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let chapter = connection.query_row("SELECT c.id,c.book_id,c.ordinal,c.title,c.start_utf16,c.end_utf16 FROM chapters c JOIN books b ON b.id=c.book_id WHERE c.id=?1 AND b.is_deleted=0", [chapter_id], |row| Ok(Chapter { id: row.get(0)?, book_id: row.get(1)?, ordinal: row.get::<_, i64>(2)?.max(0) as usize, title: row.get(3)?, start_utf16: row.get::<_, i64>(4)?.max(0) as usize, end_utf16: row.get::<_, i64>(5)?.max(0) as usize })).optional()?;
        let Some(chapter) = chapter else {
            return Ok(None);
        };
        let mut statement = connection.prepare("SELECT id,book_id,chapter_id,ordinal,text,start_utf16,end_utf16 FROM paragraphs WHERE chapter_id=?1 ORDER BY ordinal")?;
        let rows = statement.query_map([chapter_id], |row| {
            Ok(Paragraph {
                id: row.get(0)?,
                book_id: row.get(1)?,
                chapter_id: row.get(2)?,
                ordinal: row.get::<_, i64>(3)?.max(0) as usize,
                text: row.get(4)?,
                start_utf16: row.get::<_, i64>(5)?.max(0) as usize,
                end_utf16: row.get::<_, i64>(6)?.max(0) as usize,
            })
        })?;
        let paragraphs = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(Some(ChapterContent {
            chapter,
            paragraphs,
        }))
    }

    pub fn touch_book(&self, id: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute(
            "UPDATE books SET last_opened_at=?2 WHERE id=?1 AND is_deleted=0",
            rusqlite::params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_reading_position(&self, book_id: &str) -> Result<Option<ReadingAnchor>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        Ok(connection.query_row(
            "SELECT rp.book_id,rp.chapter_id,rp.paragraph_id,rp.intra_utf16,rp.absolute_utf16,rp.nearby_text,b.parser_version \
             FROM reading_positions rp JOIN books b ON b.id=rp.book_id JOIN paragraphs p ON p.id=rp.paragraph_id AND p.book_id=rp.book_id AND p.chapter_id=rp.chapter_id \
             JOIN chapters c ON c.id=rp.chapter_id AND c.book_id=rp.book_id WHERE rp.book_id=?1 AND b.is_deleted=0",
            [book_id],
            |row| Ok(ReadingAnchor { book_id: row.get(0)?, chapter_id: row.get(1)?, paragraph_id: row.get(2)?, intra_paragraph_utf16: row.get::<_, i64>(3)?.max(0) as usize, absolute_utf16: row.get::<_, i64>(4)?.max(0) as usize, nearby_text: row.get(5)?, parser_version: row.get::<_, i64>(6)?.max(1) as usize }),
        ).optional()?)
    }

    pub fn read_ai_question_context(
        &self,
        book_id: &str,
        chapter_id: &str,
    ) -> Result<Option<AiQuestionContext>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let chapter = connection
            .query_row(
                "SELECT c.title,c.ordinal,rp.absolute_utf16,rp.chapter_id,rp.intra_utf16,p.start_utf16,p.end_utf16,p.text \
                 FROM chapters c JOIN books b ON b.id=c.book_id \
                 JOIN reading_positions rp ON rp.book_id=b.id \
                 JOIN paragraphs p ON p.id=rp.paragraph_id AND p.book_id=rp.book_id AND p.chapter_id=rp.chapter_id \
                 WHERE c.id=?1 AND c.book_id=?2 AND b.is_deleted=0",
                rusqlite::params![chapter_id, book_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            chapter_title,
            chapter_ordinal,
            reading_position_utf16,
            position_chapter_id,
            intra_utf16,
            anchor_paragraph_start,
            anchor_paragraph_end,
            anchor_paragraph_text,
        )) = chapter
        else {
            return Ok(None);
        };
        let reading_position_utf16 = reading_position_utf16.max(0) as usize;
        let intra_utf16 = intra_utf16.max(0) as usize;
        let anchor_paragraph_start = anchor_paragraph_start.max(0) as usize;
        let anchor_paragraph_end = anchor_paragraph_end.max(0) as usize;
        let validated_position =
            anchor_paragraph_start + clamp_utf16_boundary(&anchor_paragraph_text, intra_utf16);
        if position_chapter_id != chapter_id
            || intra_utf16 > anchor_paragraph_text.encode_utf16().count()
            || clamp_utf16_boundary(&anchor_paragraph_text, intra_utf16) != intra_utf16
            || reading_position_utf16 != validated_position
            || reading_position_utf16 > anchor_paragraph_end
        {
            return Ok(None);
        }
        let chapter_ordinal = chapter_ordinal.max(0) as usize;
        let mut statement = connection.prepare(
            "SELECT c.id,c.ordinal,p.ordinal,p.start_utf16,p.end_utf16,p.text \
             FROM paragraphs p JOIN chapters c ON c.id=p.chapter_id AND c.book_id=p.book_id \
             JOIN books b ON b.id=p.book_id \
             WHERE p.book_id=?1 AND b.is_deleted=0 AND p.end_utf16<=?2 \
             ORDER BY p.start_utf16,p.ordinal",
        )?;
        let rows = statement.query_map(
            rusqlite::params![book_id, reading_position_utf16 as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?;
        let mut current_chapter = Vec::new();
        let mut earlier_book = Vec::new();
        for row in rows {
            let (source_chapter_id, source_chapter_ordinal, paragraph_ordinal, start, end, text) =
                row?;
            let source_chapter_ordinal = source_chapter_ordinal.max(0) as usize;
            let paragraph = AiQuestionParagraph {
                chapter_ordinal: source_chapter_ordinal,
                paragraph_ordinal: paragraph_ordinal.max(0) as usize,
                start_utf16: start.max(0) as usize,
                end_utf16: end.max(0) as usize,
                text,
            };
            if source_chapter_id == chapter_id {
                current_chapter.push(paragraph);
            } else if source_chapter_ordinal < chapter_ordinal {
                earlier_book.push(paragraph);
            }
        }
        Ok(Some(AiQuestionContext {
            chapter_id: chapter_id.to_owned(),
            chapter_title,
            chapter_ordinal,
            reading_position_utf16,
            current_chapter,
            earlier_book,
        }))
    }

    pub fn reconstruct_ai_question_selection(
        &self,
        book_id: &str,
        chapter_id: &str,
        start_paragraph_id: &str,
        start_utf16: usize,
        end_paragraph_id: &str,
        end_utf16: usize,
        expected_text: &str,
    ) -> Result<Option<String>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let endpoints = connection
            .query_row(
                "SELECT p1.ordinal,p1.start_utf16,p1.text,p2.ordinal,p2.start_utf16,p2.text,rp.absolute_utf16 \
                 FROM paragraphs p1 JOIN paragraphs p2 \
                   ON p2.book_id=p1.book_id AND p2.chapter_id=p1.chapter_id \
                 JOIN books b ON b.id=p1.book_id \
                 JOIN reading_positions rp ON rp.book_id=p1.book_id AND rp.chapter_id=p1.chapter_id \
                 WHERE p1.id=?1 AND p2.id=?2 AND p1.book_id=?3 \
                   AND p1.chapter_id=?4 AND p2.chapter_id=?4 AND b.is_deleted=0",
                rusqlite::params![start_paragraph_id, end_paragraph_id, book_id, chapter_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            start_ordinal,
            start_abs,
            start_text,
            end_ordinal,
            end_abs,
            end_text,
            reading_position_abs,
        )) = endpoints
        else {
            return Ok(None);
        };
        if end_ordinal < start_ordinal
            || start_utf16 > start_text.encode_utf16().count()
            || end_utf16 > end_text.encode_utf16().count()
        {
            return Ok(None);
        }
        let start_absolute = start_abs.max(0) as usize + start_utf16;
        let end_absolute = end_abs.max(0) as usize + end_utf16;
        if end_absolute <= start_absolute
            || end_absolute - start_absolute > 16_384
            || end_absolute > reading_position_abs.max(0) as usize
        {
            return Ok(None);
        }
        let mut statement = connection.prepare(
            "SELECT start_utf16,end_utf16,text FROM paragraphs \
             WHERE book_id=?1 AND chapter_id=?2 AND ordinal BETWEEN ?3 AND ?4 ORDER BY ordinal",
        )?;
        let rows = statement.query_map(
            rusqlite::params![book_id, chapter_id, start_ordinal, end_ordinal],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let paragraphs = rows.collect::<Result<Vec<_>, _>>()?;
        if paragraphs.len() != (end_ordinal - start_ordinal + 1) as usize {
            return Ok(None);
        }
        let mut reconstructed = String::new();
        let mut previous_end = 0usize;
        for (index, (paragraph_start, paragraph_end, text)) in paragraphs.iter().enumerate() {
            let paragraph_start = (*paragraph_start).max(0) as usize;
            let paragraph_end = (*paragraph_end).max(0) as usize;
            if index > 0 {
                let gap = paragraph_start.saturating_sub(previous_end);
                if gap > 16_384 || reconstructed.encode_utf16().count().saturating_add(gap) > 16_384
                {
                    return Ok(None);
                }
                reconstructed.extend(std::iter::repeat_n('\n', gap));
            }
            let from = if index == 0 { start_utf16 } else { 0 };
            let to = if index + 1 == paragraphs.len() {
                end_utf16
            } else {
                text.encode_utf16().count()
            };
            let Some(part) = utf16_slice(text, from, to) else {
                return Ok(None);
            };
            reconstructed.push_str(&part);
            if reconstructed.encode_utf16().count() > 16_384 {
                return Ok(None);
            }
            previous_end = paragraph_end;
        }
        if reconstructed.trim().is_empty() || reconstructed.trim() != expected_text.trim() {
            return Ok(None);
        }
        Ok(Some(reconstructed))
    }

    pub fn save_reading_position(&self, anchor: &ReadingAnchor) -> Result<(), DbError> {
        let mut connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let source = transaction.query_row(
            "SELECT p.start_utf16,p.end_utf16,p.text,b.total_utf16 FROM paragraphs p JOIN chapters c ON c.id=p.chapter_id JOIN books b ON b.id=p.book_id \
             WHERE p.id=?1 AND p.book_id=?2 AND c.id=?3 AND b.is_deleted=0",
            rusqlite::params![anchor.paragraph_id, anchor.book_id, anchor.chapter_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?)),
        ).optional()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let (start, _end, text, total) = source;
        let intra = clamp_utf16_boundary(&text, anchor.intra_paragraph_utf16);
        let absolute = start.max(0) as usize + intra;
        let nearby_text = text.chars().take(48).collect::<String>();
        let progress = if total > 0 {
            (absolute as f64 / total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let now = chrono::Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO reading_positions(book_id,chapter_id,paragraph_id,intra_utf16,absolute_utf16,nearby_text,progress_ratio,updated_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(book_id) DO UPDATE SET chapter_id=excluded.chapter_id,paragraph_id=excluded.paragraph_id, \
             intra_utf16=excluded.intra_utf16,absolute_utf16=excluded.absolute_utf16,nearby_text=excluded.nearby_text,progress_ratio=excluded.progress_ratio,updated_at=excluded.updated_at",
            rusqlite::params![anchor.book_id, anchor.chapter_id, anchor.paragraph_id, intra as i64, absolute as i64, nearby_text, progress, now],
        )?;
        transaction.execute(
            "UPDATE books SET last_opened_at=?2 WHERE id=?1",
            rusqlite::params![anchor.book_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_bookmark(
        &self,
        book_id: &str,
        paragraph_id: &str,
        intra: usize,
        note: Option<&str>,
    ) -> Result<bool, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let text: String = connection.query_row(
            "SELECT text FROM paragraphs WHERE id=?1 AND book_id=?2",
            rusqlite::params![paragraph_id, book_id],
            |row| row.get(0),
        )?;
        let offset = clamp_utf16_boundary(&text, intra);
        let inserted = connection.execute(
            "INSERT INTO bookmarks(id,book_id,paragraph_id,intra_utf16,created_at,note) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(book_id,paragraph_id,intra_utf16) DO NOTHING",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), book_id, paragraph_id, offset as i64, chrono::Utc::now().to_rfc3339(), note],
        )?;
        Ok(inserted == 1)
    }

    pub fn list_bookmarks(&self, book_id: Option<&str>) -> Result<Vec<Bookmark>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement = connection.prepare(
            "SELECT bm.id,bm.book_id,b.title,c.id,c.title,p.id,bm.intra_utf16,p.text,bm.created_at,bm.note \
             FROM bookmarks bm JOIN books b ON b.id=bm.book_id JOIN paragraphs p ON p.id=bm.paragraph_id JOIN chapters c ON c.id=p.chapter_id \
             WHERE b.is_deleted=0 AND (?1 IS NULL OR bm.book_id=?1) ORDER BY bm.created_at DESC LIMIT 500",
        )?;
        let rows = statement.query_map([book_id], |row| {
            let text: String = row.get(7)?;
            Ok(Bookmark {
                id: row.get(0)?,
                book_id: row.get(1)?,
                book_title: row.get(2)?,
                chapter_id: row.get(3)?,
                chapter_title: row.get(4)?,
                paragraph_id: row.get(5)?,
                intra_utf16: row.get::<_, i64>(6)?.max(0) as usize,
                text_preview: text.chars().take(96).collect(),
                created_at: row.get(8)?,
                note: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_bookmark(&self, id: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute("DELETE FROM bookmarks WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn add_highlight(
        &self,
        book_id: &str,
        start_paragraph_id: &str,
        start: usize,
        end_paragraph_id: &str,
        end: usize,
    ) -> Result<Option<Highlight>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let row = connection.query_row(
            "SELECT p1.ordinal,p1.start_utf16,p1.end_utf16,p1.text,p2.ordinal,p2.start_utf16,p2.end_utf16,p2.text,c.id,c.title,b.title \
             FROM paragraphs p1 JOIN paragraphs p2 ON p2.id=?2 AND p2.book_id=p1.book_id AND p2.chapter_id=p1.chapter_id \
             JOIN chapters c ON c.id=p1.chapter_id JOIN books b ON b.id=p1.book_id \
             WHERE p1.id=?1 AND p1.book_id=?3 AND b.is_deleted=0",
            rusqlite::params![start_paragraph_id, end_paragraph_id, book_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?, row.get::<_, i64>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?, row.get::<_, String>(9)?, row.get::<_, String>(10)?)),
        ).optional()?;
        let Some((
            start_ordinal,
            start_abs,
            _,
            start_text,
            end_ordinal,
            end_abs,
            _,
            end_text,
            chapter_id,
            chapter_title,
            book_title,
        )) = row
        else {
            return Ok(None);
        };
        if end_ordinal < start_ordinal {
            return Ok(None);
        }
        let start = clamp_utf16_boundary(&start_text, start);
        let end = clamp_utf16_boundary(&end_text, end);
        let absolute_start = start_abs.max(0) as usize + start;
        let absolute_end = end_abs.max(0) as usize + end;
        if absolute_end <= absolute_start || absolute_end - absolute_start > 16_384 {
            return Ok(None);
        }
        let mut statement = connection.prepare(
            "SELECT id,ordinal,start_utf16,end_utf16,text FROM paragraphs WHERE book_id=?1 AND chapter_id=?2 AND ordinal BETWEEN ?3 AND ?4 ORDER BY ordinal",
        )?;
        let rows = statement.query_map(
            rusqlite::params![book_id, chapter_id, start_ordinal, end_ordinal],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        let paragraphs = rows.collect::<Result<Vec<_>, _>>()?;
        if paragraphs.len() != (end_ordinal - start_ordinal + 1) as usize {
            return Ok(None);
        }
        let mut snapshot = String::new();
        let mut previous_end = 0usize;
        for (index, (_, _, paragraph_start, paragraph_end, text)) in paragraphs.iter().enumerate() {
            let paragraph_start = (*paragraph_start).max(0) as usize;
            let paragraph_end = (*paragraph_end).max(0) as usize;
            if index > 0 {
                let gap = paragraph_start.saturating_sub(previous_end);
                if gap > 16_384 || snapshot.encode_utf16().count().saturating_add(gap) > 16_384 {
                    return Ok(None);
                }
                snapshot.extend(std::iter::repeat_n('\n', gap));
            }
            let from = if index == 0 { start } else { 0 };
            let to = if index + 1 == paragraphs.len() {
                end
            } else {
                text.encode_utf16().count()
            };
            let Some(part) = utf16_slice(text, from, to) else {
                return Ok(None);
            };
            snapshot.push_str(&part);
            if snapshot.encode_utf16().count() > 16_384 {
                return Ok(None);
            }
            previous_end = paragraph_end;
        }
        if snapshot.is_empty() {
            return Ok(None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        connection.execute("INSERT INTO highlights(id,book_id,start_paragraph_id,start_intra_utf16,end_paragraph_id,end_intra_utf16,text_snapshot,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", rusqlite::params![id, book_id, start_paragraph_id, start as i64, end_paragraph_id, end as i64, snapshot, created_at])?;
        Ok(Some(Highlight {
            id,
            book_id: book_id.to_owned(),
            book_title,
            chapter_id,
            chapter_title,
            start_paragraph_id: start_paragraph_id.to_owned(),
            start_intra_utf16: start,
            end_paragraph_id: end_paragraph_id.to_owned(),
            end_intra_utf16: end,
            text_snapshot: snapshot,
            created_at,
        }))
    }

    pub fn list_highlights(&self, book_id: Option<&str>) -> Result<Vec<Highlight>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut statement = connection.prepare(
            "SELECT h.id,h.book_id,b.title,c.id,c.title,h.start_paragraph_id,h.start_intra_utf16,h.end_paragraph_id,h.end_intra_utf16,h.text_snapshot,h.created_at \
             FROM highlights h JOIN books b ON b.id=h.book_id JOIN paragraphs p ON p.id=h.start_paragraph_id JOIN chapters c ON c.id=p.chapter_id \
             WHERE b.is_deleted=0 AND (?1 IS NULL OR h.book_id=?1) ORDER BY h.created_at DESC LIMIT 500",
        )?;
        let rows = statement.query_map([book_id], |row| {
            Ok(Highlight {
                id: row.get(0)?,
                book_id: row.get(1)?,
                book_title: row.get(2)?,
                chapter_id: row.get(3)?,
                chapter_title: row.get(4)?,
                start_paragraph_id: row.get(5)?,
                start_intra_utf16: row.get::<_, i64>(6)?.max(0) as usize,
                end_paragraph_id: row.get(7)?,
                end_intra_utf16: row.get::<_, i64>(8)?.max(0) as usize,
                text_snapshot: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_highlight(&self, id: &str) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        connection.execute("DELETE FROM highlights WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn record_paragraph_view(
        &self,
        book_id: &str,
        paragraph_id: &str,
        dwell_ms: u64,
    ) -> Result<(), DbError> {
        if dwell_ms < 1500 {
            return Ok(());
        }
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM paragraphs WHERE id=?1 AND book_id=?2)",
            rusqlite::params![paragraph_id, book_id],
            |row| row.get(0),
        )?;
        if exists {
            connection.execute("INSERT INTO paragraph_views(id,book_id,paragraph_id,viewed_at,dwell_ms) VALUES(?1,?2,?3,?4,?5)", rusqlite::params![uuid::Uuid::new_v4().to_string(), book_id, paragraph_id, chrono::Utc::now().to_rfc3339(), dwell_ms.min(300_000) as i64])?;
        }
        Ok(())
    }

    pub fn start_reading_session(
        &self,
        book_id: &str,
        anchor: Option<&ReadingAnchor>,
    ) -> Result<String, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let first_anchor = anchor
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        connection.execute(
            "INSERT INTO reading_sessions(id,book_id,started_at,first_anchor) VALUES(?1,?2,?3,?4)",
            rusqlite::params![id, book_id, now, first_anchor],
        )?;
        Ok(id)
    }

    pub fn end_reading_session(
        &self,
        session_id: &str,
        anchor: Option<&ReadingAnchor>,
    ) -> Result<(), DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let last_anchor = anchor
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        connection.execute("UPDATE reading_sessions SET ended_at=?2,last_anchor=?3 WHERE id=?1 AND ended_at IS NULL", rusqlite::params![session_id, chrono::Utc::now().to_rfc3339(), last_anchor])?;
        Ok(())
    }

    pub fn get_or_create_daily_quote(
        &self,
        local_date: &str,
    ) -> Result<Option<DailyQuote>, DbError> {
        if let Some(cached) = self.cached_daily_quote(local_date)? {
            return Ok(Some(cached));
        }
        let Some(candidate) = self.daily_quote_candidates(local_date)?.into_iter().next() else {
            return Ok(None);
        };
        self.save_daily_quote(local_date, &candidate, "local")
    }

    pub fn cached_daily_quote(&self, local_date: &str) -> Result<Option<DailyQuote>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let cached = connection
            .query_row(
                "SELECT q.local_date,q.book_id,b.title,b.author,q.chapter_id,c.title,q.paragraph_id,q.start_intra_utf16,q.end_intra_utf16,q.text_snapshot,q.source_hash,q.created_at,q.source_local_date,q.selection_method,p.text,b.sha256,EXISTS(SELECT 1 FROM paragraph_views pv WHERE pv.paragraph_id=q.paragraph_id AND date(pv.viewed_at,'localtime')=q.source_local_date) \
                 FROM daily_quotes q JOIN books b ON b.id=q.book_id JOIN chapters c ON c.id=q.chapter_id JOIN paragraphs p ON p.id=q.paragraph_id WHERE q.local_date=?1 AND b.is_deleted=0",
                [local_date],
                |row| {
                    Ok((DailyQuote {
                        local_date: row.get(0)?, book_id: row.get(1)?, book_title: row.get(2)?, author: row.get(3)?,
                        chapter_id: row.get(4)?, chapter_title: row.get(5)?, paragraph_id: row.get(6)?,
                        start_intra_utf16: row.get::<_, i64>(7)?.max(0) as usize,
                        end_intra_utf16: row.get::<_, i64>(8)?.max(0) as usize,
                        text_snapshot: row.get(9)?, source_hash: row.get(10)?, created_at: row.get(11)?,
                        source_local_date: row.get(12)?, selection_method: row.get(13)?,
                    }, row.get::<_, String>(14)?, row.get::<_, String>(15)?, row.get::<_, bool>(16)?))
                },
            )
            .optional()?;
        let Some((quote, source_text, expected_hash, was_read_on_source_date)) = cached else {
            return Ok(None);
        };
        let valid = quote.source_local_date < quote.local_date
            && was_read_on_source_date
            && matches!(
                quote.selection_method.as_str(),
                "ai" | "local" | "ai_fallback"
            )
            && quote.source_hash == expected_hash
            && utf16_slice(&source_text, quote.start_intra_utf16, quote.end_intra_utf16).as_deref()
                == Some(quote.text_snapshot.as_str());
        if valid {
            Ok(Some(quote))
        } else {
            connection.execute("DELETE FROM daily_quotes WHERE local_date=?1", [local_date])?;
            Ok(None)
        }
    }

    pub fn daily_quote_candidates(
        &self,
        local_date: &str,
    ) -> Result<Vec<DailyQuoteCandidate>, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let source_local_date: Option<String> = connection.query_row(
            "SELECT MAX(date(pv.viewed_at,'localtime')) FROM paragraph_views pv JOIN books b ON b.id=pv.book_id WHERE date(pv.viewed_at,'localtime') < ?1 AND b.is_deleted=0",
            [local_date],
            |row| row.get(0),
        )?;
        let Some(source_local_date) = source_local_date else {
            return Ok(Vec::new());
        };
        let paragraphs = {
            let mut statement = connection.prepare(
                "SELECT p.id,p.book_id,c.id,p.text,b.sha256,SUM(pv.dwell_ms) \
                 FROM paragraph_views pv JOIN paragraphs p ON p.id=pv.paragraph_id JOIN books b ON b.id=p.book_id JOIN chapters c ON c.id=p.chapter_id \
                 WHERE date(pv.viewed_at,'localtime')=?1 AND b.is_deleted=0 GROUP BY p.id \
                 ORDER BY SUM(pv.dwell_ms) DESC,MIN(pv.viewed_at) ASC,p.id ASC",
            )?;
            let rows = statement.query_map([&source_local_date], |row| {
                Ok(QuoteSourceParagraph {
                    paragraph_id: row.get(0)?,
                    book_id: row.get(1)?,
                    chapter_id: row.get(2)?,
                    text: row.get(3)?,
                    source_hash: row.get(4)?,
                    source_local_date: source_local_date.clone(),
                    dwell_ms: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        Ok(crate::quotes::build_daily_quote_candidates(&paragraphs))
    }

    pub fn save_daily_quote(
        &self,
        local_date: &str,
        candidate: &DailyQuoteCandidate,
        selection_method: &str,
    ) -> Result<Option<DailyQuote>, DbError> {
        if !matches!(selection_method, "ai" | "local" | "ai_fallback") {
            return Ok(None);
        }
        let connection = self.connection.lock().map_err(|_| DbError::LockPoisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let current_source_date: Option<String> = transaction.query_row(
            "SELECT MAX(date(pv.viewed_at,'localtime')) FROM paragraph_views pv JOIN books b ON b.id=pv.book_id WHERE date(pv.viewed_at,'localtime') < ?1 AND b.is_deleted=0",
            [local_date],
            |row| row.get(0),
        )?;
        if current_source_date.as_deref() != Some(candidate.source_local_date.as_str()) {
            transaction.rollback()?;
            return Ok(None);
        }
        let source = transaction.query_row(
            "SELECT b.title,b.author,c.title,p.text,b.sha256,EXISTS(SELECT 1 FROM paragraph_views pv WHERE pv.paragraph_id=p.id AND date(pv.viewed_at,'localtime')=?4) \
             FROM paragraphs p JOIN books b ON b.id=p.book_id JOIN chapters c ON c.id=p.chapter_id WHERE p.id=?1 AND p.book_id=?2 AND p.chapter_id=?3 AND b.is_deleted=0",
            rusqlite::params![candidate.paragraph_id,candidate.book_id,candidate.chapter_id,candidate.source_local_date],
            |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, bool>(5)?)),
        ).optional()?;
        let Some((book_title, author, chapter_title, source_text, source_hash, was_read)) = source
        else {
            transaction.rollback()?;
            return Ok(None);
        };
        if !was_read
            || source_hash != candidate.source_hash
            || utf16_slice(
                &source_text,
                candidate.start_intra_utf16,
                candidate.end_intra_utf16,
            )
            .as_deref()
                != Some(candidate.text.as_str())
        {
            transaction.rollback()?;
            return Ok(None);
        }
        let created_at = chrono::Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO daily_quotes(local_date,book_id,chapter_id,paragraph_id,start_intra_utf16,end_intra_utf16,text_snapshot,source_hash,created_at,source_local_date,selection_method) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(local_date) DO UPDATE SET book_id=excluded.book_id,chapter_id=excluded.chapter_id,paragraph_id=excluded.paragraph_id,start_intra_utf16=excluded.start_intra_utf16,end_intra_utf16=excluded.end_intra_utf16,text_snapshot=excluded.text_snapshot,source_hash=excluded.source_hash,created_at=excluded.created_at,source_local_date=excluded.source_local_date,selection_method=excluded.selection_method",
            rusqlite::params![local_date,candidate.book_id,candidate.chapter_id,candidate.paragraph_id,candidate.start_intra_utf16 as i64,candidate.end_intra_utf16 as i64,candidate.text,candidate.source_hash,created_at,candidate.source_local_date,selection_method],
        )?;
        transaction.commit()?;
        Ok(Some(DailyQuote {
            local_date: local_date.to_owned(),
            book_id: candidate.book_id.clone(),
            book_title,
            author,
            chapter_id: candidate.chapter_id.clone(),
            chapter_title,
            paragraph_id: candidate.paragraph_id.clone(),
            start_intra_utf16: candidate.start_intra_utf16,
            end_intra_utf16: candidate.end_intra_utf16,
            text_snapshot: candidate.text.clone(),
            source_hash: candidate.source_hash.clone(),
            created_at,
            source_local_date: candidate.source_local_date.clone(),
            selection_method: selection_method.to_owned(),
        }))
    }
}

fn map_absolute_offset(
    paragraphs: &[NewParagraphLocation<'_>],
    absolute_utf16: usize,
) -> Option<MappedAnchor> {
    let mut following = None;
    let mut previous = None;
    for location in paragraphs {
        let paragraph = location.paragraph;
        if paragraph.start_utf16 <= absolute_utf16 && absolute_utf16 < paragraph.end_utf16 {
            return Some(mapped_anchor(location, absolute_utf16));
        }
        if following.is_none() && paragraph.start_utf16 >= absolute_utf16 {
            following = Some(location);
        }
        if paragraph.end_utf16 <= absolute_utf16 {
            previous = Some(location);
        }
    }
    following
        .or(previous)
        .map(|location| mapped_anchor(location, absolute_utf16))
}

fn mapped_anchor(location: &NewParagraphLocation<'_>, absolute_utf16: usize) -> MappedAnchor {
    let paragraph = location.paragraph;
    let offset = absolute_utf16
        .saturating_sub(paragraph.start_utf16)
        .min(utf16_len(&paragraph.text));
    MappedAnchor {
        chapter_id: location.chapter_id.to_owned(),
        paragraph_id: paragraph.id.clone(),
        intra_utf16: clamp_utf16_boundary(&paragraph.text, offset),
    }
}

fn map_source_range(
    paragraphs: &[NewParagraphLocation<'_>],
    start_utf16: usize,
    end_utf16: usize,
    expected_text: &str,
) -> Option<(MappedAnchor, usize)> {
    paragraphs.iter().find_map(|location| {
        let paragraph = location.paragraph;
        if start_utf16 < paragraph.start_utf16 || end_utf16 > paragraph.end_utf16 {
            return None;
        }
        let start = start_utf16 - paragraph.start_utf16;
        let end = end_utf16 - paragraph.start_utf16;
        (utf16_slice(&paragraph.text, start, end).as_deref() == Some(expected_text))
            .then(|| (mapped_anchor(location, start_utf16), end))
    })
}

fn remap_session_anchor(
    serialized: &str,
    paragraphs: &[NewParagraphLocation<'_>],
) -> Option<String> {
    let mut anchor: serde_json::Value = serde_json::from_str(serialized).ok()?;
    let absolute = anchor.get("absoluteUtf16")?.as_u64()? as usize;
    let mapped = map_absolute_offset(paragraphs, absolute)?;
    anchor["chapterId"] = serde_json::Value::String(mapped.chapter_id);
    anchor["paragraphId"] = serde_json::Value::String(mapped.paragraph_id);
    anchor["intraParagraphUtf16"] = serde_json::Value::from(mapped.intra_utf16 as u64);
    anchor["parserVersion"] = serde_json::Value::from(2);
    serde_json::to_string(&anchor).ok()
}

fn clamp_utf16_boundary(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for character in text.chars() {
        if units + character.len_utf16() > offset {
            return units;
        }
        units += character.len_utf16();
        if units == offset {
            return units;
        }
    }
    units
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn utf16_slice(text: &str, start: usize, end: usize) -> Option<String> {
    if end < start {
        return None;
    }
    let mut units = 0;
    let mut byte_start = if start == 0 { Some(0) } else { None };
    let mut byte_end = None;
    for (byte, character) in text.char_indices() {
        if units == start {
            byte_start = Some(byte);
        }
        if units == end {
            byte_end = Some(byte);
            break;
        }
        units += character.len_utf16();
        if units > start && byte_start.is_none() {
            return None;
        }
        if units > end {
            return None;
        }
    }
    if byte_start.is_none() && units == start {
        byte_start = Some(text.len());
    }
    if byte_end.is_none() && units == end {
        byte_end = Some(text.len());
    }
    Some(text.get(byte_start?..byte_end?)?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{record_view_on_date, Database};
    use crate::{
        importer::{import_txt_file, repair_legacy_single_chapter_index},
        models::ReadingAnchor,
        parser::{ParsedBook, ParsedChapter, ParsedParagraph},
    };
    use sha2::{Digest, Sha256};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn ai_question_context_only_contains_paragraphs_before_current_reading_position() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();
        let mut absolute = 0usize;
        let mut paragraph_global_ordinal = 0usize;
        let mut chapters = Vec::new();
        for (chapter_ordinal, (chapter_id, title, texts)) in [
            (
                "chapter-one",
                "第一章",
                vec!["READ_PREFIX_MARKER 人在风雨中保持耐心。"],
            ),
            (
                "chapter-current",
                "当前章",
                vec![
                    "CURRENT_CHAPTER_READ_MARKER 人物保持沉默。",
                    "CURRENT_CHAPTER_LAST_READ_MARKER 她看见了旧信。",
                    "UNREAD_CURRENT_MARKER 信的结尾还没有读到。",
                ],
            ),
            (
                "chapter-next",
                "下一章",
                vec!["UNREAD_NEXT_MARKER 新的人物出现。"],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let chapter_start = absolute;
            let mut paragraphs = Vec::new();
            for text in texts {
                let start_utf16 = absolute;
                let end_utf16 = start_utf16 + text.encode_utf16().count();
                paragraphs.push(ParsedParagraph {
                    id: format!("{chapter_id}-p{paragraph_global_ordinal}"),
                    chapter_id: chapter_id.into(),
                    ordinal: paragraph_global_ordinal,
                    text: text.into(),
                    start_utf16,
                    end_utf16,
                });
                paragraph_global_ordinal += 1;
                absolute = end_utf16 + 2;
            }
            let chapter_end = absolute - 2;
            chapters.push(ParsedChapter {
                id: chapter_id.into(),
                ordinal: chapter_ordinal,
                title: title.into(),
                start_utf16: chapter_start,
                end_utf16: chapter_end,
                paragraphs,
            });
        }
        let total_utf16 = absolute - 2;
        let parsed = ParsedBook {
            normalized_sha256: "synthetic-context-book".into(),
            total_utf16,
            chapters,
        };
        database
            .insert_imported_book(
                "context-book",
                "synthetic-context-book-hash",
                "合成测试书",
                "作者",
                "context.txt",
                "context.txt",
                &parsed,
            )
            .unwrap();
        let other_text = "OTHER_BOOK_MARKER 这段内容来自另一本书。";
        let other_end = other_text.encode_utf16().count();
        let other_book = ParsedBook {
            normalized_sha256: "synthetic-other-book".into(),
            total_utf16: other_end,
            chapters: vec![ParsedChapter {
                id: "other-chapter".into(),
                ordinal: 0,
                title: "另一本书的章节".into(),
                start_utf16: 0,
                end_utf16: other_end,
                paragraphs: vec![ParsedParagraph {
                    id: "other-paragraph".into(),
                    chapter_id: "other-chapter".into(),
                    ordinal: 0,
                    text: other_text.into(),
                    start_utf16: 0,
                    end_utf16: other_end,
                }],
            }],
        };
        database
            .insert_imported_book(
                "other-book",
                "synthetic-other-book-hash",
                "另一本合成书",
                "作者",
                "other.txt",
                "other.txt",
                &other_book,
            )
            .unwrap();
        let read_paragraph = parsed.chapters[1].paragraphs[1].clone();
        database
            .save_reading_position(&ReadingAnchor {
                book_id: "context-book".into(),
                chapter_id: "chapter-current".into(),
                paragraph_id: read_paragraph.id.clone(),
                intra_paragraph_utf16: read_paragraph.text.encode_utf16().count(),
                absolute_utf16: read_paragraph.end_utf16,
                nearby_text: String::new(),
                parser_version: 2,
            })
            .unwrap();
        let selected = database
            .reconstruct_ai_question_selection(
                "context-book",
                "chapter-current",
                &read_paragraph.id,
                0,
                &read_paragraph.id,
                read_paragraph.text.encode_utf16().count(),
                &read_paragraph.text,
            )
            .unwrap()
            .unwrap();
        assert_eq!(selected, read_paragraph.text);
        assert!(database
            .reconstruct_ai_question_selection(
                "context-book",
                "chapter-current",
                &read_paragraph.id,
                0,
                &read_paragraph.id,
                read_paragraph.text.encode_utf16().count(),
                "伪造选段",
            )
            .unwrap()
            .is_none());
        let unread_paragraph = parsed.chapters[1].paragraphs[2].clone();
        assert!(database
            .reconstruct_ai_question_selection(
                "context-book",
                "chapter-current",
                &unread_paragraph.id,
                0,
                &unread_paragraph.id,
                unread_paragraph.text.encode_utf16().count(),
                &unread_paragraph.text,
            )
            .unwrap()
            .is_none());

        let context = database
            .read_ai_question_context("context-book", "chapter-current")
            .unwrap()
            .unwrap();

        assert_eq!(context.current_chapter.len(), 2);
        assert_eq!(context.earlier_book.len(), 1);
        let included = context
            .current_chapter
            .iter()
            .chain(context.earlier_book.iter())
            .map(|paragraph| paragraph.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(included.contains("READ_PREFIX_MARKER"));
        assert!(included.contains("CURRENT_CHAPTER_READ_MARKER"));
        assert!(included.contains("CURRENT_CHAPTER_LAST_READ_MARKER"));
        assert!(!included.contains("OTHER_BOOK_MARKER"));
        assert!(!included.contains("UNREAD_CURRENT_MARKER"));
        assert!(!included.contains("UNREAD_NEXT_MARKER"));
        assert!(database
            .read_ai_question_context("context-book", "chapter-next")
            .unwrap()
            .is_none());
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE reading_positions SET absolute_utf16=999999 WHERE book_id='context-book'",
                [],
            )
            .unwrap();
        assert!(database
            .read_ai_question_context("context-book", "chapter-current")
            .unwrap()
            .is_none());
    }

    #[test]
    fn fresh_database_applies_the_initial_schema() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();

        assert_eq!(database.schema_version().unwrap(), 5);
        for table in [
            "books",
            "chapters",
            "paragraphs",
            "reading_positions",
            "bookmarks",
            "highlights",
            "paragraph_views",
            "daily_quotes",
            "reading_sessions",
            "recommendation_cache",
            "discovery_search_cache",
            "preferences",
        ] {
            assert!(database.has_table(table).unwrap(), "missing {table}");
        }
    }

    #[test]
    fn reopening_database_keeps_preferences_and_does_not_rerun_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shuyu.sqlite3");
        let database = Database::open(&path).unwrap();
        database
            .save_preference("reader", r#"{"theme":"dark"}"#)
            .unwrap();
        drop(database);

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), 5);
        assert_eq!(
            reopened.preference("reader").unwrap().as_deref(),
            Some(r#"{"theme":"dark"}"#)
        );
    }

    #[test]
    fn repairs_legacy_single_chapter_index_without_losing_reader_state() {
        let directory = tempfile::tempdir().unwrap();
        let books_directory = directory.path().join("books");
        std::fs::create_dir_all(&books_directory).unwrap();
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();
        let book_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let text = "第一章 起风\n山里的猫。\n第二章 归来\n春天来了。";
        let source_path = books_directory.join(format!("{book_id}.txt"));
        std::fs::write(&source_path, text).unwrap();
        let raw_hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let old_chapter_id = format!("{book_id}:c:0:v1");
        let old_paragraph_id = format!("{book_id}:p:0:v1");
        let old_parsed = ParsedBook {
            normalized_sha256: raw_hash.clone(),
            total_utf16: text.encode_utf16().count(),
            chapters: vec![ParsedChapter {
                id: old_chapter_id.clone(),
                ordinal: 0,
                title: "正文".into(),
                start_utf16: 0,
                end_utf16: text.encode_utf16().count(),
                paragraphs: vec![ParsedParagraph {
                    id: old_paragraph_id.clone(),
                    chapter_id: old_chapter_id.clone(),
                    ordinal: 0,
                    text: text.into(),
                    start_utf16: 0,
                    end_utf16: text.encode_utf16().count(),
                }],
            }],
        };
        database
            .insert_imported_book(
                book_id,
                &raw_hash,
                "章节测试",
                "未知作者",
                "章节测试.txt",
                &source_path.to_string_lossy(),
                &old_parsed,
            )
            .unwrap();
        database
            .connection
            .lock()
            .unwrap()
            .execute("UPDATE books SET parser_version=1 WHERE id=?1", [book_id])
            .unwrap();

        let offset_of = |needle: &str| text[..text.find(needle).unwrap()].encode_utf16().count();
        let second_body_start = offset_of("春天来了。");
        let anchor = ReadingAnchor {
            book_id: book_id.into(),
            chapter_id: old_chapter_id.clone(),
            paragraph_id: old_paragraph_id.clone(),
            intra_paragraph_utf16: second_body_start,
            absolute_utf16: second_body_start,
            nearby_text: "春天来了。".into(),
            parser_version: 1,
        };
        database.save_reading_position(&anchor).unwrap();
        assert!(database
            .add_bookmark(
                book_id,
                &old_paragraph_id,
                offset_of("猫"),
                Some("保留注释")
            )
            .unwrap());
        database
            .add_highlight(
                book_id,
                &old_paragraph_id,
                offset_of("山里"),
                &old_paragraph_id,
                text.encode_utf16().count(),
            )
            .unwrap()
            .unwrap();
        database
            .record_paragraph_view(book_id, &old_paragraph_id, 8_000)
            .unwrap();
        let session_id = database
            .start_reading_session(book_id, Some(&anchor))
            .unwrap();
        database
            .end_reading_session(&session_id, Some(&anchor))
            .unwrap();
        {
            let connection = database.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE paragraph_views SET viewed_at='2999-12-31T12:00:00Z' WHERE book_id=?1",
                    [book_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO daily_quotes(local_date,book_id,chapter_id,paragraph_id,start_intra_utf16,end_intra_utf16,text_snapshot,source_hash,created_at,source_local_date,selection_method) VALUES('3000-01-01',?1,?2,?3,?4,?5,'春天来了。',?6,'2999-12-31T12:00:00Z','2999-12-31','local')",
                    rusqlite::params![book_id, old_chapter_id, old_paragraph_id, second_body_start as i64, (second_body_start + "春天来了。".encode_utf16().count()) as i64, raw_hash],
                )
                .unwrap();
        }

        assert!(repair_legacy_single_chapter_index(&database, &books_directory, book_id).unwrap());
        let chapters = database.list_chapters(book_id).unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第一章 起风");
        assert_eq!(chapters[1].title, "第二章 归来");
        assert_eq!(
            database
                .chapter_content(&chapters[0].id)
                .unwrap()
                .unwrap()
                .paragraphs[0]
                .text,
            "山里的猫。"
        );
        assert_eq!(
            database
                .chapter_content(&chapters[1].id)
                .unwrap()
                .unwrap()
                .paragraphs[0]
                .text,
            "春天来了。"
        );

        let restored_position = database.get_reading_position(book_id).unwrap().unwrap();
        assert_eq!(restored_position.chapter_id, chapters[1].id);
        assert_eq!(restored_position.absolute_utf16, second_body_start);
        assert_eq!(restored_position.parser_version, 2);
        let bookmarks = database.list_bookmarks(Some(book_id)).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].note.as_deref(), Some("保留注释"));
        assert_eq!(database.list_highlights(Some(book_id)).unwrap().len(), 1);
        let quote = database.cached_daily_quote("3000-01-01").unwrap().unwrap();
        assert_eq!(quote.chapter_id, chapters[1].id);
        assert_eq!(quote.text_snapshot, "春天来了。");
        {
            let connection = database.connection.lock().unwrap();
            let (view_count, dwell_total): (i64, i64) = connection
                .query_row(
                    "SELECT COUNT(*),SUM(dwell_ms) FROM paragraph_views WHERE book_id=?1",
                    [book_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(view_count, 2);
            assert_eq!(dwell_total, 8_000);
            let (first_anchor, last_anchor): (String, String) = connection
                .query_row(
                    "SELECT first_anchor,last_anchor FROM reading_sessions WHERE id=?1",
                    [&session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let first: ReadingAnchor = serde_json::from_str(&first_anchor).unwrap();
            let last: ReadingAnchor = serde_json::from_str(&last_anchor).unwrap();
            assert_eq!(first.chapter_id, chapters[1].id);
            assert_eq!(first.parser_version, 2);
            assert_eq!(last.paragraph_id, first.paragraph_id);
        }
        assert_eq!(std::fs::read(&source_path).unwrap(), text.as_bytes());
    }

    #[test]
    fn discovery_search_cache_survives_reopen_and_updates_by_query_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shuyu.sqlite3");
        let database = Database::open(&path).unwrap();
        database
            .save_discovery_search_cache("query-hash", "科幻", "[]", "2026-10-08T00:00:00Z")
            .unwrap();
        database
            .save_discovery_search_cache("query-hash", "科幻", "[{}]", "2026-10-08T00:01:00Z")
            .unwrap();
        drop(database);

        let reopened = Database::open(&path).unwrap();
        assert_eq!(
            reopened.discovery_search_cache("query-hash").unwrap(),
            Some(("[{}]".into(), "2026-10-08T00:01:00Z".into()))
        );
        assert!(reopened
            .discovery_search_cache("missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn version_one_database_adds_discovery_and_quote_fields_without_losing_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shuyu.sqlite3");
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch(include_str!("../migrations/001_initial.sql"))
                .unwrap();
            connection
                .execute(
                    "INSERT INTO preferences(key,json_value,updated_at) VALUES('reader','{\"theme\":\"dark\"}',datetime('now'))",
                    [],
                )
                .unwrap();
            connection.pragma_update(None, "user_version", 1).unwrap();
        }

        let upgraded = Database::open(&path).unwrap();
        assert_eq!(upgraded.schema_version().unwrap(), 5);
        assert_eq!(
            upgraded.preference("reader").unwrap().as_deref(),
            Some(r#"{"theme":"dark"}"#)
        );
        assert!(upgraded.has_table("discovery_search_cache").unwrap());
        let connection = upgraded.connection.lock().unwrap();
        let quote_fields = connection
            .prepare("PRAGMA table_info(daily_quotes)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(quote_fields.contains(&"source_local_date".into()));
        assert!(quote_fields.contains(&"selection_method".into()));
    }

    #[test]
    fn version_three_migration_saves_ai_rollback_and_preserves_reader_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shuyu.sqlite3");
        let legacy_ai = r#"{"enabled":true,"provider":"openrouter","model":"vendor/model","ollamaEndpoint":"http://127.0.0.1:11434"}"#;
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
                .execute_batch(
                    "INSERT INTO books(id,sha256,normalized_sha256,title,author,import_name,file_path,total_utf16,chapter_count,created_at) VALUES('book','sha','normalized','Fixture','Author','book.txt','fixture.txt',8,1,'2026-10-08T00:00:00Z');
                     INSERT INTO chapters(id,book_id,ordinal,title,start_utf16,end_utf16) VALUES('chapter','book',0,'Chapter',0,8);
                     INSERT INTO paragraphs(id,book_id,chapter_id,ordinal,text,start_utf16,end_utf16) VALUES('paragraph','book','chapter',0,'Fixture text',0,8);
                     INSERT INTO reading_positions(book_id,chapter_id,paragraph_id,intra_utf16,absolute_utf16,nearby_text,progress_ratio,updated_at) VALUES('book','chapter','paragraph',2,2,'Fixture',0.25,'2026-10-08T00:01:00Z');
                     INSERT INTO bookmarks(id,book_id,paragraph_id,intra_utf16,created_at,note) VALUES('bookmark','book','paragraph',1,'2026-10-08T00:02:00Z','saved note');
                     INSERT INTO highlights(id,book_id,start_paragraph_id,start_intra_utf16,end_paragraph_id,end_intra_utf16,text_snapshot,created_at) VALUES('highlight','book','paragraph',0,'paragraph',2,'Fi','2026-10-08T00:03:00Z');
                     INSERT INTO paragraph_views(id,book_id,paragraph_id,viewed_at,dwell_ms) VALUES('view','book','paragraph','2026-10-08T00:04:00Z',90000);
                     INSERT INTO reading_sessions(id,book_id,started_at,ended_at,first_anchor,last_anchor) VALUES('session','book','2026-10-08T00:00:00Z',NULL,'first','last');
                     INSERT INTO daily_quotes(local_date,book_id,chapter_id,paragraph_id,start_intra_utf16,end_intra_utf16,text_snapshot,source_hash,created_at,source_local_date,selection_method) VALUES('2026-10-08','book','chapter','paragraph',0,2,'Fi','hash','2026-10-08T00:05:00Z','2026-10-07','local');
                     INSERT INTO discovery_search_cache(query_hash,query,result_json,fetched_at) VALUES('hash','query','[]','2026-10-08T00:06:00Z');
                     INSERT INTO preferences(key,json_value,updated_at) VALUES('reader','{\"theme\":\"dark\"}','2026-10-08T00:07:00Z');",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO preferences(key,json_value,updated_at) VALUES('ai_client',?1,'2026-10-08T00:07:00Z')",
                    [legacy_ai],
                )
                .unwrap();
            connection.pragma_update(None, "user_version", 3).unwrap();
        }

        let upgraded = Database::open(&path).unwrap();
        assert_eq!(upgraded.schema_version().unwrap(), 5);
        assert_eq!(
            upgraded
                .preference("ai_client_rollback_v4")
                .unwrap()
                .as_deref(),
            Some(legacy_ai)
        );
        assert_eq!(
            upgraded.preference("reader").unwrap().as_deref(),
            Some(r#"{"theme":"dark"}"#)
        );
        let active: serde_json::Value = serde_json::from_str(
            upgraded
                .preference("ai_client")
                .unwrap()
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(active["enabled"], false);
        assert_eq!(active["selectedModelSlug"], serde_json::Value::Null);
        assert_eq!(active["allowAiQa"], false);
        assert_eq!(active["allowAiDailyQuote"], false);
        assert!(active.get("provider").is_none());
        assert!(active.get("ollamaEndpoint").is_none());

        let connection = upgraded.connection.lock().unwrap();
        for table in [
            "books",
            "chapters",
            "paragraphs",
            "reading_positions",
            "bookmarks",
            "highlights",
            "paragraph_views",
            "reading_sessions",
            "daily_quotes",
            "discovery_search_cache",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "migration changed {table}");
        }
        let reader_state: (String, f64) = connection
            .query_row(
                "SELECT nearby_text, progress_ratio FROM reading_positions WHERE book_id='book'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(reader_state, ("Fixture".into(), 0.25));
        let quote: (String, String) = connection
            .query_row(
                "SELECT text_snapshot, selection_method FROM daily_quotes WHERE local_date='2026-10-08'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(quote, ("Fi".into(), "local".into()));
    }

    #[test]
    fn version_four_migration_failure_rolls_back_preferences_and_schema_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shuyu.sqlite3");
        let legacy_ai = r#"{"enabled":true,"provider":"openrouter","model":"vendor/model"}"#;
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
                .execute(
                    "INSERT INTO preferences(key,json_value,updated_at) VALUES('ai_client',?1,'2026-10-08T00:07:00Z')",
                    [legacy_ai],
                )
                .unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_ai_migration BEFORE UPDATE ON preferences \
                 WHEN OLD.key='ai_client' BEGIN SELECT RAISE(ABORT,'migration denied'); END;",
                )
                .unwrap();
            connection.pragma_update(None, "user_version", 3).unwrap();
        }

        assert!(Database::open(&path).is_err());

        let connection = rusqlite::Connection::open(&path).unwrap();
        let schema_version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let active_ai: String = connection
            .query_row(
                "SELECT json_value FROM preferences WHERE key='ai_client'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let rollback_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM preferences WHERE key='ai_client_rollback_v4'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, 3);
        assert_eq!(active_ai, legacy_ai);
        assert_eq!(rollback_count, 0);
    }

    #[test]
    fn quote_source_falls_back_to_the_most_recent_read_date_and_ignores_today() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("reader.txt");
        let text = "第一章 起风\n\n这是上一段阅读中值得记住的一句原文，后来的人仍然能够找到方向。";
        std::fs::write(&source, text).unwrap();
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();
        let imported = import_txt_file(
            &database,
            &directory.path().join("books"),
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let chapter = database.list_chapters(&imported.book_id).unwrap().remove(0);
        let paragraph = database
            .chapter_content(&chapter.id)
            .unwrap()
            .unwrap()
            .paragraphs
            .into_iter()
            .find(|paragraph| paragraph.text.starts_with("这是上一段"))
            .unwrap();
        let today = chrono::Local::now().date_naive();
        let requested_date = today.to_string();
        let latest_read_date = today - chrono::Days::new(3);
        record_view_on_date(
            &database,
            &imported.book_id,
            &paragraph.id,
            &latest_read_date.to_string(),
        );
        database
            .record_paragraph_view(&imported.book_id, &paragraph.id, 90_000)
            .unwrap();
        let candidates = database.daily_quote_candidates(&requested_date).unwrap();
        assert!(
            !candidates.is_empty(),
            "no candidates for {latest_read_date}"
        );
        assert_eq!(
            candidates[0].source_local_date,
            latest_read_date.to_string()
        );

        let quote = database
            .get_or_create_daily_quote(&requested_date)
            .unwrap()
            .unwrap();

        assert_eq!(quote.source_local_date, latest_read_date.to_string());
        assert_eq!(quote.text_snapshot, paragraph.text);
        assert_eq!(quote.selection_method, "local");
        let candidate = candidates.first().unwrap();
        let mut invented = candidate.clone();
        invented.text = "AI改写出来的句子，不得展示。".into();
        assert!(database
            .save_daily_quote(&requested_date, &invented, "ai")
            .unwrap()
            .is_none());
        let ai_selected = database
            .save_daily_quote(&requested_date, candidate, "ai")
            .unwrap()
            .unwrap();
        assert_eq!(ai_selected.text_snapshot, candidate.text);
        assert_eq!(ai_selected.selection_method, "ai");
        assert_eq!(
            database
                .cached_daily_quote(&requested_date)
                .unwrap()
                .unwrap()
                .text_snapshot,
            candidate.text
        );
        {
            let connection = database.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE daily_quotes SET source_local_date=?1 WHERE local_date=?1",
                    [&requested_date],
                )
                .unwrap();
        }
        assert!(database
            .cached_daily_quote(&requested_date)
            .unwrap()
            .is_none());
    }

    #[test]
    fn positions_bookmarks_cross_paragraph_highlights_sessions_and_daily_quotes_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("reader.txt");
        let books_dir = directory.path().join("books");
        let text = "第一章 起风\n\n甲乙😀丙\n\n后文一段继续向前，读者在此停留，今日摘录应该来自这段真实正文。\n\n第二章 回家\n\n终章正文";
        std::fs::write(&source, text).unwrap();
        let database = Database::open(&directory.path().join("shuyu.sqlite3")).unwrap();
        let imported = import_txt_file(
            &database,
            &books_dir,
            &source,
            None,
            false,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let chapters = database.list_chapters(&imported.book_id).unwrap();
        let first_chapter = database.chapter_content(&chapters[0].id).unwrap().unwrap();
        let first_paragraph = &first_chapter.paragraphs[0];
        let second_paragraph = &first_chapter.paragraphs[1];

        let anchor = ReadingAnchor {
            book_id: imported.book_id.clone(),
            chapter_id: first_chapter.chapter.id.clone(),
            paragraph_id: second_paragraph.id.clone(),
            intra_paragraph_utf16: 2,
            absolute_utf16: second_paragraph.start_utf16 + 2,
            nearby_text: "后文".into(),
            parser_version: 1,
        };
        database.save_reading_position(&anchor).unwrap();
        assert_eq!(
            database
                .get_reading_position(&imported.book_id)
                .unwrap()
                .unwrap()
                .paragraph_id,
            second_paragraph.id
        );

        assert!(database
            .add_bookmark(&imported.book_id, &first_paragraph.id, 2, None)
            .unwrap());
        assert!(!database
            .add_bookmark(&imported.book_id, &first_paragraph.id, 2, None)
            .unwrap());
        assert_eq!(
            database
                .list_bookmarks(Some(&imported.book_id))
                .unwrap()
                .len(),
            1
        );

        let highlight = database
            .add_highlight(
                &imported.book_id,
                &first_paragraph.id,
                1,
                &second_paragraph.id,
                2,
            )
            .unwrap()
            .unwrap();
        assert_eq!(highlight.text_snapshot, "乙😀丙\n\n后文");
        assert_eq!(
            database
                .list_highlights(Some(&imported.book_id))
                .unwrap()
                .len(),
            1
        );

        let date = chrono::Local::now().date_naive().to_string();
        assert!(database.get_or_create_daily_quote(&date).unwrap().is_none());
        database
            .record_paragraph_view(&imported.book_id, &second_paragraph.id, 4200)
            .unwrap();
        let yesterday = chrono::Local::now().date_naive() - chrono::Days::new(1);
        record_view_on_date(
            &database,
            &imported.book_id,
            &second_paragraph.id,
            &yesterday.to_string(),
        );
        let quote = database.get_or_create_daily_quote(&date).unwrap().unwrap();
        assert_eq!(quote.text_snapshot, second_paragraph.text);
        assert_eq!(
            quote.author,
            database.list_books("recent").unwrap()[0].author
        );
        assert_eq!(quote.source_local_date, yesterday.to_string());
        assert_eq!(
            quote.source_hash,
            sha2::Sha256::digest(text.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let cached = database.get_or_create_daily_quote(&date).unwrap().unwrap();
        assert_eq!(cached.text_snapshot, quote.text_snapshot);
        assert_eq!(cached.created_at, quote.created_at);

        let session_id = database
            .start_reading_session(&imported.book_id, Some(&anchor))
            .unwrap();
        database
            .end_reading_session(&session_id, Some(&anchor))
            .unwrap();
    }
}

#[cfg(test)]
fn record_view_on_date(database: &Database, book_id: &str, paragraph_id: &str, date: &str) {
    use chrono::TimeZone;
    let local_date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    let local_noon = local_date.and_hms_opt(12, 0, 0).unwrap();
    let viewed_at = chrono::Local
        .from_local_datetime(&local_noon)
        .single()
        .unwrap()
        .to_rfc3339();
    let connection = database.connection.lock().unwrap();
    connection
        .execute(
            "INSERT INTO paragraph_views(id,book_id,paragraph_id,viewed_at,dwell_ms) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), book_id, paragraph_id, viewed_at, 4_200_i64],
        )
        .unwrap();
}
