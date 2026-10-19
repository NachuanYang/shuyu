use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ParsedBook {
    pub normalized_sha256: String,
    pub total_utf16: usize,
    pub chapters: Vec<ParsedChapter>,
}

#[derive(Debug, Clone)]
pub struct ParsedChapter {
    pub id: String,
    pub ordinal: usize,
    pub title: String,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub paragraphs: Vec<ParsedParagraph>,
}

#[derive(Debug, Clone)]
pub struct ParsedParagraph {
    pub id: String,
    pub chapter_id: String,
    pub ordinal: usize,
    pub text: String,
    pub start_utf16: usize,
    pub end_utf16: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unsupported text encoding or invalid byte sequence")]
    InvalidEncoding,
    #[error("text has no readable content")]
    Empty,
    #[error("import was cancelled")]
    Cancelled,
}

pub fn decode_txt(bytes: &[u8], encoding: Option<&str>) -> Result<String, ParseError> {
    let encoding = encoding.map(str::to_ascii_lowercase);
    let decoded = match encoding.as_deref() {
        Some("utf-8" | "utf8") => decode_utf8(bytes)?,
        Some("utf-16le" | "utf16le") => decode_utf16(bytes, true)?,
        Some("utf-16be" | "utf16be") => decode_utf16(bytes, false)?,
        Some("gbk" | "gb18030") => decode_gb18030(bytes)?,
        Some("auto") | None => decode_auto(bytes)?,
        _ => return Err(ParseError::InvalidEncoding),
    };
    let normalized = normalize_newlines(&decoded);
    if normalized.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(normalized)
}

fn decode_auto(bytes: &[u8]) -> Result<String, ParseError> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return decode_utf8(bytes);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(bytes, true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(bytes, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_owned()),
        Err(_) => decode_gb18030(bytes),
    }
}

fn decode_utf8(bytes: &[u8]) -> Result<String, ParseError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ParseError::InvalidEncoding)
}

fn decode_gb18030(bytes: &[u8]) -> Result<String, ParseError> {
    let (decoded, had_errors) = encoding_rs::GB18030.decode_without_bom_handling(bytes);
    if had_errors {
        return Err(ParseError::InvalidEncoding);
    }
    Ok(decoded.into_owned())
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, ParseError> {
    let bytes = if little_endian {
        bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes)
    } else {
        bytes.strip_prefix(&[0xFE, 0xFF]).unwrap_or(bytes)
    };
    if bytes.len() % 2 != 0 {
        return Err(ParseError::InvalidEncoding);
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| ParseError::InvalidEncoding)
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[derive(Debug)]
struct TextBlock {
    text: String,
    start_utf16: usize,
    end_utf16: usize,
}

fn collect_blocks(
    text: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<TextBlock>, ParseError> {
    let mut blocks = Vec::new();
    let mut current: Option<TextBlock> = None;
    let mut cursor_utf16 = 0;

    for segment in text.split_inclusive('\n') {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(ParseError::Cancelled);
        }
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line_start = cursor_utf16;
        let line_end = line_start + utf16_len(line);
        if line.trim().is_empty() {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
        } else if is_chapter_title(line) {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            blocks.push(TextBlock {
                text: line.to_owned(),
                start_utf16: line_start,
                end_utf16: line_end,
            });
        } else {
            let block = current.get_or_insert_with(|| TextBlock {
                text: String::new(),
                start_utf16: line_start,
                end_utf16: line_start,
            });
            if !block.text.is_empty() {
                block.text.push('\n');
            }
            block.text.push_str(line);
            block.end_utf16 = line_end;
        }
        cursor_utf16 += utf16_len(segment);
    }
    if let Some(block) = current {
        blocks.push(block);
    }
    Ok(blocks)
}

fn chapter_title_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?i)^(?:第[零〇一二三四五六七八九十百千万两0-9]+[章节回卷](?:\s*[-—:：、.．]?\s*.*)?|chapter\s+\d+(?:\s+.*)?)$")
            .expect("chapter title regex is valid")
    })
}

fn is_chapter_title(text: &str) -> bool {
    let title = text.trim();
    utf16_len(title) <= 50 && chapter_title_pattern().is_match(title)
}

fn add_paragraphs(
    book_id: &str,
    chapter_id: &str,
    blocks: &[TextBlock],
    next_ordinal: &mut usize,
) -> Vec<ParsedParagraph> {
    blocks
        .iter()
        .map(|block| {
            let ordinal = *next_ordinal;
            *next_ordinal += 1;
            ParsedParagraph {
                id: format!("{book_id}:p:{}:v2", block.start_utf16),
                chapter_id: chapter_id.to_owned(),
                ordinal,
                text: block.text.clone(),
                start_utf16: block.start_utf16,
                end_utf16: block.end_utf16,
            }
        })
        .collect()
}

pub fn parse_book(book_id: &str, text: &str) -> Result<ParsedBook, ParseError> {
    parse_book_cancellable(book_id, text, None)
}

