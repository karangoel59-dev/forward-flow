//! Chooses which notebook pages accompany a chat message when they cannot all fit.

use std::collections::{HashMap, HashSet};

/// Share of the budget kept for the newest pages, so "what did I just write" still works.
const RECENT_SHARE: usize = 4;
const K1: f64 = 1.2;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "because", "been", "before", "but", "by", "can", "could", "did", "do", "doe", "for", "from",
    "had", "has", "have", "he", "her", "here", "hi", "him", "his", "how", "if", "in", "into", "is", "it",
    "just", "like", "me", "more", "most", "my", "no", "not", "of", "on", "or", "our", "out",
    "over", "she", "should", "so", "some", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "thi", "those", "to", "too", "up", "us", "very", "was", "we", "were",
    "what", "when", "where", "which", "who", "why", "will", "with", "would", "you", "your",
    "tell", "show", "give", "find", "please", "write", "page", "note", "notebook", "entry", "entrie",
];

pub(crate) struct Page {
    pub filename: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// Lowercased words with plural endings trimmed, so "rivers" finds "river".
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| {
            let mut word = word.to_lowercase();
            if word.chars().count() > 3 && word.ends_with('s') && !word.ends_with("ss") {
                word.pop();
            }
            (word.chars().count() >= 2 && !STOPWORDS.contains(&word.as_str())).then_some(word)
        })
        .collect()
}

/// BM25 score of every page for the weighted questions; zero means no shared words.
fn scores(pages: &[Page], questions: &[(&str, f64)]) -> Vec<f64> {
    let mut wanted: HashMap<String, f64> = HashMap::new();
    for (question, weight) in questions {
        for term in terms(question).into_iter().collect::<HashSet<_>>() {
            let current = wanted.entry(term).or_insert(0.0);
            *current = current.max(*weight);
        }
    }
    if wanted.is_empty() || pages.is_empty() {
        return vec![0.0; pages.len()];
    }
    let counted: Vec<(HashMap<String, f64>, f64)> = pages
        .iter()
        .map(|page| {
            let mut counts: HashMap<String, f64> = HashMap::new();
            let words = terms(&page.body);
            let length = words.len() as f64;
            for word in words {
                if wanted.contains_key(&word) {
                    *counts.entry(word).or_insert(0.0) += 1.0;
                }
            }
            // A tag or filename names what the page is about; count it as several mentions.
            for word in terms(&format!("{} {}", page.tags.join(" "), page.filename)) {
                if wanted.contains_key(&word) {
                    *counts.entry(word).or_insert(0.0) += 3.0;
                }
            }
            (counts, length)
        })
        .collect();
    let total = pages.len() as f64;
    let average = (counted.iter().map(|(_, length)| length).sum::<f64>() / total).max(1.0);
    counted
        .iter()
        .map(|(counts, length)| {
            wanted
                .iter()
                .map(|(term, weight)| {
                    let Some(frequency) = counts.get(term) else {
                        return 0.0;
                    };
                    let containing = counted.iter().filter(|(c, _)| c.contains_key(term)).count() as f64;
                    let rarity = ((total - containing + 0.5) / (containing + 0.5) + 1.0).ln();
                    weight * rarity * frequency * (K1 + 1.0)
                        / (frequency + K1 * (1.0 - B + B * length / average))
                })
                .sum()
        })
        .collect()
}

