use std::path::{Path, PathBuf};
use lopdf::Document as PdfDocument;

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

    /// Load PDF file page by page using lopdf, guaranteeing correct order of pages
    fn load_pdf(path: &Path) -> Result<Vec<String>, String> {
        let doc = PdfDocument::load(path)
            .map_err(|e| format!("Không thể mở file PDF: {}", e))?;

        let pages_dict = doc.get_pages();
        let mut page_numbers: Vec<u32> = pages_dict.keys().cloned().collect();
        page_numbers.sort();

        let mut pages = Vec::with_capacity(page_numbers.len());

        for page_num in page_numbers {
            // lopdf extract_text takes 1-based page numbers
            match doc.extract_text(&[page_num]) {
                Ok(text) => {
                    pages.push(text);
                }
                Err(_) => {
                    pages.push(format!("[Lỗi trích xuất trang {}]", page_num));
                }
            }
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
}
