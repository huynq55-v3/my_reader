use std::path::{Path, PathBuf};
// We no longer use lopdf for page parsing since we use pdftotext, but Cargo.toml maintains it for compatibility.

#[derive(Clone, Debug, Default)]
pub struct PageLayout {
    pub width: f32,
    pub height: f32,
    pub words: Vec<WordBbox>,
}

#[derive(Clone, Debug)]
pub struct WordBbox {
    pub text: String,
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
    pub start_offset: usize, // relative to page text start
    pub end_offset: usize,   // relative to page text start
}

#[derive(Clone, Debug, PartialEq)]
pub enum SegmentStatus {
    Idle,
    Loading,
    Analyzed(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct DocumentSegment {
    pub id: usize,
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub status: SegmentStatus,
    pub is_gap: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReaderState {
    pub file_path: Option<PathBuf>,
    pub file_name: String,
    pub pages: Vec<String>,
    pub current_page: usize, // 0-indexed
    pub segments: Vec<DocumentSegment>,
    pub segmentation_loading: bool,
    pub segmentation_error: Option<String>,
}

impl ReaderState {
    /// Load a document from a path (automatically detects PDF or Text)
    pub fn load_file<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path_ref = path.as_ref();
        let file_name = path_ref
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown File".to_string());

        let ext = path_ref
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let pages = if ext == "pdf" {
            Self::load_pdf(path_ref)?
        } else {
            Self::load_text_file(path_ref)?
        };

        if pages.is_empty() {
            return Err("Tài liệu trống hoặc không thể đọc được nội dung.".to_string());
        }

        let state = Self {
            file_path: Some(path_ref.to_path_buf()),
            file_name,
            pages,
            current_page: 0,
            segments: Vec::new(),
            segmentation_loading: false,
            segmentation_error: None,
        };
        
        // We do not run update_segments automatically here,
        // because the GUI main thread will determine whether to run AI segmentation or local fallback.
        Ok(state)
    }

    /// Load PDF file page by page using pdftotext tool, guaranteeing correct order and robustness
    fn load_pdf(path: &Path) -> Result<Vec<String>, String> {
        let output = std::process::Command::new("pdftotext")
            .arg(path)
            .arg("-")
            .output()
            .map_err(|e| format!("Không thể chạy pdftotext: {}", e))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Lỗi trích xuất văn bản PDF: {}", err_msg));
        }

        let content = String::from_utf8_lossy(&output.stdout);
        
        // Split by form feed (\x0c) which pdftotext inserts between pages
        let mut pages = content
            .split('\x0c')
            .map(|p| p.trim().to_string())
            .collect::<Vec<_>>();

        // Remove trailing empty page if pdftotext added a trailing form feed
        if pages.len() > 1 && pages.last().unwrap().is_empty() {
            pages.pop();
        }

        if pages.is_empty() {
            return Err("Tài liệu trống hoặc không thể đọc được nội dung.".to_string());
        }

        Ok(pages)
    }

    /// Load standard Text file and split into pages
    fn load_text_file(path: &Path) -> Result<Vec<String>, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Không thể đọc file text: {}", e))?;

        // 1. Try splitting by Form Feed character (\x0c) if present
        if content.contains('\x0c') {
            let pages = content
                .split('\x0c')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>();
            if !pages.is_empty() {
                return Ok(pages);
            }
        }

        // 2. Fallback: Group paragraphs by double newlines into pages
        // Each page contains roughly 2000-2500 characters
        let paragraphs = content
            .split("\n\n")
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>();

        let mut pages = Vec::new();
        let mut current_page_text = String::new();

        for para in paragraphs {
            if current_page_text.len() + para.len() > 2500 && !current_page_text.is_empty() {
                pages.push(current_page_text);
                current_page_text = String::new();
            }
            if !current_page_text.is_empty() {
                current_page_text.push_str("\n\n");
            }
            current_page_text.push_str(para);
        }

        if !current_page_text.is_empty() {
            pages.push(current_page_text);
        }

        Ok(pages)
    }

    /// Fallback to local segmentation (splits the current page by double newlines)
    pub fn fallback_local_segmentation(&mut self) {
        self.update_segments();
        self.segmentation_loading = false;
        self.segmentation_error = None;
    }

