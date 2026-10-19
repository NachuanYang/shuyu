use crate::models::{AiQuestionContext, AiQuestionParagraph};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const MAX_CURRENT_CHAPTER_CHARACTERS: usize = 40_000;
pub const MAX_RELATED_CONTEXT_CHARACTERS: usize = 24_000;
pub const MAX_RELATED_PASSAGES: usize = 48;
pub const MAX_TOTAL_CONTEXT_CHARACTERS: usize = 64_000;
const MAX_SELECTION_CHARACTERS: usize = 16_384;
const MAX_QUESTION_CHARACTERS: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiContextPassage {
    pub source_id: String,
    pub chapter_ordinal: usize,
    pub paragraph_ordinal: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiQuestionContextPreview {
    pub preview_digest: String,
    pub chapter_title: String,
    pub chapter_ordinal: usize,
    pub reading_position_utf16: usize,
    pub selected_text: String,
    pub question: String,
    pub current_chapter: Vec<AiContextPassage>,
    pub related_passages: Vec<AiContextPassage>,
    pub current_chapter_characters: usize,
    pub related_characters: usize,
    pub total_characters: usize,
}

pub fn prepare_context(
    context: &AiQuestionContext,
    selected_text: &str,
    question: &str,
) -> Result<AiQuestionContextPreview, String> {
    let selected_text = selected_text.trim();
    if selected_text.is_empty() {
        return Err("请选择需要解释的原文。".into());
    }
    let selected_characters = selected_text.chars().count();
    if selected_characters > MAX_SELECTION_CHARACTERS {
        return Err("选段过长，请缩短到16,384个字符以内。".into());
    }
    let question = question.trim();
    if question.chars().count() > MAX_QUESTION_CHARACTERS {
        return Err("问题不能超过1,000个字符。".into());
    }
    let question = if question.is_empty() {
        "请解释这段原文的含义。"
    } else {
        question
    };
    let current_chapter_characters = context
        .current_chapter
        .iter()
        .map(|paragraph| paragraph.text.chars().count())
        .sum::<usize>();
    if current_chapter_characters > MAX_CURRENT_CHAPTER_CHARACTERS {
        return Err(format!(
            "当前章节已读内容共{}个字符，超过{}字符的上下文安全上限；未发送任何内容。",
            current_chapter_characters, MAX_CURRENT_CHAPTER_CHARACTERS
        ));
    }
    let question_characters = question.chars().count();
    let mandatory_characters = selected_characters
        .saturating_add(current_chapter_characters)
        .saturating_add(question_characters);
    if mandatory_characters > MAX_TOTAL_CONTEXT_CHARACTERS {
        return Err(format!(
            "选段、问题与当前章节已读内容共{}个字符，超过{}字符的总上下文安全上限；未发送任何内容。",
            mandatory_characters, MAX_TOTAL_CONTEXT_CHARACTERS
        ));
    }
    let current_chapter = context
        .current_chapter
        .iter()
        .map(|paragraph| passage(paragraph))
        .collect::<Vec<_>>();
    let related_budget = MAX_RELATED_CONTEXT_CHARACTERS
        .min(MAX_TOTAL_CONTEXT_CHARACTERS.saturating_sub(mandatory_characters));
    let related_passages = rank_related_passages(
        &context.earlier_book,
        selected_text,
        question,
        related_budget,
    );
    let related_characters = related_passages
        .iter()
        .map(|passage| passage.text.chars().count())
        .sum::<usize>();
    let total_characters = mandatory_characters.saturating_add(related_characters);
    Ok(AiQuestionContextPreview {
        preview_digest: String::new(),
        chapter_title: context.chapter_title.clone(),
        chapter_ordinal: context.chapter_ordinal,
        reading_position_utf16: context.reading_position_utf16,
        selected_text: selected_text.to_owned(),
        question: question.to_owned(),
        current_chapter,
        related_passages,
        current_chapter_characters,
        related_characters,
        total_characters,
    })
}

pub fn context_digest(
    preview: &AiQuestionContextPreview,
    book_id: &str,
    chapter_id: &str,
    start_paragraph_id: &str,
    start_utf16: usize,
    end_paragraph_id: &str,
    end_utf16: usize,
    model_slug: &str,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "book_id": book_id,
        "chapter_id": chapter_id,
        "start_paragraph_id": start_paragraph_id,
        "start_utf16": start_utf16,
        "end_paragraph_id": end_paragraph_id,
        "end_utf16": end_utf16,
        "model_slug": model_slug,
        "chapter_ordinal": preview.chapter_ordinal,
        "reading_position_utf16": preview.reading_position_utf16,
        "selected_text": preview.selected_text,
        "question": preview.question,
        "current_chapter": preview.current_chapter,
        "related_passages": preview.related_passages,
    });
    let bytes = serde_json::to_vec(&payload)
        .map_err(|_| "上下文预览摘要生成失败；未发送任何内容。".to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn rank_related_passages(
    candidates: &[AiQuestionParagraph],
    selected_text: &str,
    question: &str,
    max_characters: usize,
) -> Vec<AiContextPassage> {
    if candidates.is_empty() || max_characters == 0 {
        return Vec::new();
    }
    let mut query_weights = HashMap::<String, f64>::new();
    for term in extract_terms(question) {
        *query_weights.entry(term).or_default() += 2.0;
    }
    for term in extract_terms(selected_text) {
        *query_weights.entry(term).or_default() += 1.0;
    }
    if query_weights.is_empty() {
        return Vec::new();
    }

    let documents = candidates
        .iter()
        .map(|paragraph| extract_terms(&paragraph.text))
        .collect::<Vec<_>>();
    let document_lengths = documents.iter().map(Vec::len).collect::<Vec<_>>();
    let average_length =
        document_lengths.iter().sum::<usize>() as f64 / document_lengths.len().max(1) as f64;
    let mut document_frequency = HashMap::<String, usize>::new();
    for terms in &documents {
        let unique_terms = terms.iter().collect::<HashSet<_>>();
        for term in unique_terms {
            *document_frequency.entry(term.clone()).or_default() += 1;
        }
    }
    let count = candidates.len() as f64;
    let mut ranked = candidates
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let frequencies = documents[index].iter().fold(
                HashMap::<&str, usize>::new(),
                |mut frequencies, term| {
                    *frequencies.entry(term).or_default() += 1;
                    frequencies
                },
            );
            let length = document_lengths[index] as f64;
            let score = query_weights.iter().fold(0.0, |score, (term, weight)| {
                let frequency = frequencies.get(term.as_str()).copied().unwrap_or(0) as f64;
                if frequency == 0.0 {
                    return score;
                }
                let df = document_frequency.get(term).copied().unwrap_or(0) as f64;
                let inverse_frequency = (1.0 + (count - df + 0.5) / (df + 0.5)).ln();
                let denominator =
                    frequency + 1.2 * (1.0 - 0.75 + 0.75 * length / average_length.max(1.0));
                score + weight * inverse_frequency * frequency * 2.2 / denominator
            });
            (index, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_index, left_score), (right_index, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| {
                candidates[*left_index]
                    .start_utf16
                    .cmp(&candidates[*right_index].start_utf16)
            })
            .then_with(|| {
                candidates[*left_index]
                    .paragraph_ordinal
                    .cmp(&candidates[*right_index].paragraph_ordinal)
            })
    });

    let mut selected_indices = Vec::new();
    let mut selected_texts = HashSet::new();
    let mut used_characters = 0usize;
    for (match_index, _) in ranked {
        let mut related_indices = Vec::with_capacity(3);
        related_indices.push(match_index);
        if match_index > 0 {
            let previous = match_index - 1;
            if candidates[previous].chapter_ordinal == candidates[match_index].chapter_ordinal
                && candidates[previous].paragraph_ordinal.saturating_add(1)
                    == candidates[match_index].paragraph_ordinal
            {
                related_indices.push(previous);
            }
        }
        let next = match_index + 1;
        if next < candidates.len()
            && candidates[next].chapter_ordinal == candidates[match_index].chapter_ordinal
            && candidates[next].paragraph_ordinal
                == candidates[match_index].paragraph_ordinal.saturating_add(1)
        {
            related_indices.push(next);
        }

        for index in related_indices {
            if selected_indices.len() >= MAX_RELATED_PASSAGES {
                break;
            }
            if selected_indices.contains(&index) {
                continue;
            }
            let paragraph = &candidates[index];
            let characters = paragraph.text.chars().count();
            if characters == 0 || used_characters.saturating_add(characters) > max_characters {
                continue;
            }
            let normalized = paragraph.text.split_whitespace().collect::<String>();
            if !selected_texts.insert(normalized) {
                continue;
            }
            used_characters += characters;
            selected_indices.push(index);
        }
        if selected_indices.len() >= MAX_RELATED_PASSAGES {
            break;
        }
    }
    selected_indices.sort_by_key(|index| {
        (
            candidates[*index].chapter_ordinal,
            candidates[*index].paragraph_ordinal,
        )
    });
    selected_indices
        .into_iter()
        .map(|index| passage(&candidates[index]))
        .collect()
}

