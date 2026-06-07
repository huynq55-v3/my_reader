use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedSegment {
    pub text: String,
    pub start_offset: usize, // absolute character/byte offset in document
    pub end_offset: usize,   // absolute character/byte offset in document
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DocumentCache {
    pub file_path: String,
    pub segmented_pages: Vec<usize>,  // 0-indexed page numbers
    pub segments: Vec<CachedSegment>, // Sorted by start_offset
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppCache {
    pub documents: HashMap<String, DocumentCache>,
}

impl AppCache {
    /// Get the path to the segment cache file: ~/.config/my_reader/segment_cache.json
    pub fn cache_path() -> Option<PathBuf> {
        let base_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .ok()
            })?;
        Some(base_dir.join("my_reader").join("segment_cache.json"))
    }

    /// Load the cache from disk
    pub fn load() -> Self {
        if let Some(path) = Self::cache_path() {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(cache) = serde_json::from_str::<Self>(&content) {
                        return cache;
                    }
                }
            }
        }
        Self::default()
    }

    /// Save the cache to disk
    pub fn save(&self) -> Result<(), String> {
        let path = Self::cache_path()
            .ok_or_else(|| "Không xác định được thư mục để lưu cache.".to_string())?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Không thể tạo thư mục lưu cache: {}", e))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Lỗi serialization cache: {}", e))?;

        std::fs::write(path, json)
            .map_err(|e| format!("Lỗi ghi file cache: {}", e))?;

        Ok(())
    }

    /// Update segments for a range of pages in a document.
    /// Only target_page is marked as fully segmented.
    /// Removes existing segments in [window_start, window_end] to prevent duplicate overlaps.
    pub fn update_segments(
        &mut self,
        file_path: &str,
        target_page: usize,
        window_start: usize,
        window_end: usize,
        new_segments: Vec<CachedSegment>,
    ) {
        let doc = self.documents.entry(file_path.to_string()).or_insert_with(|| DocumentCache {
            file_path: file_path.to_string(),
            segmented_pages: Vec::new(),
            segments: Vec::new(),
        });

        // 1. Only mark target_page as segmented
        if !doc.segmented_pages.contains(&target_page) {
            doc.segmented_pages.push(target_page);
            doc.segmented_pages.sort();
        }

        // 2. Remove segments that fall within the current window range
        doc.segments.retain(|seg| {
            seg.end_offset <= window_start || seg.start_offset >= window_end
        });

        // 3. Insert new segments and sort
        doc.segments.extend(new_segments);
        doc.segments.sort_by_key(|s| s.start_offset);

        // Save immediately to disk
        let _ = self.save();
    }

    /// Find how much of Page N (from page_start to page_end) is already covered
    /// by a cached segment. Returns the absolute byte offset `covered_until` (which is >= page_start).
    /// Allows crossing small formatting gaps (whitespace, newlines, separators) up to 15 bytes.
    pub fn get_covered_until(&self, file_path: &str, page_start: usize, page_end: usize) -> usize {
        let mut current_covered = page_start;
        if let Some(doc) = self.documents.get(file_path) {
            loop {
                let mut extended = false;
                for seg in &doc.segments {
                    // Check if segment starts within current_covered + 15 bytes and ends after current_covered
                    if seg.start_offset <= current_covered + 15 && seg.end_offset > current_covered {
                        current_covered = seg.end_offset;
                        extended = true;
                        break;
                    }
                }
                if !extended || current_covered >= page_end {
                    break;
                }
            }
        }
        current_covered
    }

    /// Get segments overlapping with a specific page's byte offsets
    pub fn get_segments_for_page(
        &self,
        file_path: &str,
        page_start: usize,
        page_end: usize,
    ) -> Option<Vec<CachedSegment>> {
        let doc = self.documents.get(file_path)?;
        let page_segs: Vec<CachedSegment> = doc
            .segments
            .iter()
            .filter(|seg| {
                // Overlaps if: seg.start_offset < page_end AND seg.end_offset > page_start
                seg.start_offset < page_end && seg.end_offset > page_start
            })
            .cloned()
            .collect();
        Some(page_segs)
    }

    /// Check if a page is already marked as segmented
    pub fn is_page_segmented(&self, file_path: &str, page: usize) -> bool {
        if let Some(doc) = self.documents.get(file_path) {
            doc.segmented_pages.contains(&page)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_segmentation_management() {
        let mut cache = AppCache::default();
        let file = "test_document.txt";

        // Mark pages 0, 1 as segmented and add some segments
        let seg1 = CachedSegment {
            text: "Hello page 0".to_string(),
            start_offset: 0,
            end_offset: 12,
        };
        let seg2 = CachedSegment {
            text: "World page 0-1 split".to_string(),
            start_offset: 13,
            end_offset: 33,
        };
        let seg3 = CachedSegment {
            text: "Hello page 1".to_string(),
            start_offset: 55,
            end_offset: 67,
        };

        // Update cache (simulated save fails silently if config folder not writable, which is fine for tests)
        cache.update_segments(
            file,
            0,
            0,
            70,
            vec![seg1.clone(), seg2.clone(), seg3.clone()],
        );
        cache.update_segments(
            file,
            1,
            0,
            70,
            vec![seg1.clone(), seg2.clone(), seg3.clone()],
        );

        // Verify status
        assert!(cache.is_page_segmented(file, 0));
        assert!(cache.is_page_segmented(file, 1));
        assert!(!cache.is_page_segmented(file, 2));

        // Get segments for Page 0 (say start 0, end 20)
        let page_0_segs = cache.get_segments_for_page(file, 0, 20).unwrap();
        assert_eq!(page_0_segs.len(), 2); // seg1 and seg2 overlap with [0, 20]
        assert_eq!(page_0_segs[0].text, "Hello page 0");
        assert_eq!(page_0_segs[1].text, "World page 0-1 split");

        // Get segments for Page 1 (say start 20, end 70)
        let page_1_segs = cache.get_segments_for_page(file, 20, 70).unwrap();
        assert_eq!(page_1_segs.len(), 2); // seg2 and seg3 overlap with [20, 70]
        assert_eq!(page_1_segs[0].text, "World page 0-1 split");
        assert_eq!(page_1_segs[1].text, "Hello page 1");

        // Test get_covered_until
        // Page 0 starts at 0, ends at 20. Segments cover 0 to 33 continuously.
        let covered = cache.get_covered_until(file, 0, 20);
        assert_eq!(covered, 33); // covered up to 33 (which is >= page_end 20)

        // Page 1 starts at 20, ends at 40. Segments cover 13 to 33, then 55 to 67.
        // Starting coverage from 20:
        // - segment [13, 33] covers 20. covered becomes 33.
        // - segment [55, 67] starts at 55, which is > 33 + 15. So there is a real gap at 33.
        // Hence, it should return 33.
        let covered_p1 = cache.get_covered_until(file, 20, 40);
        assert_eq!(covered_p1, 33);

        // Starting coverage from 55 (inside page 1, after the gap):
        // - segment [55, 67] covers 55. covered becomes 67.
        // Since 67 >= page_end 60, it stops and returns 67.
        let covered_p1_after_gap = cache.get_covered_until(file, 55, 60);
        assert_eq!(covered_p1_after_gap, 67);
    }
}