    fn update_segments(&mut self) {
        if self.pages.is_empty() || self.current_page >= self.pages.len() {
            self.segments = Vec::new();
            return;
        }

        let page_offsets_all = self.get_page_absolute_offsets();
        let page_start = page_offsets_all.get(self.current_page).map(|&(s, _)| s).unwrap_or(0);
        let page_text = &self.pages[self.current_page];
        
        let raw_paragraphs = page_text
            .replace("\r\n", "\n")
            .split("\n\n")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>();

        let mut current_pos = 0;
        let mut segments = Vec::new();
        for (id, text) in raw_paragraphs.into_iter().enumerate() {
            let relative_start = page_text[current_pos..].find(&text).unwrap_or(0);
            let start_offset = page_start + current_pos + relative_start;
            let end_offset = start_offset + text.len();
            current_pos = current_pos + relative_start + text.len();

            segments.push(DocumentSegment {
                id,
                text,
                start_offset,
                end_offset,
                status: SegmentStatus::Idle,
                is_gap: false,
            });
        }
        self.segments = segments;
    }

    /// Retrieve the text of pages [N-1, N, N+1] as Context
    pub fn get_sliding_window_context(&self) -> String {
        if self.pages.is_empty() {
            return String::new();
        }

        let mut context = String::new();
        let current = self.current_page;
        let total = self.pages.len();

        // Page N-1
        if current > 0 {
            context.push_str(&format!("--- TRANG TRƯỚC (TRANG {}) ---\n", current));
            context.push_str(&self.pages[current - 1]);
            context.push_str("\n\n");
        }

        // Page N
        context.push_str(&format!("--- TRANG HIỆN TẠI (TRANG {}) ---\n", current + 1));
        context.push_str(&self.pages[current]);
        context.push_str("\n\n");

        // Page N+1
        if current + 1 < total {
            context.push_str(&format!("--- TRANG SAU (TRANG {}) ---\n", current + 2));
            context.push_str(&self.pages[current + 1]);
            context.push_str("\n");
        }

        context
    }

    /// Calculate the absolute start and end byte offsets of all pages in the document
    pub fn get_page_absolute_offsets(&self) -> Vec<(usize, usize)> {
        let mut offsets = Vec::with_capacity(self.pages.len());
        let mut current = 0;
        for page in &self.pages {
            let start = current;
            let end = current + page.len();
            offsets.push((start, end));
            current = end + 1; // +1 for '\n'
        }
        offsets
    }

