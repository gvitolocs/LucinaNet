/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The find bar's page text, from the engine to the window: the fork's `PageText` becomes the blocks
//! `lucinanet-find` searches, goes out as one [`News::PageText`], and is kept nowhere here. The one log line says how
//! many blocks and runs there were and whether the limit cut the text short - counts, never text.

use lucinanet_find::{Block, Run};
use lucinanet_ui::News;
use servo::PageText;

/// The most page text one request takes: the byte limit handed to `WebView::request_page_text`, counted over the runs.
pub const LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// The blocks of `page` and whether the limit cut it short. The node ids are the engine's own 64-bit ones, kept whole.
pub fn to_blocks(page: PageText) -> (Vec<Block>, bool) {
    let blocks = page
        .blocks
        .into_iter()
        .map(|block| Block { runs: block.runs.into_iter().map(|run| Run { node: run.node, text: run.text }).collect() })
        .collect();
    (blocks, page.partial)
}

/// Answer request `id` with `page`: one [`News::PageText`] through `news`, one counts-only line through `log`.
pub fn answer(id: u64, page: PageText, news: &dyn Fn(News), log: &mut dyn FnMut(String)) {
    let (blocks, partial) = to_blocks(page);
    let runs: usize = blocks.iter().map(|block| block.runs.len()).sum();
    log(format!("lucinanet: page text for find: blocks={} runs={runs} partial={partial}", blocks.len()));
    news(News::PageText { id, blocks, partial });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use servo::{TextBlock, TextRun};

    use super::*;

    fn page(blocks: &[&[(u64, &str)]], partial: bool) -> PageText {
        PageText {
            blocks: blocks
                .iter()
                .map(|runs| TextBlock { runs: runs.iter().map(|(node, text)| TextRun { node: *node, text: (*text).to_string() }).collect() })
                .collect(),
            partial,
        }
    }

    /// Blocks and runs keep their order and their text, the 64-bit node id is kept whole, and `partial` comes through.
    #[test]
    fn the_fork_text_becomes_find_blocks() {
        let wide = u64::from(u32::MAX) + 5;
        let (blocks, partial) = to_blocks(page(&[&[(1, "Hello "), (wide, "world")], &[(3, "second")]], true));
        assert!(partial, "the limit's cut is reported");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].runs.len(), 2);
        assert_eq!((blocks[0].runs[0].node, blocks[0].runs[0].text.as_str()), (1, "Hello "));
        assert_eq!((blocks[0].runs[1].node, blocks[0].runs[1].text.as_str()), (wide, "world"));
        assert_eq!(blocks[1].runs[0].text, "second");
        let (none, partial) = to_blocks(PageText::default());
        assert!(none.is_empty() && !partial, "a page that went away is no blocks");
    }

    /// The answer goes out as the one news with the request's id, and the log, whatever else it prints, never holds the
    /// page's text: a marker in the text is in no log line and no `Debug` of the news that went out.
    #[test]
    fn the_answer_goes_out_once_and_no_log_line_carries_the_text() {
        let marker = "ZQX-marker-4417-never-logged";
        let got: RefCell<Vec<News>> = RefCell::new(Vec::new());
        let mut lines: Vec<String> = Vec::new();
        answer(7, page(&[&[(1, marker), (2, "tail")]], false), &|n| got.borrow_mut().push(n), &mut |l| lines.push(l));
        let got = got.into_inner();
        assert_eq!(got.len(), 1, "one answer");
        match &got[0] {
            News::PageText { id, blocks, partial } => {
                assert_eq!(*id, 7);
                assert!(!partial);
                assert_eq!(blocks[0].runs[0].text, marker, "the text did reach the window");
            },
            other => panic!("{other:?}"),
        }
        assert!(!format!("{:?}", got[0]).contains(marker), "the news' Debug carries no text");
        assert_eq!(lines.len(), 1, "one line: {lines:?}");
        assert!(lines.iter().all(|l| !l.contains(marker) && !l.contains("tail")), "page text in a log line: {lines:?}");
        assert!(lines[0].contains("blocks=1") && lines[0].contains("runs=2") && lines[0].contains("partial=false"), "{lines:?}");
    }
}
