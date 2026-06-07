use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedSegment {
    pub text: String,
    pub start_offset: usize, // absolute character/byte offset in document
    pub end_offset: usize,   // absolute character/byte offset in document
    #[serde(default)]
    pub analysis: Option<String>,
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

    /// Update segments for a batch of pages.
    /// Marks all target_pages as fully segmented.
    /// Removes existing segments in [window_start, window_end] to prevent duplicate overlaps.
    pub fn update_segments_batch(
        &mut self,
        file_path: &str,
        target_pages: Vec<usize>,
        window_start: usize,
        window_end: usize,
        new_segments: Vec<CachedSegment>,
    ) {
        let doc = self.documents.entry(file_path.to_string()).or_insert_with(|| DocumentCache {
            file_path: file_path.to_string(),
            segmented_pages: Vec::new(),
            segments: Vec::new(),
        });

        // 1. Mark target pages as segmented
        for page in target_pages {
            if !doc.segmented_pages.contains(&page) {
                doc.segmented_pages.push(page);
            }
        }
        doc.segmented_pages.sort();

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

    /// Insert a manually created segment, resolving any overlaps by deleting overlapping cached segments
    pub fn insert_manual_segment(&mut self, file_path: &str, new_segment: CachedSegment) {
        let doc = self.documents.entry(file_path.to_string()).or_insert_with(|| DocumentCache {
            file_path: file_path.to_string(),
            segmented_pages: Vec::new(),
            segments: Vec::new(),
        });

        let new_start = new_segment.start_offset;
        let new_end = new_segment.end_offset;

        // Remove any old segments that overlap with the new manual segment
        doc.segments.retain(|seg| {
            !(seg.start_offset < new_end && seg.end_offset > new_start)
        });

        // Add the new segment and sort
        doc.segments.push(new_segment);
        doc.segments.sort_by_key(|s| s.start_offset);

        let _ = self.save();
    }

    /// Update the cached analysis for a specific segment identified by its absolute offsets
    pub fn update_segment_analysis(
        &mut self,
        file_path: &str,
        start_offset: usize,
        end_offset: usize,
        analysis: String,
    ) {
        if let Some(doc) = self.documents.get_mut(file_path) {
            if let Some(seg) = doc.segments.iter_mut().find(|s| {
                s.start_offset == start_offset && s.end_offset == end_offset
            }) {
                seg.analysis = Some(analysis);
                let _ = self.save();
            }
        }
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

    /// Find the start offset of the first cached segment that starts at or after `from_offset`.
    /// If none exists, returns `None`.
    pub fn get_next_segment_start(&self, file_path: &str, from_offset: usize) -> Option<usize> {
        let doc = self.documents.get(file_path)?;
        for seg in &doc.segments {
            if seg.start_offset >= from_offset {
                return Some(seg.start_offset);
            }
        }
        None
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
            analysis: None,
        };
        let seg2 = CachedSegment {
            text: "World page 0-1 split".to_string(),
            start_offset: 13,
            end_offset: 33,
            analysis: None,
        };
        let seg3 = CachedSegment {
            text: "Hello page 1".to_string(),
            start_offset: 55,
            end_offset: 67,
            analysis: None,
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

        // Test update_segment_analysis
        cache.update_segment_analysis(file, 0, 12, "Analysis for page 0 segment".to_string());
        
        let updated_page_0_segs = cache.get_segments_for_page(file, 0, 20).unwrap();
        assert_eq!(updated_page_0_segs[0].analysis, Some("Analysis for page 0 segment".to_string()));
        assert_eq!(updated_page_0_segs[1].analysis, None);

        // Test update_segments_batch
        let batch_seg = CachedSegment {
            text: "Hello batch segment".to_string(),
            start_offset: 75,
            end_offset: 95,
            analysis: None,
        };
        cache.update_segments_batch(
            file,
            vec![2, 3],
            70,
            100,
            vec![batch_seg],
        );
        assert!(cache.is_page_segmented(file, 2));
        assert!(cache.is_page_segmented(file, 3));
        let page_2_segs = cache.get_segments_for_page(file, 70, 100).unwrap();
        assert_eq!(page_2_segs.len(), 1);
        assert_eq!(page_2_segs[0].text, "Hello batch segment");

        // Test insert_manual_segment (overlapping resolution)
        let manual_seg = CachedSegment {
            text: "Manual overlay".to_string(),
            start_offset: 80,
            end_offset: 90,
            analysis: None,
        };
        cache.insert_manual_segment(file, manual_seg);
        
        let resolved_segs = cache.get_segments_for_page(file, 70, 100).unwrap();
        // The batch segment [75, 95] should be removed because it overlaps with manual segment [80, 90]
        assert_eq!(resolved_segs.len(), 1);
        assert_eq!(resolved_segs[0].text, "Manual overlay");
    }
}