fn passage(paragraph: &AiQuestionParagraph) -> AiContextPassage {
    AiContextPassage {
        source_id: format!(
            "C{}-P{}",
            paragraph.chapter_ordinal + 1,
            paragraph.paragraph_ordinal + 1
        ),
        chapter_ordinal: paragraph.chapter_ordinal,
        paragraph_ordinal: paragraph.paragraph_ordinal,
        text: paragraph.text.clone(),
    }
}

fn extract_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut cjk_run = Vec::new();
    let mut latin_run = String::new();
    let flush_cjk = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.len() == 1 {
            terms.push(run[0].to_string());
        } else {
            terms.extend(
                run.windows(2)
                    .map(|window| window.iter().collect::<String>()),
            );
        }
        run.clear();
    };
    let flush_latin = |run: &mut String, terms: &mut Vec<String>| {
        if !run.is_empty() {
            terms.push(std::mem::take(run));
        }
    };
    for character in text.chars() {
        if is_cjk(character) {
            flush_latin(&mut latin_run, &mut terms);
            cjk_run.push(character);
        } else {
            flush_cjk(&mut cjk_run, &mut terms);
            if character.is_ascii_alphanumeric() {
                latin_run.push(character.to_ascii_lowercase());
            } else {
                flush_latin(&mut latin_run, &mut terms);
            }
        }
    }
    flush_cjk(&mut cjk_run, &mut terms);
    flush_latin(&mut latin_run, &mut terms);
    terms
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    )
}

