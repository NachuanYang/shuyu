use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookSummary {
    pub id: String,
    pub title: String,
    pub author: String,
    pub import_name: String,
    pub chapter_count: usize,
    pub total_utf16: usize,
    pub created_at: String,
    pub last_opened_at: Option<String>,
    pub progress_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterContent {
    pub chapter: Chapter,
    pub paragraphs: Vec<Paragraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub id: String,
    pub book_id: String,
    pub ordinal: usize,
    pub title: String,
    pub start_utf16: usize,
    pub end_utf16: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paragraph {
    pub id: String,
    pub book_id: String,
    pub chapter_id: String,
    pub ordinal: usize,
    pub text: String,
    pub start_utf16: usize,
    pub end_utf16: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingAnchor {
    pub book_id: String,
    pub chapter_id: String,
    pub paragraph_id: String,
    pub intra_paragraph_utf16: usize,
    pub absolute_utf16: usize,
    pub nearby_text: String,
    pub parser_version: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiQuestionParagraph {
    pub chapter_ordinal: usize,
    pub paragraph_ordinal: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiQuestionContext {
    pub chapter_id: String,
    pub chapter_title: String,
    pub chapter_ordinal: usize,
    pub reading_position_utf16: usize,
    pub current_chapter: Vec<AiQuestionParagraph>,
    pub earlier_book: Vec<AiQuestionParagraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: String,
    pub book_id: String,
    pub book_title: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub paragraph_id: String,
    pub intra_utf16: usize,
    pub text_preview: String,
    pub created_at: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Highlight {
    pub id: String,
    pub book_id: String,
    pub book_title: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub start_paragraph_id: String,
    pub start_intra_utf16: usize,
    pub end_paragraph_id: String,
    pub end_intra_utf16: usize,
    pub text_snapshot: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyQuote {
    pub local_date: String,
    pub book_id: String,
    pub book_title: String,
    pub author: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub paragraph_id: String,
    pub start_intra_utf16: usize,
    pub end_intra_utf16: usize,
    pub text_snapshot: String,
    pub source_hash: String,
    pub created_at: String,
    pub source_local_date: String,
    pub selection_method: String,
}

#[derive(Debug, Clone)]
pub struct QuoteSourceParagraph {
    pub book_id: String,
    pub chapter_id: String,
    pub paragraph_id: String,
    pub text: String,
    pub source_hash: String,
    pub source_local_date: String,
    pub dwell_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyQuoteCandidate {
    pub candidate_id: String,
    pub book_id: String,
    pub chapter_id: String,
    pub paragraph_id: String,
    pub start_intra_utf16: usize,
    pub end_intra_utf16: usize,
    pub text: String,
    pub source_hash: String,
    pub source_local_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalBook {
    pub work_id: String,
    pub title: String,
    pub author: String,
    pub first_publish_year: Option<i32>,
    pub edition_count: Option<u32>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookDiscoveryResult {
    pub items: Vec<ExternalBook>,
    pub from_cache: bool,
    pub stale: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphView {
    pub book_id: String,
    pub chapter_id: String,
    pub paragraph_id: String,
    pub text: String,
    pub start_utf16: usize,
    pub viewed_at: String,
    pub dwell_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub book_id: String,
    pub title: String,
    pub chapter_count: usize,
    pub total_utf16: usize,
    pub status: String,
}