/// Picks pages within `budget` bytes of prose. `pages` arrive newest first and leave in that order.
///
/// Everything is kept when it fits. Otherwise the newest pages take a quarter of the budget, the
/// pages most related to `questions` take the rest, and any room left goes to newer pages.
pub(crate) fn select(pages: Vec<Page>, questions: &[(&str, f64)], budget: usize) -> Vec<Page> {
    if pages.iter().map(|p| p.body.len()).sum::<usize>() <= budget {
        return pages;
    }
    let scores = scores(&pages, questions);
    let mut chosen = vec![false; pages.len()];
    let mut remaining = budget;
    let mut take = |index: usize, limit: usize, remaining: &mut usize| {
        let size = pages[index].body.len();
        if !chosen[index] && size <= limit.min(*remaining) {
            chosen[index] = true;
            *remaining -= size;
        }
    };

    let mut recent = budget / RECENT_SHARE;
    for index in 0..pages.len() {
        let size = pages[index].body.len();
        if size > recent {
            break;
        }
        take(index, recent, &mut remaining);
        recent -= size;
    }
    let mut ranked: Vec<usize> = (0..pages.len()).filter(|&i| scores[i] > 0.0).collect();
    ranked.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    for index in ranked {
        take(index, budget, &mut remaining);
    }
    for index in 0..pages.len() {
        take(index, budget, &mut remaining);
    }

    pages
        .into_iter()
        .zip(chosen)
        .filter_map(|(page, keep)| keep.then_some(page))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(filename: &str, body: &str, tags: &[&str]) -> Page {
        Page {
            filename: filename.into(),
            body: body.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }
    fn names(pages: &[Page]) -> Vec<&str> {
        pages.iter().map(|p| p.filename.as_str()).collect()
    }
    /// Ten pages of 100 bytes, newest first; only the oldest two mention herons.
    fn notebook() -> Vec<Page> {
        (0..10)
            .map(|i| {
                let topic = match i {
                    8 => "A heron stood in the river at dawn. ",
                    9 => "Herons again, three of them by the weir. ",
                    _ => "Groceries, errands and the usual lists. ",
                };
                page(&format!("{i}.md"), &format!("{topic:x<100}"), &[])
            })
            .collect()
    }

    #[test]
    fn a_notebook_that_fits_is_sent_whole_in_its_original_order() {
        let chosen = select(notebook(), &[("herons", 1.0)], 1000);
        assert_eq!(chosen.len(), 10);
        assert_eq!(chosen[0].filename, "0.md");
    }

    #[test]
    fn related_older_pages_displace_unrelated_newer_ones() {
        let chosen = select(notebook(), &[("What did I see about the heron?", 1.0)], 400);
        assert_eq!(names(&chosen), ["0.md", "1.md", "8.md", "9.md"]);
        assert!(chosen.iter().map(|p| p.body.len()).sum::<usize>() <= 400);
    }

    #[test]
    fn a_question_without_matching_words_falls_back_to_the_newest_pages() {
        for question in ["Tell me more about that", "", "quasar"] {
            let chosen = select(notebook(), &[(question, 1.0)], 400);
            assert_eq!(names(&chosen), ["0.md", "1.md", "2.md", "3.md"], "{question}");
        }
    }

    #[test]
    fn tags_count_and_the_previous_question_carries_a_follow_up() {
        let mut pages = notebook();
        pages[7] = page("7.md", &format!("{:x<100}", "Nothing obvious here. "), &["herons"]);
        let chosen = select(pages, &[("And what else?", 1.0), ("herons", 0.5)], 500);
        assert_eq!(names(&chosen), ["0.md", "1.md", "7.md", "8.md", "9.md"]);
    }

    #[test]
    fn a_page_too_large_for_the_remaining_room_is_skipped_not_truncated() {
        let mut pages = notebook();
        pages[8] = page("8.md", &format!("{:x<350}", "heron heron heron "), &[]);
        let chosen = select(pages, &[("heron", 1.0)], 400);
        assert_eq!(names(&chosen), ["0.md", "1.md", "2.md", "9.md"]);
        assert!(chosen.iter().all(|p| p.body.len() == 100));
    }

    #[test]
    fn rarer_words_outrank_common_ones_and_text_in_any_script_is_tokenised() {
        let pages = vec![
            page("a.md", "walk walk walk by the canal", &[]),
            page("b.md", "walk past the Überführung", &[]),
            page("c.md", "walk home", &[]),
        ];
        let found = scores(&pages, &[("walk überführung", 1.0)]);
        assert!(found[1] > found[0] && found[1] > found[2]);
        assert_eq!(terms("The Rivers' glass, über"), ["river", "glass", "über"]);
    }
}