    /// Extract a range of text between two absolute offsets, safely respecting character boundaries
    pub fn get_text_range(&self, start: usize, end: usize) -> String {
        let mut result = String::new();
        let page_offsets = self.get_page_absolute_offsets();
        for (i, page) in self.pages.iter().enumerate() {
            let (p_start, p_end) = page_offsets[i];
            if p_start < end && p_end > start {
                let overlap_start = start.max(p_start);
                let overlap_end = end.min(p_end);
                if overlap_start < overlap_end {
                    let mut rel_start = overlap_start - p_start;
                    let mut rel_end = overlap_end - p_start;
                    while rel_start < page.len() && !page.is_char_boundary(rel_start) {
                        rel_start += 1;
                    }
                    while rel_end > 0 && !page.is_char_boundary(rel_end) {
                        rel_end -= 1;
                    }
                    if rel_start < rel_end {
                        let page_slice = &page[rel_start..rel_end];
                        if !result.is_empty() && rel_start == 0 {
                            result.push('\n');
                        }
                        result.push_str(page_slice);
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_splitting() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_doc.txt");
        let content = "Paragraph 1 line 1\nParagraph 1 line 2\n\nParagraph 2 line 1\nParagraph 2 line 2\n\nParagraph 3";
        std::fs::write(&file_path, content).unwrap();

        let mut state = ReaderState::load_file(&file_path).unwrap();
        state.fallback_local_segmentation();
        assert_eq!(state.pages.len(), 1);
        assert_eq!(state.segments.len(), 3);
        assert_eq!(state.segments[0].text, "Paragraph 1 line 1\nParagraph 1 line 2");
        assert_eq!(state.segments[1].text, "Paragraph 2 line 1\nParagraph 2 line 2");
        assert_eq!(state.segments[2].text, "Paragraph 3");

        let context = state.get_sliding_window_context();
        assert!(context.contains("--- TRANG HIỆN TẠI (TRANG 1) ---"));
        assert!(context.contains("Paragraph 1 line 1"));
        
        let _ = std::fs::remove_file(file_path);
    }

    #[test]
    fn test_get_text_range() {
        let state = ReaderState {
            pages: vec![
                "Hello".to_string(),
                "World".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(state.get_text_range(0, 3), "Hel");
        assert_eq!(state.get_text_range(0, 8), "Hello\nWo");

        let state_utf8 = ReaderState {
            pages: vec![
                "Xin–Chào".to_string(),
            ],
            ..Default::default()
        };
        assert_eq!(state_utf8.get_text_range(0, 5), "Xin");
    }

    #[test]
    fn test_load_real_pdf() {
        let path = Path::new("/home/huy/Documents/emperor_new_mind.pdf");
        if path.exists() {
            let state = ReaderState::load_file(path).unwrap();
            assert_eq!(state.pages.len(), 1023);
            assert!(state.pages[9].contains("Mclntyre"));
        }
    }

    #[test]
    fn test_bbox_parsing_real_pdf() {
        let path = Path::new("/home/huy/Documents/emperor_new_mind.pdf");
        if path.exists() {
            let output = std::process::Command::new("pdftotext")
                .arg("-f")
                .arg("10")
                .arg("-l")
                .arg("10")
                .arg("-bbox")
                .arg(path)
                .arg("-")
                .output()
                .unwrap();
            assert!(output.status.success());
            let html = String::from_utf8_lossy(&output.stdout);
            let layout = parse_bbox_html(&html).unwrap();
            assert!(layout.width > 0.0);
            assert!(layout.height > 0.0);
            assert!(!layout.words.is_empty());
            assert!(layout.words.iter().any(|w| w.text.contains("Mclntyre")));
        }
    }

    #[test]
    fn test_assign_word_offsets() {
        let page_text = "OVER THE PAST few decades, electronic computer technology has made enormous strides.";
        let mut words = vec![
            WordBbox { text: "OVER".to_string(), x_min: 0.0, y_min: 0.0, x_max: 10.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
            WordBbox { text: "THE".to_string(), x_min: 15.0, y_min: 0.0, x_max: 25.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
            WordBbox { text: "PAST".to_string(), x_min: 30.0, y_min: 0.0, x_max: 40.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
            WordBbox { text: "few".to_string(), x_min: 45.0, y_min: 0.0, x_max: 55.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
            WordBbox { text: "decades,".to_string(), x_min: 60.0, y_min: 0.0, x_max: 75.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
            WordBbox { text: "electronic".to_string(), x_min: 80.0, y_min: 0.0, x_max: 100.0, y_max: 10.0, start_offset: 0, end_offset: 0 },
        ];

        assign_word_offsets(page_text, &mut words);

        assert_eq!(words[0].start_offset, 0);
        assert_eq!(words[0].end_offset, 4);
        assert_eq!(words[1].start_offset, 5);
        assert_eq!(words[1].end_offset, 8);
        assert_eq!(words[2].start_offset, 9);
        assert_eq!(words[2].end_offset, 13);
        assert_eq!(words[3].start_offset, 14);
        assert_eq!(words[3].end_offset, 17);
        assert_eq!(words[4].start_offset, 18);
        assert_eq!(words[4].end_offset, 26); // "decades," has 8 chars
        assert_eq!(words[5].start_offset, 27);
        assert_eq!(words[5].end_offset, 37);
    }
}

pub fn parse_bbox_html(html: &str) -> Option<PageLayout> {
    // 1. Parse page width and height
    // E.g. <page width="612.000000" height="792.000000">
    let page_re = regex::Regex::new(r#"<page\s+width="([\d.]+)"\s+height="([\d.]+)""#).ok()?;
    let caps = page_re.captures(html)?;
    let width = caps.get(1)?.as_str().parse::<f32>().ok()?;
    let height = caps.get(2)?.as_str().parse::<f32>().ok()?;

    // 2. Parse words
    // E.g. <word xMin="280.847156" yMin="493.555644" xMax="344.392076" yMax="518.091775">Mclntyre,</word>
    let word_re = regex::Regex::new(
        r#"<word\s+xMin="([\d.]+)"\s+yMin="([\d.]+)"\s+xMax="([\d.]+)"\s+yMax="([\d.]+)">([^<]*)</word>"#
    ).ok()?;

    let mut words = Vec::new();
    for caps in word_re.captures_iter(html) {
        let x_min = caps.get(1).unwrap().as_str().parse::<f32>().unwrap_or(0.0);
        let y_min = caps.get(2).unwrap().as_str().parse::<f32>().unwrap_or(0.0);
        let x_max = caps.get(3).unwrap().as_str().parse::<f32>().unwrap_or(0.0);
        let y_max = caps.get(4).unwrap().as_str().parse::<f32>().unwrap_or(0.0);
        let text = caps.get(5).unwrap().as_str().to_string();
        
        words.push(WordBbox {
            text,
            x_min,
            y_min,
            x_max,
            y_max,
            start_offset: 0,
            end_offset: 0,
        });
    }

    Some(PageLayout {
        width,
        height,
        words,
    })
}

pub fn assign_word_offsets(page_text: &str, words: &mut [WordBbox]) {
    let mut current_pos = 0;
    let text_len = page_text.len();

    // Collect all char boundary byte positions in page_text
    let char_boundaries: Vec<usize> = page_text
        .char_indices()
        .map(|(idx, _)| idx)
        .chain(std::iter::once(text_len))
        .collect();

    for word in words {
        let word_len = word.text.len();
        if word_len == 0 {
            word.start_offset = current_pos;
            word.end_offset = current_pos;
            continue;
        }

        // Find character index of current_pos
        let char_idx = match char_boundaries.binary_search(&current_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.min(char_boundaries.len() - 1),
        };

        // Search in a character-boundary-safe window to avoid skipping ahead too far
        let window_len_chars = 200;
        let limit_char_idx = (char_idx + window_len_chars).min(char_boundaries.len() - 1);
        let search_limit = char_boundaries[limit_char_idx];
        
        let search_window = &page_text[current_pos..search_limit];

        // 1. Try exact match in the search window
        if let Some(pos) = search_window.find(&word.text) {
            let start = current_pos + pos;
            word.start_offset = start;
            word.end_offset = start + word_len;
            current_pos = word.end_offset;
            continue;
        }

        // 2. Try case-insensitive/normalized token match in search window
        let norm_word = normalize_word(&word.text);
        if !norm_word.is_empty() {
            let mut found = false;
            let mut match_start_byte = 0;
            let mut match_end_byte = 0;

            // Iterate over character indices of search_window to slice safely
            for (i, _) in search_window.char_indices() {
                let mut norm_sub = String::new();
                for (sub_idx, ch) in search_window[i..].char_indices() {
                    if ch.is_alphanumeric() {
                        norm_sub.push_str(&ch.to_lowercase().to_string());
                    }
                    if norm_sub == norm_word {
                        found = true;
                        match_start_byte = current_pos + i;
                        match_end_byte = current_pos + i + sub_idx + ch.len_utf8();
                        break;
                    }
                    if norm_sub.len() > norm_word.len() {
                        break;
                    }
                }
                if found {
                    break;
                }
            }

            if found {
                word.start_offset = match_start_byte;
                word.end_offset = match_end_byte;
                current_pos = match_end_byte;
                continue;
            }
        }

        // 3. Fallback to full search of the remaining page
        let search_window_full = &page_text[current_pos..];
        if let Some(pos) = search_window_full.find(&word.text) {
            let start = current_pos + pos;
            word.start_offset = start;
            word.end_offset = start + word_len;
            current_pos = word.end_offset;
            continue;
        }

        // 4. Ultimate fallback (safely aligned to a character boundary)
        let target_byte = current_pos + word_len;
        let boundary_idx = match char_boundaries.binary_search(&target_byte) {
            Ok(idx) => idx,
            Err(idx) => idx.min(char_boundaries.len() - 1),
        };
        let end_boundary = char_boundaries[boundary_idx];
        word.start_offset = current_pos;
        word.end_offset = end_boundary;
        current_pos = end_boundary;
    }
}

fn normalize_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

#[allow(dead_code)]
pub fn find_segment_words(
    page_words: &[WordBbox],
    segment_text: &str,
) -> Vec<usize> {
    let segment_words: Vec<String> = segment_text
        .split_whitespace()
        .map(normalize_word)
        .filter(|w| !w.is_empty())
        .collect();

    if segment_words.is_empty() {
        return Vec::new();
    }

    let page_words_normalized: Vec<String> = page_words
        .iter()
        .map(|w| normalize_word(&w.text))
        .collect();

    let mut matched_indices = Vec::new();
    let n = page_words_normalized.len();
    let m = segment_words.len();
    
    if n < m {
        return Vec::new();
    }

    let mut best_start = 0;
    let mut best_count = 0;

    for i in 0..=(n - m) {
        let mut count = 0;
        for j in 0..m {
            if page_words_normalized[i + j] == segment_words[j] {
                count += 1;
            }
        }
        if count > best_count {
            best_count = count;
            best_start = i;
        }
        if count == m {
            for k in 0..m {
                matched_indices.push(i + k);
            }
            return matched_indices;
        }
    }

    if best_count > m / 2 {
        for k in 0..m {
            matched_indices.push(best_start + k);
        }
    }

    matched_indices
}