pub fn parse_book_cancellable(
    book_id: &str,
    text: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<ParsedBook, ParseError> {
    let normalized = normalize_newlines(text);
    if normalized.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    let blocks = collect_blocks(&normalized, cancelled)?;
    if blocks.is_empty() {
        return Err(ParseError::Empty);
    }

    let total_utf16 = utf16_len(&normalized);
    let digest = Sha256::digest(normalized.as_bytes());
    let normalized_sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let heading_indices = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| is_chapter_title(&block.text).then_some(index))
        .collect::<Vec<_>>();
    let reliable_headings = heading_indices.len() >= 2 && heading_indices.len() * 2 <= blocks.len();
    let mut chapters = Vec::new();
    let mut paragraph_ordinal = 0;

    if !reliable_headings {
        let chapter_id = format!("{book_id}:c:0:v2");
        chapters.push(ParsedChapter {
            id: chapter_id.clone(),
            ordinal: 0,
            title: "正文".into(),
            start_utf16: blocks[0].start_utf16,
            end_utf16: total_utf16,
            paragraphs: add_paragraphs(book_id, &chapter_id, &blocks, &mut paragraph_ordinal),
        });
    } else {
        let first_heading = heading_indices[0];
        if first_heading > 0 {
            let chapter_id = format!("{book_id}:c:0:v2");
            chapters.push(ParsedChapter {
                id: chapter_id.clone(),
                ordinal: 0,
                title: "序章".into(),
                start_utf16: blocks[0].start_utf16,
                end_utf16: blocks[first_heading].start_utf16,
                paragraphs: add_paragraphs(
                    book_id,
                    &chapter_id,
                    &blocks[..first_heading],
                    &mut paragraph_ordinal,
                ),
            });
        }
        for (heading_ordinal, heading_index) in heading_indices.iter().copied().enumerate() {
            let ordinal = chapters.len();
            let chapter_id = format!("{book_id}:c:{ordinal}:v2");
            let next_heading = heading_indices.get(heading_ordinal + 1).copied();
            let body_start = heading_index + 1;
            let body_end = next_heading.unwrap_or(blocks.len());
            let body = &blocks[body_start..body_end];
            chapters.push(ParsedChapter {
                id: chapter_id.clone(),
                ordinal,
                title: blocks[heading_index].text.trim().to_owned(),
                start_utf16: blocks[heading_index].start_utf16,
                end_utf16: next_heading.map_or(total_utf16, |next| blocks[next].start_utf16),
                paragraphs: add_paragraphs(book_id, &chapter_id, body, &mut paragraph_ordinal),
            });
        }
    }

    Ok(ParsedBook {
        normalized_sha256,
        total_utf16,
        chapters,
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_txt, parse_book, ParseError};

    #[test]
    fn decodes_utf8_gbk_and_utf16_files() {
        let utf8 = b"\xEF\xBB\xBF\xE6\x98\xA5\xE5\xA4\xA9";
        assert_eq!(decode_txt(utf8, None).unwrap(), "春天");

        let (gbk, _, had_errors) = encoding_rs::GBK.encode("春天");
        assert!(!had_errors);
        assert_eq!(decode_txt(gbk.as_ref(), Some("gbk")).unwrap(), "春天");

        let utf16 = [0xFF, 0xFE, 0x25, 0x66, 0x29, 0x59];
        assert_eq!(decode_txt(&utf16, None).unwrap(), "春天");
    }

    #[test]
    fn rejects_invalid_manual_encoding_instead_of_importing_replacement_glyphs() {
        assert_eq!(
            decode_txt(&[0xFF, 0xFE, 0x00], Some("utf-16le")),
            Err(ParseError::InvalidEncoding)
        );
    }

    #[test]
    fn normalizes_newlines_and_uses_utf16_code_unit_offsets() {
        let parsed = parse_book("book-a", "甲\r\n\r\n😀乙").unwrap();
        assert_eq!(parsed.total_utf16, 6);
        assert_eq!(parsed.chapters[0].paragraphs[0].text, "甲");
        assert_eq!(parsed.chapters[0].paragraphs[0].start_utf16, 0);
        assert_eq!(parsed.chapters[0].paragraphs[0].end_utf16, 1);
        assert_eq!(parsed.chapters[0].paragraphs[1].text, "😀乙");
        assert_eq!(parsed.chapters[0].paragraphs[1].start_utf16, 3);
        assert_eq!(parsed.chapters[0].paragraphs[1].end_utf16, 6);
    }

    #[test]
    fn recognizes_standalone_chapter_lines_and_preserves_paragraph_order() {
        let parsed = parse_book(
            "book-b",
            "第一章 起风\n\n山里的猫。\n\n第二章 归来\n\n春天来了。",
        )
        .unwrap();
        assert_eq!(parsed.chapters.len(), 2);
        assert_eq!(parsed.chapters[0].title, "第一章 起风");
        assert_eq!(parsed.chapters[1].title, "第二章 归来");
        assert_eq!(parsed.chapters[0].paragraphs[0].text, "山里的猫。");
        assert_eq!(parsed.chapters[1].paragraphs[0].text, "春天来了。");
    }

    #[test]
    fn recognizes_chapter_lines_without_blank_line_separators() {
        let parsed = parse_book(
            "book-no-blank-lines",
            "第一章 起风\n山里的猫。\n第二章 归来\n春天来了。",
        )
        .unwrap();
        assert_eq!(parsed.chapters.len(), 2);
        assert_eq!(parsed.chapters[0].title, "第一章 起风");
        assert_eq!(parsed.chapters[1].title, "第二章 归来");
        assert_eq!(parsed.chapters[0].paragraphs[0].text, "山里的猫。");
        assert_eq!(parsed.chapters[1].paragraphs[0].text, "春天来了。");
    }

    #[test]
    fn uses_one_body_chapter_when_titles_are_not_reliable() {
        let parsed = parse_book(
            "book-c",
            "正文提到第一章，但这里没有独立标题。\n下一段仍是正文。",
        )
        .unwrap();
        assert_eq!(parsed.chapters.len(), 1);
        assert_eq!(parsed.chapters[0].title, "正文");
        assert_eq!(parsed.chapters[0].paragraphs.len(), 1);
        assert!(parsed.chapters[0].paragraphs[0].text.contains("第一章"));
        assert!(parse_book("book-empty", " \r\n\t ").is_err());
    }
}
