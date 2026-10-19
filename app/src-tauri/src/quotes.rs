use crate::models::{DailyQuote, DailyQuoteCandidate, QuoteSourceParagraph};

const MAX_CANDIDATES: usize = 30;
const MIN_CANDIDATE_UTF16: usize = 8;
const MAX_CANDIDATE_UTF16: usize = 160;

pub fn build_daily_quote_candidates(
    paragraphs: &[QuoteSourceParagraph],
) -> Vec<DailyQuoteCandidate> {
    let sentences = paragraphs
        .iter()
        .map(|paragraph| {
            sentence_spans(&paragraph.text)
                .into_iter()
                .filter_map(|(start, end)| {
                    let text = paragraph.text.get(start..end)?.trim();
                    let leading_bytes = paragraph.text.get(start..end)?.len()
                        - paragraph.text.get(start..end)?.trim_start().len();
                    let start_byte = start + leading_bytes;
                    let start_utf16 = paragraph.text.get(..start_byte)?.encode_utf16().count();
                    let length_utf16 = text.encode_utf16().count();
                    if !(MIN_CANDIDATE_UTF16..=MAX_CANDIDATE_UTF16).contains(&length_utf16) {
                        return None;
                    }
                    Some(DailyQuoteCandidate {
                        candidate_id: String::new(),
                        book_id: paragraph.book_id.clone(),
                        chapter_id: paragraph.chapter_id.clone(),
                        paragraph_id: paragraph.paragraph_id.clone(),
                        start_intra_utf16: start_utf16,
                        end_intra_utf16: start_utf16 + length_utf16,
                        text: text.to_owned(),
                        source_hash: paragraph.source_hash.clone(),
                        source_local_date: paragraph.source_local_date.clone(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut candidates = Vec::new();
    for sentence_index in 0.. {
        let mut found = false;
        for paragraph_sentences in &sentences {
            if let Some(sentence) = paragraph_sentences.get(sentence_index) {
                candidates.push(sentence.clone());
                found = true;
                if candidates.len() == MAX_CANDIDATES {
                    break;
                }
            }
        }
        if !found || candidates.len() == MAX_CANDIDATES {
            break;
        }
    }
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.candidate_id = format!("Q{:03}", index + 1);
    }
    candidates
}

pub fn candidates_for_quote_refresh(
    candidates: &[DailyQuoteCandidate],
    current_quote: Option<&DailyQuote>,
) -> Vec<DailyQuoteCandidate> {
    let Some(current_quote) = current_quote else {
        return candidates.to_vec();
    };
    let alternatives = candidates
        .iter()
        .filter(|candidate| !matches_daily_quote(candidate, current_quote))
        .cloned()
        .collect::<Vec<_>>();
    if alternatives.is_empty() {
        candidates.to_vec()
    } else {
        alternatives
    }
}

fn matches_daily_quote(candidate: &DailyQuoteCandidate, quote: &DailyQuote) -> bool {
    candidate.book_id == quote.book_id
        && candidate.chapter_id == quote.chapter_id
        && candidate.paragraph_id == quote.paragraph_id
        && candidate.start_intra_utf16 == quote.start_intra_utf16
        && candidate.end_intra_utf16 == quote.end_intra_utf16
        && candidate.text == quote.text_snapshot
}

fn sentence_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut in_terminal = false;
    for (byte, character) in text.char_indices() {
        if in_terminal && !is_closing_quote(character) && !is_terminal(character) {
            if !text[start..byte].trim().is_empty() {
                spans.push((start, byte));
            }
            start = byte;
            in_terminal = false;
        }
        if is_terminal(character) {
            in_terminal = true;
        }
    }
    if start < text.len() && !text[start..].trim().is_empty() {
        spans.push((start, text.len()));
    }
    spans
}

fn is_terminal(character: char) -> bool {
    matches!(character, '。' | '！' | '？' | '.' | '!' | '?')
}

fn is_closing_quote(character: char) -> bool {
    matches!(
        character,
        '”' | '’' | '」' | '』' | '》' | '】' | ')' | '）'
    )
}

#[cfg(test)]
mod tests {
    use crate::models::{DailyQuote, DailyQuoteCandidate, QuoteSourceParagraph};

    fn candidate(id: &str, text: &str, start: usize) -> DailyQuoteCandidate {
        DailyQuoteCandidate {
            candidate_id: id.into(),
            book_id: "book-1".into(),
            chapter_id: "chapter-1".into(),
            paragraph_id: "paragraph-1".into(),
            start_intra_utf16: start,
            end_intra_utf16: start + text.encode_utf16().count(),
            text: text.into(),
            source_hash: "hash-1".into(),
            source_local_date: "2026-10-08".into(),
        }
    }

    fn quote_from_candidate(candidate: &DailyQuoteCandidate) -> DailyQuote {
        DailyQuote {
            local_date: "2026-10-09".into(),
            book_id: candidate.book_id.clone(),
            book_title: "book title".into(),
            author: "book author".into(),
            chapter_id: candidate.chapter_id.clone(),
            chapter_title: "chapter title".into(),
            paragraph_id: candidate.paragraph_id.clone(),
            start_intra_utf16: candidate.start_intra_utf16,
            end_intra_utf16: candidate.end_intra_utf16,
            text_snapshot: candidate.text.clone(),
            source_hash: candidate.source_hash.clone(),
            created_at: "2026-10-09T00:00:00Z".into(),
            source_local_date: candidate.source_local_date.clone(),
            selection_method: "ai".into(),
        }
    }

    #[test]
    fn extracts_exact_read_sentences_with_utf16_offsets() {
        let paragraph = QuoteSourceParagraph {
            book_id: "book-1".into(),
            chapter_id: "chapter-1".into(),
            paragraph_id: "paragraph-1".into(),
            text: "前文很短。星河😀落在掌心，沉默也有自己的回声。后来我们继续赶路！".into(),
            source_hash: "hash".into(),
            source_local_date: "2026-10-07".into(),
            dwell_ms: 5000,
        };

        let candidates = super::build_daily_quote_candidates(&[paragraph]);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].candidate_id, "Q001");
        assert_eq!(candidates[0].text, "星河😀落在掌心，沉默也有自己的回声。");
        assert_eq!(
            candidates[0].start_intra_utf16,
            "前文很短。".encode_utf16().count()
        );
        assert_eq!(
            candidates[0].end_intra_utf16 - candidates[0].start_intra_utf16,
            candidates[0].text.encode_utf16().count()
        );
        assert_eq!(candidates[1].text, "后来我们继续赶路！");
    }

    #[test]
    fn limits_candidates_and_only_uses_paragraphs_that_were_read() {
        let paragraphs = (0..40)
            .map(|index| QuoteSourceParagraph {
                book_id: format!("book-{index}"),
                chapter_id: format!("chapter-{index}"),
                paragraph_id: format!("paragraph-{index}"),
                text: "这是一句有代表性且值得留存的阅读原文。另一句同样值得认真思考。".into(),
                source_hash: format!("hash-{index}"),
                source_local_date: "2026-10-07".into(),
                dwell_ms: 5000,
            })
            .collect::<Vec<_>>();

        let candidates = super::build_daily_quote_candidates(&paragraphs);

        assert_eq!(candidates.len(), 30);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.text.len() <= 480));
    }

    #[test]
    fn quote_refresh_skips_the_current_candidate_when_an_alternative_exists() {
        let current = candidate("Q001", "第一句有意义值得留下。", 0);
        let alternative = candidate("Q002", "第二句也值得再次回味。", 14);
        let current_quote = quote_from_candidate(&current);

        let refreshed = super::candidates_for_quote_refresh(
            &[current, alternative.clone()],
            Some(&current_quote),
        );

        assert_eq!(refreshed, vec![alternative]);
    }

    #[test]
    fn quote_refresh_keeps_the_current_candidate_when_it_is_the_only_option() {
        let current = candidate("Q001", "唯一一句也值得留下。", 0);
        let current_quote = quote_from_candidate(&current);

        let refreshed =
            super::candidates_for_quote_refresh(&[current.clone()], Some(&current_quote));

        assert_eq!(refreshed, vec![current]);
    }
}
