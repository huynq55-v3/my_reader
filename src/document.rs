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
        });
    }

    Some(PageLayout {
        width,
        height,
        words,
    })
}

fn normalize_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

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