#[cfg(test)]
mod tests {
    use crate::models::{AiQuestionContext, AiQuestionParagraph};

    fn paragraph(chapter: usize, ordinal: usize, start: usize, text: &str) -> AiQuestionParagraph {
        AiQuestionParagraph {
            chapter_ordinal: chapter,
            paragraph_ordinal: ordinal,
            start_utf16: start,
            end_utf16: start + text.encode_utf16().count(),
            text: text.into(),
        }
    }

    fn context() -> AiQuestionContext {
        AiQuestionContext {
            chapter_id: "current".into(),
            chapter_title: "当前章".into(),
            chapter_ordinal: 3,
            reading_position_utf16: 500,
            current_chapter: vec![paragraph(
                3,
                21,
                400,
                "CURRENT_CHAPTER_MARKER 她把旧信收了起来。",
            )],
            earlier_book: vec![
                paragraph(0, 2, 10, "RELATED_MATCH_MARKER 她一直把信留在身边。"),
                paragraph(1, 8, 120, "无关的人正在讨论天气。"),
                paragraph(2, 15, 250, "另一个无关人物买了一把伞。"),
            ],
        }
    }

    #[test]
    fn prepared_context_keeps_the_whole_read_chapter_and_ranks_related_book_passages() {
        let prepared =
            super::prepare_context(&context(), "她把旧信收了起来。", "她为什么一直留着信？")
                .unwrap();

        assert_eq!(prepared.current_chapter.len(), 1);
        assert!(prepared.current_chapter[0]
            .text
            .contains("CURRENT_CHAPTER_MARKER"));
        assert_eq!(prepared.related_passages.len(), 1);
        assert!(prepared.related_passages[0]
            .text
            .contains("RELATED_MATCH_MARKER"));
        assert!(prepared.total_characters > prepared.current_chapter_characters);
    }

    #[test]
    fn historical_question_includes_neighboring_narrative_around_a_character_match() {
        let mut context = context();
        context.earlier_book = vec![
            paragraph(0, 0, 0, "无关段落写着窗外的雨一直没有停。"),
            paragraph(0, 1, 40, "RELATED_MATCH 陈进把项目资料放在桌上。"),
            paragraph(0, 2, 80, "我和他第一次见面，是在公司楼下的小餐馆。"),
            paragraph(0, 3, 120, "那天我们谈到了同一位朋友。"),
            paragraph(1, 0, 150, "另一章里陈进又提到了这件事。"),
        ];

        let prepared = super::prepare_context(
            &context,
            "我们后来一直保持联系。",
            "陈进和我最早是怎么认识的？",
        )
        .unwrap();

        let included = prepared
            .related_passages
            .iter()
            .map(|passage| passage.text.as_str())
            .collect::<Vec<_>>();
        assert!(included.iter().any(|text| text.contains("RELATED_MATCH")));
        assert!(included.iter().any(|text| text.contains("第一次见面")));
        assert!(prepared.related_passages.windows(2).all(|pair| (
            pair[0].chapter_ordinal,
            pair[0].paragraph_ordinal
        ) <= (
            pair[1].chapter_ordinal,
            pair[1].paragraph_ordinal
        )));
    }

    #[test]
    fn oversized_current_chapter_is_rejected_without_truncation() {
        let mut oversized = context();
        oversized.current_chapter = vec![paragraph(3, 21, 400, &"长".repeat(40_001))];

        let error = super::prepare_context(&oversized, "选段", "问题").unwrap_err();

        assert!(error.contains("当前章节已读内容"));
        assert!(error.contains("未发送"));
    }

    #[test]
    fn preview_digest_is_bound_to_the_selected_source_range_and_model() {
        let preview =
            super::prepare_context(&context(), "她把旧信收了起来。", "她为什么留着信？").unwrap();
        let digest = super::context_digest(
            &preview,
            "book",
            "current",
            "paragraph-a",
            2,
            "paragraph-a",
            12,
            "gpt-model",
        )
        .unwrap();
        let changed_range = super::context_digest(
            &preview,
            "book",
            "current",
            "paragraph-b",
            2,
            "paragraph-b",
            12,
            "gpt-model",
        )
        .unwrap();
        let changed_model = super::context_digest(
            &preview,
            "book",
            "current",
            "paragraph-a",
            2,
            "paragraph-a",
            12,
            "another-model",
        )
        .unwrap();

        assert_ne!(digest, changed_range);
        assert_ne!(digest, changed_model);
    }
}
