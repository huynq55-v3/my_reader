mod config;
mod document;
mod ai_client;
mod cache;

use config::AppConfig;
use document::{ReaderState, DocumentSegment, SegmentStatus};
use cache::AppCache;
use eframe::egui;
use std::path::PathBuf;

pub struct SegmentationSuccess {
    pub filtered_chunks: Vec<String>,
    pub all_segments: Vec<(String, usize, usize)>, // (text, start, end in combined_text)
    pub page_offsets: Vec<(usize, usize, usize)>,  // page, start, end in combined_text
    pub file_path: PathBuf,
    pub window_start_abs: usize,
}

pub enum WorkerMessage {
    LoadFile(PathBuf),
    SegmentText {
        config: AppConfig,
        combined_text: String,
        page_offsets: Vec<(usize, usize, usize)>,
        current_page: usize,
        file_path: PathBuf,
        window_start_abs: usize,
    },
    AnalyzeSegment {
        config: AppConfig,
        context: String,
        target_text: String,
        segment_id: usize,
    },
    RenderPage {
        file_path: PathBuf,
        page_index: usize,
    },
}

pub enum UiMessage {
    FileLoaded(Result<ReaderState, String>),
    SegmentationResult(Result<SegmentationSuccess, String>),
    AnalysisResult(usize, Result<String, String>),
    PageRendered {
        page_index: usize,
        color_image: egui::ColorImage,
        layout: crate::document::PageLayout,
    },
    PageRenderError {
        page_index: usize,
        error: String,
    },
}

pub struct RenderedPageData {
    pub page_index: usize,
    pub texture: egui::TextureHandle,
    pub layout: crate::document::PageLayout,
}

pub struct UiApp {
    reader_state: ReaderState,
    config: AppConfig,
    cache: AppCache,
    selected_segment_id: Option<usize>,
    active_analysis: Option<String>,
    tx: tokio::sync::mpsc::Sender<WorkerMessage>,
    rx: std::sync::mpsc::Receiver<UiMessage>,
    
    // UI state
    is_config_open: bool,
    error_message: Option<String>,
    loading_file: bool,
    page_jump_text: String,
    hovered_segment_id: Option<usize>,
    is_segmented: bool,

    // Visual PDF view state
    visual_view: bool,
    rendered_page_data: Option<RenderedPageData>,
    rendering_page_index: Option<usize>,
    page_render_error: Option<String>,
}

impl UiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Configure theme / styling
        let mut style = (*cc.egui_ctx.style()).clone();
        
        // Dark theme adjustments
        style.visuals.dark_mode = true;
        style.visuals.window_rounding = 8.0.into();
        
        // Configure rounding for standard widgets
        style.visuals.widgets.noninteractive.rounding = 6.0.into();
        style.visuals.widgets.inactive.rounding = 6.0.into();
        style.visuals.widgets.hovered.rounding = 6.0.into();
        style.visuals.widgets.active.rounding = 6.0.into();
        style.visuals.widgets.open.rounding = 6.0.into();
        
        // Colors
        style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(18, 18, 18);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(59, 130, 246); // Accent blue
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(37, 99, 235);
        cc.egui_ctx.set_style(style);

        // Configure custom fonts for Vietnamese diacritics support
        configure_fonts(&cc.egui_ctx);

        let (ui_tx, std_rx) = std::sync::mpsc::channel::<UiMessage>();
        let (tx, mut worker_rx) = tokio::sync::mpsc::channel::<WorkerMessage>(100);

        let ctx = cc.egui_ctx.clone();

        // Spawn Tokio background worker thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                while let Some(msg) = worker_rx.recv().await {
                    let ui_tx = ui_tx.clone();
                    let ctx = ctx.clone();
                    match msg {
                        WorkerMessage::LoadFile(path) => {
                            tokio::spawn(async move {
                                let res = ReaderState::load_file(path);
                                let _ = ui_tx.send(UiMessage::FileLoaded(res));
                                ctx.request_repaint();
                            });
                        }
                        WorkerMessage::SegmentText { config, combined_text, page_offsets, current_page, file_path, window_start_abs } => {
                            tokio::spawn(async move {
                                let res = crate::ai_client::segment_text(&config, &combined_text).await;
                                
                                let processed_res = match res {
                                    Ok(chunks) => {
                                        // 1. Calculate offsets for all chunks in combined_text
                                        let mut all_segments = Vec::new();
                                        for chunk in &chunks {
                                            if let Some((start_idx, end_idx)) = find_approximate_match(&combined_text, chunk) {
                                                all_segments.push((chunk.clone(), start_idx, end_idx));
                                            } else {
                                                // Fallback if not found: map to start of combined_text
                                                all_segments.push((chunk.clone(), 0, chunk.len()));
                                            }
                                        }

                                        // 2. Filter chunks that overlap with current_page
                                        let page_bound = page_offsets.iter().find(|(p, _, _)| *p == current_page);
                                        let mut filtered_chunks = Vec::new();
                                        if let Some((_, page_start, page_end)) = page_bound {
                                            for (chunk, start_idx, end_idx) in &all_segments {
                                                if *start_idx < *page_end && *end_idx > *page_start {
                                                    filtered_chunks.push(chunk.clone());
                                                }
                                            }
                                        } else {
                                            filtered_chunks = chunks;
                                        }

                                        Ok(SegmentationSuccess {
                                            filtered_chunks,
                                            all_segments,
                                            page_offsets,
                                            file_path,
                                            window_start_abs,
                                        })
                                    }
                                    Err(e) => Err(e),
                                };
                                
                                let _ = ui_tx.send(UiMessage::SegmentationResult(processed_res));
                                ctx.request_repaint();
                            });
                        }
                        WorkerMessage::AnalyzeSegment {
                            config,
                            context,
                            target_text,
                            segment_id,
                        } => {
                            tokio::spawn(async move {
                                let res = crate::ai_client::analyze_segment(&config, &context, &target_text).await;
                                let _ = ui_tx.send(UiMessage::AnalysisResult(segment_id, res));
                                ctx.request_repaint();
                            });
                        }
                        WorkerMessage::RenderPage { file_path, page_index } => {
                            tokio::spawn(async move {
                                let res = render_pdf_page_task(&file_path, page_index).await;
                                match res {
                                    Ok((color_image, layout)) => {
                                        let _ = ui_tx.send(UiMessage::PageRendered {
                                            page_index,
                                            color_image,
                                            layout,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = ui_tx.send(UiMessage::PageRenderError {
                                            page_index,
                                            error: e,
                                        });
                                    }
                                }
                                ctx.request_repaint();
                            });
                        }
                    }
                }
            });
        });

        // Load configuration from local config directories (XDG compliant ~/.config/my_reader/config.json)
        let config = AppConfig::load();
        let cache = AppCache::load();

        Self {
            reader_state: ReaderState::default(),
            config,
            cache,
            selected_segment_id: None,
            active_analysis: None,
            tx,
            rx: std_rx,
            is_config_open: false,
            error_message: None,
            loading_file: false,
            page_jump_text: String::new(),
            hovered_segment_id: None,
            is_segmented: false,
            visual_view: true,
            rendered_page_data: None,
            rendering_page_index: None,
            page_render_error: None,
        }
    }

    /// Check if the current page has already been segmented and is fully cached.
    /// If so, load the cached segments immediately and enter segmented view.
    /// Otherwise, reset the page to the normal unsegmented text view.
    fn check_cache_or_reset(&mut self) {
        self.rendered_page_data = None;
        self.page_render_error = None;
        self.rendering_page_index = None;

        if self.reader_state.pages.is_empty() {
            return;
        }
        self.selected_segment_id = None;
        self.active_analysis = None;

        let current_page = self.reader_state.current_page;
        let file_path = self.reader_state.file_path.clone().unwrap_or_default();
        let file_path_str = file_path.to_string_lossy().to_string();

        let page_offsets_all = self.reader_state.get_page_absolute_offsets();
        if current_page < page_offsets_all.len() {
            let (page_start, page_end) = page_offsets_all[current_page];
            let covered_until = self.cache.get_covered_until(&file_path_str, page_start, page_end);

            if covered_until >= page_end {
                // If not marked as segmented yet, mark it now
                if !self.cache.is_page_segmented(&file_path_str, current_page) {
                    self.cache.update_segments(&file_path_str, current_page, page_start, page_end, vec![]);
                }
                
                if let Some(cached_segs) = self.cache.get_segments_for_page(&file_path_str, page_start, page_end) {
                    // Filter out segments that start on a previous page that is already segmented
                    let mut filtered_segs = Vec::new();
                    for seg in cached_segs {
                        let page_idx_of_start = page_offsets_all.iter().position(|&(start, end)| {
                            seg.start_offset >= start && seg.start_offset < end
                        });
                        
                        let should_keep = if let Some(p_idx) = page_idx_of_start {
                            if p_idx < current_page {
                                !self.cache.is_page_segmented(&file_path_str, p_idx)
                            } else {
                                true
                            }
                        } else {
                            true
                        };
                        
                        if should_keep {
                            filtered_segs.push(seg);
                        }
                    }

                    if !filtered_segs.is_empty() {
                        self.reader_state.segments = filtered_segs
                            .into_iter()
                            .enumerate()
                            .map(|(idx, seg)| DocumentSegment {
                                id: idx,
                                text: seg.text,
                                start_offset: seg.start_offset,
                                end_offset: seg.end_offset,
                                status: match seg.analysis {
                                    Some(ref analysis) => SegmentStatus::Analyzed(analysis.clone()),
                                    None => SegmentStatus::Idle,
                                },
                            })
                            .collect();
                        self.reader_state.segmentation_loading = false;
                        self.reader_state.segmentation_error = None;
                        self.is_segmented = true;
                        return; // Cached hit! Loaded successfully.
                    }
                }
            }
        }

        // If cache miss, reset view to normal unsegmented text
        self.is_segmented = false;
        self.reader_state.segments.clear();
        self.reader_state.segmentation_loading = false;
        self.reader_state.segmentation_error = None;
    }

    /// Request AI text segmentation for the current context window
    fn trigger_segmentation(&mut self) {
        if self.reader_state.pages.is_empty() {
            return;
        }
        self.selected_segment_id = None;
        self.active_analysis = None;

        // Check if API key is configured. If empty, fall back immediately to local segmentation
        if self.config.api_key.trim().is_empty() {
            self.reader_state.fallback_local_segmentation();
            return;
        }

        self.reader_state.segmentation_loading = true;
        self.reader_state.segmentation_error = None;
        self.reader_state.segments.clear();

        // Calculate sliding window page indices dynamically (only looking forward for context)
        let current_page = self.reader_state.current_page;
        let file_path = self.reader_state.file_path.clone().unwrap_or_default();
        let file_path_str = file_path.to_string_lossy().to_string();
        let page_offsets_all = self.reader_state.get_page_absolute_offsets();

        let total_pages = self.reader_state.pages.len();
        let window = self.config.context_window_size;
        let end_page = if current_page + window < total_pages { current_page + window } else { total_pages - 1 };

        let mut combined_text = String::new();
        let mut page_offsets = Vec::new();

        // 1. Unsegmented part of the target page
        let (page_start, _page_end) = page_offsets_all[current_page];
        let covered_until = self.cache.get_covered_until(&file_path_str, page_start, _page_end);
        let unsegmented_text = &self.reader_state.pages[current_page][covered_until - page_start..];

        let start_offset = 0;
        combined_text.push_str(unsegmented_text);
        let end_offset = combined_text.len();
        page_offsets.push((current_page, start_offset, end_offset));

        // 2. Succeeding pages for context
        for p in (current_page + 1)..=end_page {
            combined_text.push('\n');
            let start_offset = combined_text.len();
            combined_text.push_str(&self.reader_state.pages[p]);
            let end_offset = combined_text.len();
            page_offsets.push((p, start_offset, end_offset));
        }

        let _ = self.tx.blocking_send(WorkerMessage::SegmentText {
            config: self.config.clone(),
            combined_text,
            page_offsets,
            current_page,
            file_path,
            window_start_abs: covered_until,
        });
    }

    /// Process messages from the background worker
    fn poll_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMessage::FileLoaded(res) => {
                    self.loading_file = false;
                    match res {
                        Ok(state) => {
                            self.reader_state = state;
                            self.page_jump_text = (self.reader_state.current_page + 1).to_string();
                            self.check_cache_or_reset();
                        }
                        Err(e) => {
                            self.error_message = Some(e);
                        }
                    }
                }
                UiMessage::SegmentationResult(res) => {
                    self.reader_state.segmentation_loading = false;
                    match res {
                Ok(success) => {
                            self.selected_segment_id = None;
                            self.active_analysis = None;
                            self.reader_state.segmentation_error = None;

                            // 1. Cache the result persistently first
                            let file_path_str = success.file_path.to_string_lossy().to_string();
                            if !file_path_str.is_empty() {
                                let page_absolute_offsets = self.reader_state.get_page_absolute_offsets();
                                let window_pages: Vec<usize> = success.page_offsets.iter().map(|(p, _, _)| *p).collect();
                                if !window_pages.is_empty() {
                                    let start_page = window_pages[0];
                                    let end_page = window_pages[window_pages.len() - 1];

                                    if start_page < page_absolute_offsets.len() && end_page < page_absolute_offsets.len() {
                                        let window_start_char = success.window_start_abs;
                                        let window_end_char = page_absolute_offsets[end_page].1;
                                        let abs_start_offset = success.window_start_abs;

                                        let cached_segs: Vec<crate::cache::CachedSegment> = success.all_segments
                                            .into_iter()
                                            .map(|(text, start, end)| crate::cache::CachedSegment {
                                                text,
                                                start_offset: abs_start_offset + start,
                                                end_offset: abs_start_offset + end,
                                                analysis: None,
                                            })
                                            .collect();

                                        self.cache.update_segments(
                                            &file_path_str,
                                            self.reader_state.current_page,
                                            window_start_char,
                                            window_end_char,
                                            cached_segs,
                                        );
                                    }
                                }
                            }

                            // 2. Query the cache and apply our unified duplication-prevention filtering logic
                            if !file_path_str.is_empty() {
                                let page_absolute_offsets = self.reader_state.get_page_absolute_offsets();
                                let current_page = self.reader_state.current_page;
                                if current_page < page_absolute_offsets.len() {
                                    let (page_start, page_end) = page_absolute_offsets[current_page];
                                    if let Some(cached_segs) = self.cache.get_segments_for_page(&file_path_str, page_start, page_end) {
                                        let mut filtered_segs = Vec::new();
                                        for seg in cached_segs {
                                            let page_idx_of_start = page_absolute_offsets.iter().position(|&(start, end)| {
                                                seg.start_offset >= start && seg.start_offset < end
                                            });
                                            let should_keep = if let Some(p_idx) = page_idx_of_start {
                                                if p_idx < current_page {
                                                    !self.cache.is_page_segmented(&file_path_str, p_idx)
                                                } else {
                                                    true
                                                }
                                            } else {
                                                true
                                            };
                                            if should_keep {
                                                filtered_segs.push(seg);
                                            }
                                        }

                                        self.reader_state.segments = filtered_segs
                                            .into_iter()
                                            .enumerate()
                                            .map(|(idx, seg)| DocumentSegment {
                                                id: idx,
                                                text: seg.text,
                                                start_offset: seg.start_offset,
                                                end_offset: seg.end_offset,
                                                status: match seg.analysis {
                                                    Some(ref analysis) => SegmentStatus::Analyzed(analysis.clone()),
                                                    None => SegmentStatus::Idle,
                                                },
                                            })
                                            .collect();
                                        self.is_segmented = true;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.reader_state.segmentation_error = Some(e);
                        }
                    }
                }
                UiMessage::AnalysisResult(segment_id, res) => {
                    if let Some(segment) = self.reader_state.segments.iter_mut().find(|s| s.id == segment_id) {
                        match res {
                            Ok(analysis) => {
                                segment.status = SegmentStatus::Analyzed(analysis.clone());
                                if self.selected_segment_id == Some(segment_id) {
                                    self.active_analysis = Some(analysis.clone());
                                }
                                
                                // Cache the analysis explanation persistently
                                let file_path_str = self.reader_state.file_path.clone().unwrap_or_default().to_string_lossy().to_string();
                                if !file_path_str.is_empty() {
                                    self.cache.update_segment_analysis(
                                        &file_path_str,
                                        segment.start_offset,
                                        segment.end_offset,
                                        analysis,
                                    );
                                }
                            }
                            Err(e) => {
                                segment.status = SegmentStatus::Error(e.clone());
                                if self.selected_segment_id == Some(segment_id) {
                                    self.active_analysis = Some(format!("Lỗi phân tích: {}", e));
                                }
                            }
                        }
                    }
                }
                UiMessage::PageRendered { page_index, color_image, layout } => {
                    if self.reader_state.current_page == page_index {
                        let texture = ctx.load_texture(
                            format!("pdf_page_{}", page_index),
                            color_image,
                            egui::TextureOptions::default(),
                        );
                        self.rendered_page_data = Some(RenderedPageData {
                            page_index,
                            texture,
                            layout,
                        });
                        self.page_render_error = None;
                    }
                    if self.rendering_page_index == Some(page_index) {
                        self.rendering_page_index = None;
                    }
                }
                UiMessage::PageRenderError { page_index, error } => {
                    if self.reader_state.current_page == page_index {
                        self.page_render_error = Some(error);
                    }
                    if self.rendering_page_index == Some(page_index) {
                        self.rendering_page_index = None;
                    }
                }
            }
        }
    }
}

impl eframe::App for UiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages(ctx);

        // 1. TOP CONTROL PANEL
        egui::TopBottomPanel::top("control_panel")
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(24, 24, 27)).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("📂 Mở tài liệu").on_hover_text("Mở file tài liệu Text hoặc PDF").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Tài liệu (PDF, TXT)", &["pdf", "txt"])
                            .pick_file()
                        {
                            self.loading_file = true;
                            let _ = self.tx.blocking_send(WorkerMessage::LoadFile(path));
                        }
                    }

                    if ui.button("⚙ Cấu hình API").on_hover_text("Thay đổi API Key, Endpoint, và Ngôn ngữ").clicked() {
                        self.is_config_open = true;
                    }

                    ui.separator();

                    if !self.reader_state.pages.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Đang đọc: {}", self.reader_state.file_name))
                                .strong()
                                .color(egui::Color32::from_rgb(147, 197, 253)),
                        );
                    } else if self.loading_file {
                        ui.spinner();
                        ui.label("Đang nạp tài liệu...");
                    } else {
                        ui.label(egui::RichText::new("Vui lòng mở một file để bắt đầu.").weak());
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.reader_state.pages.is_empty() {
                            let total_pages = self.reader_state.pages.len();
                            let current = self.reader_state.current_page;

                            // Next button
                            let next_btn = ui.add_enabled(current + 1 < total_pages, egui::Button::new("Trang sau ➡"));
                            if next_btn.clicked() {
                                self.reader_state.current_page += 1;
                                self.page_jump_text = (self.reader_state.current_page + 1).to_string();
                                self.check_cache_or_reset();
                            }

                            // Page indicator and Jump box
                            ui.label(format!(" / {}", total_pages));
                            
                            let text_edit = ui.add(
                                egui::TextEdit::singleline(&mut self.page_jump_text)
                                    .desired_width(30.0)
                            );
                            if text_edit.lost_focus() || (text_edit.gained_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter))) {
                                if let Ok(page_num) = self.page_jump_text.trim().parse::<usize>() {
                                    if page_num >= 1 && page_num <= total_pages {
                                        self.reader_state.current_page = page_num - 1;
                                        self.check_cache_or_reset();
                                    }
                                }
                                self.page_jump_text = (self.reader_state.current_page + 1).to_string();
                            }

                            // Prev button
                            let prev_btn = ui.add_enabled(current > 0, egui::Button::new("⬅ Trang trước"));
                            if prev_btn.clicked() {
                                self.reader_state.current_page -= 1;
                                self.page_jump_text = (self.reader_state.current_page + 1).to_string();
                                self.check_cache_or_reset();
                            }
                        }
                    });
                });
            });

        // 2. ERROR BANNER (if any) - Handled with local copies to prevent borrowing self mutably inside closure
        let mut clear_error = false;
        if let Some(err) = &self.error_message {
            let err_str = err.clone();
            egui::TopBottomPanel::top("error_banner")
                .frame(egui::Frame::none().fill(egui::Color32::from_rgb(127, 29, 29)).inner_margin(8.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("⚠").strong().color(egui::Color32::WHITE));
                        ui.label(egui::RichText::new(&err_str).color(egui::Color32::WHITE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Đóng").clicked() {
                                clear_error = true;
                            }
                        });
                    });
                });
        }
        if clear_error {
            self.error_message = None;
        }

        // 3. SETTINGS OVERLAY WINDOW - Decouple open state tracking
        let mut is_config_open = self.is_config_open;
        let mut close_config = false;
        if is_config_open {
            egui::Window::new("⚙ Cấu hình API & Hệ thống")
                .open(&mut is_config_open)
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        
                        ui.label(egui::RichText::new("CẤU HÌNH API LLM").strong().color(egui::Color32::from_rgb(147, 197, 253)));
                        ui.add_space(4.0);

                        egui::Grid::new("config_grid")
                            .num_columns(2)
                            .spacing([12.0, 10.0])
                            .show(ui, |ui| {
                                ui.label("Provider Base URL:");
                                ui.text_edit_singleline(&mut self.config.base_url);
                                ui.end_row();

                                ui.label("API Key:");
                                ui.add(egui::TextEdit::singleline(&mut self.config.api_key).password(true));
                                ui.end_row();

                                ui.label("Model Name:");
                                ui.text_edit_singleline(&mut self.config.model);
                                ui.end_row();

                                ui.label("Phạm vi ngữ cảnh (N ± X):");
                                egui::ComboBox::new("context_window_select", "")
                                    .selected_text(format!("Trang N ± {}", self.config.context_window_size))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.config.context_window_size, 1, "Trang N ± 1");
                                        ui.selectable_value(&mut self.config.context_window_size, 2, "Trang N ± 2");
                                        ui.selectable_value(&mut self.config.context_window_size, 3, "Trang N ± 3");
                                        ui.selectable_value(&mut self.config.context_window_size, 4, "Trang N ± 4");
                                        ui.selectable_value(&mut self.config.context_window_size, 5, "Trang N ± 5");
                                    });
                                ui.end_row();

                                ui.label("Ngôn ngữ phản hồi:");
                                egui::ComboBox::new("language_select", "")
                                    .selected_text(&self.config.language)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.config.language, "Tiếng Việt".to_string(), "Tiếng Việt");
                                        ui.selectable_value(&mut self.config.language, "English".to_string(), "English");
                                        ui.selectable_value(&mut self.config.language, "日本語".to_string(), "日本語");
                                    });
                                ui.end_row();
                            });

                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(6.0);
                        
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Lưu & Đóng").clicked() {
                                    close_config = true;
                                }
                            });
                        });
                    });
                });
            self.is_config_open = is_config_open && !close_config;
            
            // If settings closed, trigger AI segmentation just in case they added key or updated range
            if self.is_config_open == false && is_config_open == true {
                // Save config to disk (XDG ~/.config/my_reader/config.json)
                if let Err(e) = self.config.save() {
                    self.error_message = Some(format!("Không thể lưu cấu hình: {}", e));
                }
                
                // Only re-segment if we were already in segmented mode
                if self.is_segmented {
                    self.trigger_segmentation();
                }
            }
        }

        // 4. RIGHT SIDE PANEL: AI ANALYSIS SIDEBAR
        let mut should_retry = false;

        egui::SidePanel::right("ai_analysis_panel")
            .resizable(true)
            .default_width(420.0)
            .width_range(300.0..=600.0)
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(15, 15, 17)).inner_margin(16.0))
            .show(ctx, |ui| {
                ui.heading(
                    egui::RichText::new("🧠 Phân tích Trợ lý AI")
                        .strong()
                        .color(egui::Color32::from_rgb(16, 185, 129)),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                if let Some(seg_id) = self.selected_segment_id {
                    // Find the segment text
                    if let Some(segment) = self.reader_state.segments.iter().find(|s| s.id == seg_id) {
                        egui::ScrollArea::vertical()
                            .id_source("ai_scroll")
                            .show(ui, |ui| {
                                match &segment.status {
                                    SegmentStatus::Loading => {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label("Đang gửi yêu cầu phân tích và chờ AI phản hồi...");
                                        });
                                    }
                                    SegmentStatus::Analyzed(analysis) => {
                                        show_markdown(ui, analysis);
                                    }
                                    SegmentStatus::Error(err) => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(239, 68, 68),
                                            format!("⚠ Có lỗi xảy ra trong quá trình gọi API:\n\n{}", err),
                                        );
                                        if ui.button("Thử lại").clicked() {
                                            should_retry = true;
                                        }
                                    }
                                    SegmentStatus::Idle => {
                                        ui.label("Nhấp chọn đoạn văn ở panel trái để phân tích.");
                                    }
                                }
                            });
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("Hãy chọn một phân đoạn văn bản ở bên trái\nđể xem giải nghĩa và phân tích sâu.")
                                    .weak()
                                    .size(14.0),
                            );
                        });
                    });
                }
            });

        // Handle retry trigger outside immutable borrow of reader_state segments
        if should_retry {
            if let Some(seg_id) = self.selected_segment_id {
                let context = self.reader_state.get_sliding_window_context();
                let target_text = if let Some(s) = self.reader_state.segments.iter().find(|s| s.id == seg_id) {
                    s.text.clone()
                } else {
                    String::new()
                };

                if let Some(active_seg) = self.reader_state.segments.iter_mut().find(|s| s.id == seg_id) {
                    active_seg.status = SegmentStatus::Loading;
                }
                self.active_analysis = Some("Đang thử lại...".to_string());
                
                let _ = self.tx.blocking_send(WorkerMessage::AnalyzeSegment {
                    config: self.config.clone(),
                    context,
                    target_text,
                    segment_id: seg_id,
                });
            }
        }

        // 5. CENTRAL PANEL: THE DOCUMENT READER VIEWPORT
        let mut next_hovered_id = None;
        let mut fallback_local_trigger = false;
        let mut retry_segmentation_trigger = false;
        let mut show_original_trigger = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(9, 9, 11)).inner_margin(16.0))
            .show(ctx, |ui| {
                if self.reader_state.pages.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.35);
                            ui.heading(
                                egui::RichText::new("AI Context-Aware Desktop Reader")
                                    .strong()
                                    .size(26.0)
                                    .color(egui::Color32::from_rgb(59, 130, 246)),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("Mở file văn bản (.txt) hoặc file PDF (.pdf) để bắt đầu trải nghiệm.")
                                    .weak()
                                    .size(15.0),
                            );
                            ui.add_space(16.0);
                            
                            if ui.add(egui::Button::new("📂 Chọn File Ngay").min_size(egui::vec2(150.0, 36.0))).clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Tài liệu (PDF, TXT)", &["pdf", "txt"])
                                    .pick_file()
                                {
                                    self.loading_file = true;
                                    let _ = self.tx.blocking_send(WorkerMessage::LoadFile(path));
                                }
                            }
                        });
                    });
                } else {
                    let is_pdf = self.reader_state.file_path.as_ref()
                        .map(|p| p.extension().map(|e| e.to_string_lossy().to_lowercase() == "pdf").unwrap_or(false))
                        .unwrap_or(false);

                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.heading(
                                egui::RichText::new(format!(
                                    "Trang {} / {}",
                                    self.reader_state.current_page + 1,
                                    self.reader_state.pages.len()
                                ))
                                .strong()
                                .color(egui::Color32::WHITE),
                            );

                            if is_pdf {
                                ui.separator();
                                ui.checkbox(&mut self.visual_view, "🖼 Xem Trang Gốc (PDF Visual)");
                            }

                            ui.separator();

                            if !self.is_segmented {
                                // Primary button to trigger AI segmentation on demand
                                if ui.add(egui::Button::new("⚡ Phân đoạn AI (Segment)").min_size(egui::vec2(130.0, 26.0)))
                                    .on_hover_text("Sử dụng AI để chia trang này thành các phân đoạn thông tin nhỏ")
                                    .clicked()
                                {
                                    retry_segmentation_trigger = true;
                                }
                            } else {
                                if !is_pdf || !self.visual_view {
                                    ui.label(egui::RichText::new("Nhấp vào bất kỳ đoạn văn nào bên dưới để AI phân tích ngữ cảnh.").weak());
                                } else {
                                    ui.label(egui::RichText::new("Nhấp chọn phân đoạn trực tiếp trên ảnh PDF để xem phân tích.").weak());
                                }
                                
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("📄 Bản gốc (Original)").on_hover_text("Quay lại chế độ xem tài liệu thông thường").clicked() {
                                        show_original_trigger = true;
                                    }
                                });
                            }
                        });
                        
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        if is_pdf && self.visual_view {
                            self.render_visual_pdf_view(ui, ctx);
                        } else if !self.is_segmented {
                            // Normal non-segmented page text viewer mode
                            egui::ScrollArea::vertical()
                                .id_source("document_scroll")
                                .show(ui, |ui| {
                                    let page_text = &self.reader_state.pages[self.reader_state.current_page];
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(page_text)
                                                .size(15.0)
                                                .line_height(Some(22.0))
                                                .color(egui::Color32::from_rgb(228, 228, 231)),
                                        ),
                                    );
                                    ui.add_space(20.0);
                                });
                        } else {
                            // Segmented interactive mode
                            if self.reader_state.segmentation_loading {
                                ui.centered_and_justified(|ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.spinner();
                                        ui.add_space(10.0);
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Đang gửi yêu cầu phân đoạn AI (Sliding Window N ± {})...", 
                                                self.config.context_window_size
                                            ))
                                            .weak()
                                            .size(14.0),
                                        );
                                    });
                                });
                            } else if let Some(err) = &self.reader_state.segmentation_error {
                                let err_str = err.clone();
                                ui.vertical_centered(|ui| {
                                    ui.add_space(10.0);
                                    ui.colored_label(
                                        egui::Color32::from_rgb(239, 68, 68),
                                        egui::RichText::new("⚠ Lỗi phân đoạn AI:").strong().size(16.0),
                                    );
                                    ui.add_space(10.0);
                                    
                                    ui.horizontal(|ui| {
                                        ui.add_space(10.0);
                                        if ui.button("🔄 Thử lại phân đoạn AI").clicked() {
                                            retry_segmentation_trigger = true;
                                        }
                                        ui.add_space(10.0);
                                        if ui.button("📄 Dùng phân đoạn mặc định (Local)").clicked() {
                                            fallback_local_trigger = true;
                                        }
                                        ui.add_space(10.0);
                                        if ui.button("📄 Bản gốc").clicked() {
                                            show_original_trigger = true;
                                        }
                                    });
                                    ui.add_space(12.0);
                                    
                                    egui::ScrollArea::vertical()
                                        .max_height(400.0)
                                        .id_source("segmentation_error_scroll")
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&err_str)
                                                        .monospace()
                                                        .color(egui::Color32::from_rgb(239, 68, 68))
                                                )
                                                .selectable(true)
                                            );
                                        });
                                });
                            } else {
                                egui::ScrollArea::vertical()
                                    .id_source("document_scroll")
                                    .show(ui, |ui| {
                                        // Draw segments of the current page
                                        for idx in 0..self.reader_state.segments.len() {
                                            let segment = &self.reader_state.segments[idx];
                                            let is_selected = self.selected_segment_id == Some(segment.id);
                                            let is_hovered = self.hovered_segment_id == Some(segment.id);

                                            // Styling colors based on interaction state
                                            let fill = if is_selected {
                                                egui::Color32::from_rgb(254, 240, 138) // Tailwind yellow-300 (#fef08a)
                                            } else if is_hovered {
                                                egui::Color32::from_rgb(24, 24, 27) // Lighter dark for hovered
                                            } else {
                                                egui::Color32::from_rgb(16, 16, 18) // Base dark card color
                                            };

                                            let stroke = if is_selected {
                                                egui::Stroke::new(1.8, egui::Color32::from_rgb(234, 179, 8)) // Accent yellow border
                                            } else if is_hovered {
                                                egui::Stroke::new(1.0, egui::Color32::from_rgb(63, 63, 70)) // Subtle border on hover
                                            } else {
                                                egui::Stroke::new(1.0, egui::Color32::from_rgb(39, 39, 42)) // Standard dark border
                                            };

                                            let frame = egui::Frame::none()
                                                .fill(fill)
                                                .stroke(stroke)
                                                .rounding(6.0)
                                                .inner_margin(12.0)
                                                .outer_margin(egui::Margin::symmetric(0.0, 4.0));

                                            let response = frame.show(ui, |ui| {
                                                ui.vertical(|ui| {
                                                    ui.horizontal(|ui| {
                                                        match &segment.status {
                                                            SegmentStatus::Idle => {
                                                                ui.label(
                                                                    egui::RichText::new(format!("Đoạn #{}", segment.id + 1))
                                                                        .size(11.0)
                                                                        .strong()
                                                                        .color(if is_selected {
                                                                            egui::Color32::from_rgb(113, 63, 4) // Darker brown-yellow
                                                                        } else {
                                                                            egui::Color32::from_rgb(113, 113, 122)
                                                                        }),
                                                                );
                                                            }
                                                            SegmentStatus::Loading => {
                                                                ui.spinner();
                                                                ui.colored_label(
                                                                    if is_selected {
                                                                        egui::Color32::from_rgb(180, 83, 9)
                                                                    } else {
                                                                        egui::Color32::from_rgb(245, 158, 11)
                                                                    },
                                                                    "Đang phân tích..."
                                                                );
                                                            }
                                                            SegmentStatus::Analyzed(_) => {
                                                                ui.colored_label(
                                                                    if is_selected {
                                                                        egui::Color32::from_rgb(21, 128, 61)
                                                                    } else {
                                                                        egui::Color32::from_rgb(16, 185, 129)
                                                                    },
                                                                    "✓ Đã phân tích"
                                                                );
                                                            }
                                                            SegmentStatus::Error(_) => {
                                                                ui.colored_label(
                                                                    if is_selected {
                                                                        egui::Color32::from_rgb(185, 28, 28)
                                                                    } else {
                                                                        egui::Color32::from_rgb(239, 68, 68)
                                                                    },
                                                                    "⚠ Gặp lỗi"
                                                                );
                                                            }
                                                        }
                                                    });
                                                    
                                                    ui.add_space(6.0);
                                                    
                                                    // High contrast text inside yellow selection frame
                                                    let text_color = if is_selected {
                                                        egui::Color32::from_rgb(28, 25, 23) // Stone-900 (Dark)
                                                    } else {
                                                        egui::Color32::from_rgb(228, 228, 231) // Zinc-200 (Light)
                                                    };

                                                    ui.label(
                                                        egui::RichText::new(&segment.text)
                                                            .size(14.5)
                                                            .line_height(Some(20.0))
                                                            .color(text_color),
                                                    );
                                                });
                                            }).response;

                                            let click_response = ui.interact(response.rect, response.id, egui::Sense::click());
                                            
                                            // Update hover status for the next frame
                                            if click_response.hovered() {
                                                next_hovered_id = Some(segment.id);
                                            }

                                            if click_response.clicked() {
                                                self.selected_segment_id = Some(segment.id);
                                                
                                                let status = segment.status.clone();
                                                let segment_id = segment.id;
                                                
                                                match &status {
                                                    SegmentStatus::Analyzed(analysis) => {
                                                        self.active_analysis = Some(analysis.clone());
                                                    }
                                                    SegmentStatus::Error(e) => {
                                                        self.active_analysis = Some(format!("Lỗi trước đó: {}", e));
                                                    }
                                                    SegmentStatus::Loading => {
                                                        self.active_analysis = Some("Đang tải dữ liệu...".to_string());
                                                    }
                                                    SegmentStatus::Idle => {
                                                        // 1. Get Context and Target Text *before* borrowing mutable reference to segment
                                                        let context = self.reader_state.get_sliding_window_context();
                                                        let target_text = self.reader_state.segments[idx].text.clone();
                                                        
                                                        // 2. Perform mutable borrows of the reader_state
                                                        let segment_mut = &mut self.reader_state.segments[idx];
                                                        segment_mut.status = SegmentStatus::Loading;
                                                        self.active_analysis = Some("Đang tải dữ liệu...".to_string());
                                                        
                                                        let _ = self.tx.blocking_send(WorkerMessage::AnalyzeSegment {
                                                            config: self.config.clone(),
                                                            context,
                                                            target_text,
                                                            segment_id,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        ui.add_space(20.0);
                                    });
                            }
                        }
                    });
                }
            });

        // Trigger local fallback outside central panel
        if fallback_local_trigger {
            self.reader_state.fallback_local_segmentation();
        }

        // Trigger retry AI segmentation outside central panel
        if retry_segmentation_trigger {
            self.is_segmented = true;
            self.trigger_segmentation();
        }

        // Trigger show original page text
        if show_original_trigger {
            self.is_segmented = false;
            self.selected_segment_id = None;
            self.active_analysis = None;
        }

        // Set the hover state for next frame
        if self.hovered_segment_id != next_hovered_id {
            self.hovered_segment_id = next_hovered_id;
            ctx.request_repaint(); // Trigger repaint to show hover outline instantly
        }
    }
}

fn show_markdown(ui: &mut egui::Ui, text: &str) {
    let mut in_code_block = false;
    let mut code_content = String::new();

    for line in text.lines() {
        if line.starts_with("```") {
            if in_code_block {
                // End code block
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(24, 24, 27))
                    .rounding(4.0)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&code_content)
                                .monospace()
                                .color(egui::Color32::from_rgb(190, 242, 100))
                                .size(12.5),
                        );
                    });
                code_content.clear();
                in_code_block = false;
            } else {
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            code_content.push_str(line);
            code_content.push('\n');
            continue;
        }

        if line.starts_with("# ") {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(&line[2..]).strong().size(20.0).color(egui::Color32::WHITE));
            ui.add_space(4.0);
        } else if line.starts_with("## ") {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&line[3..]).strong().size(17.0).color(egui::Color32::from_rgb(229, 231, 235)));
            ui.add_space(4.0);
        } else if line.starts_with("### ") {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&line[4..]).strong().size(15.0).color(egui::Color32::from_rgb(209, 213, 219)));
            ui.add_space(3.0);
        } else if line.starts_with("- ") || line.starts_with("* ") {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.label(egui::RichText::new("•").strong().color(egui::Color32::from_rgb(16, 185, 129)));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    render_inline_text(ui, &line[2..]);
                });
            });
            ui.add_space(2.0);
        } else if line.trim().is_empty() {
            ui.add_space(6.0);
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                render_inline_text(ui, line);
            });
            ui.add_space(4.0);
        }
    }
}

fn render_inline_text(ui: &mut egui::Ui, text: &str) {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '*' && chars.peek() == Some(&'*') {
            chars.next(); // consume second '*'
            if !current.is_empty() {
                parts.push((current.clone(), false, false));
                current.clear();
            }
            let mut bold_content = String::new();
            while let Some(bc) = chars.next() {
                if bc == '*' && chars.peek() == Some(&'*') {
                    chars.next(); // consume second '*'
                    break;
                }
                bold_content.push(bc);
            }
            parts.push((bold_content, true, false));
        } else if c == '`' {
            if !current.is_empty() {
                parts.push((current.clone(), false, false));
                current.clear();
            }
            let mut code_content = String::new();
            while let Some(cc) = chars.next() {
                if cc == '`' {
                    break;
                }
                code_content.push(cc);
            }
            parts.push((code_content, false, true));
        } else {
            current.push(c);
        }
    }

    if !current.is_empty() {
        parts.push((current, false, false));
    }

    for (txt, bold, code) in parts {
        let mut rt = egui::RichText::new(txt).size(13.5).color(egui::Color32::from_rgb(228, 228, 231));
        if bold {
            rt = rt.strong().color(egui::Color32::WHITE);
        }
        if code {
            rt = rt.monospace()
                .color(egui::Color32::from_rgb(244, 63, 94))
                .background_color(egui::Color32::from_rgb(31, 31, 35));
        }
        ui.label(rt);
    }
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Load Roboto font from embedded bytes (downloaded to workspace)
    fonts.font_data.insert(
        "roboto".to_owned(),
        egui::FontData::from_static(include_bytes!("../Roboto-Regular.ttf")),
    );

    // Prepend "roboto" as the primary font family for Proportional (default UI text) and Monospace (code/quotes)
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "roboto".to_owned());

    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "roboto".to_owned());

    ctx.set_fonts(fonts);
}

fn find_approximate_match(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    crate::ai_client::find_normalized(haystack, needle)
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AI Desktop Reader - Tự động Phân đoạn")
            .with_inner_size([1150.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ai_desktop_reader",
        options,
        Box::new(|cc| Box::new(UiApp::new(cc))),
    )
}

impl UiApp {
    fn render_visual_pdf_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let file_path = match &self.reader_state.file_path {
            Some(p) => p.clone(),
            None => return,
        };
        let current_page = self.reader_state.current_page;

        // If not loaded and not loading, trigger load
        if self.rendered_page_data.is_none()
            && self.rendering_page_index.is_none()
            && self.page_render_error.is_none()
        {
            self.rendering_page_index = Some(current_page);
            let _ = self.tx.blocking_send(WorkerMessage::RenderPage {
                file_path,
                page_index: current_page,
            });
        }

        let mut retry_rendering = false;
        if let Some(err) = &self.page_render_error {
            ui.vertical_centered(|ui| {
                ui.add_space(50.0);
                ui.colored_label(egui::Color32::from_rgb(239, 68, 68), format!("Lỗi hiển thị trang PDF: {}", err));
                if ui.button("🔄 Thử lại").clicked() {
                    retry_rendering = true;
                }
            });
        }
        if retry_rendering {
            self.page_render_error = None;
        } else if let Some(data) = &self.rendered_page_data {
            if data.page_index == current_page {
                egui::ScrollArea::both()
                    .id_source("pdf_visual_scroll")
                    .show(ui, |ui| {
                        // Display the PDF page image
                        let max_width = ui.available_width();
                        let max_height = ui.available_height() - 10.0;
                        
                        let img_size = data.texture.size_vec2();
                        let aspect_ratio = img_size.x / img_size.y;
                        
                        let display_width = max_width.min(max_height * aspect_ratio);
                        let display_height = display_width / aspect_ratio;
                        let display_size = egui::vec2(display_width, display_height);

                        let (rect, response) = ui.allocate_exact_size(display_size, egui::Sense::click());

                        // Draw the image
                        let mut mesh = egui::Mesh::with_texture(data.texture.id());
                        mesh.add_rect_with_uv(
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                        ui.painter().add(egui::Shape::mesh(mesh));

                        let page_width = data.layout.width;
                        let page_height = data.layout.height;

                        let mut hovered_segment_id = None;

                        // Click handling on PDF image
                        if response.clicked() {
                            if let Some(pointer_pos) = response.interact_pointer_pos() {
                                // Map click to PDF coordinates
                                let pdf_x = ((pointer_pos.x - rect.min.x) / rect.width()) * page_width;
                                let pdf_y = ((pointer_pos.y - rect.min.y) / rect.height()) * page_height;

                                // Find word under click
                                if let Some(clicked_word_idx) = data.layout.words.iter().position(|w| {
                                    pdf_x >= w.x_min && pdf_x <= w.x_max && pdf_y >= w.y_min && pdf_y <= w.y_max
                                }) {
                                    // Search segments
                                    for segment in &self.reader_state.segments {
                                        let matched = crate::document::find_segment_words(&data.layout.words, &segment.text);
                                        if matched.contains(&clicked_word_idx) {
                                            self.selected_segment_id = Some(segment.id);
                                            let status = segment.status.clone();
                                            let segment_id = segment.id;
                                            match &status {
                                                SegmentStatus::Analyzed(analysis) => {
                                                    self.active_analysis = Some(analysis.clone());
                                                }
                                                SegmentStatus::Idle => {
                                                    let context = self.reader_state.get_sliding_window_context();
                                                    let target_text = segment.text.clone();
                                                    
                                                    if let Some(s_mut) = self.reader_state.segments.iter_mut().find(|s| s.id == segment_id) {
                                                        s_mut.status = SegmentStatus::Loading;
                                                    }
                                                    self.active_analysis = Some("Đang tải dữ liệu...".to_string());

                                                    let _ = self.tx.blocking_send(WorkerMessage::AnalyzeSegment {
                                                        config: self.config.clone(),
                                                        context,
                                                        target_text,
                                                        segment_id,
                                                    });
                                                }
                                                _ => {}
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        // Check hovering on image to highlight segment cards
                        if let Some(pointer_pos) = response.hover_pos() {
                            let pdf_x = ((pointer_pos.x - rect.min.x) / rect.width()) * page_width;
                            let pdf_y = ((pointer_pos.y - rect.min.y) / rect.height()) * page_height;

                            if let Some(hovered_word_idx) = data.layout.words.iter().position(|w| {
                                pdf_x >= w.x_min && pdf_x <= w.x_max && pdf_y >= w.y_min && pdf_y <= w.y_max
                            }) {
                                for segment in &self.reader_state.segments {
                                    let matched = crate::document::find_segment_words(&data.layout.words, &segment.text);
                                    if matched.contains(&hovered_word_idx) {
                                        hovered_segment_id = Some(segment.id);
                                        break;
                                    }
                                }
                            }
                        }

                        if self.hovered_segment_id != hovered_segment_id {
                            self.hovered_segment_id = hovered_segment_id;
                            ctx.request_repaint();
                        }

                        // Draw highlights for each segment
                        for segment in &self.reader_state.segments {
                            let matched_word_indices = crate::document::find_segment_words(&data.layout.words, &segment.text);
                            if matched_word_indices.is_empty() {
                                continue;
                            }

                            let is_selected = self.selected_segment_id == Some(segment.id);
                            let is_hovered = self.hovered_segment_id == Some(segment.id);

                            let fill_color = if is_selected {
                                egui::Color32::from_rgba_unmultiplied(234, 179, 8, 80) // 30% yellow
                            } else if is_hovered {
                                egui::Color32::from_rgba_unmultiplied(59, 130, 246, 50) // 20% blue
                            } else {
                                egui::Color32::from_rgba_unmultiplied(16, 185, 129, 25) // 10% green (segment indicator)
                            };

                            let stroke_color = if is_selected {
                                egui::Stroke::new(1.5, egui::Color32::from_rgb(234, 179, 8))
                            } else if is_hovered {
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(59, 130, 246))
                            } else {
                                egui::Stroke::NONE
                            };

                            for idx in matched_word_indices {
                                let w = &data.layout.words[idx];
                                let screen_x_min = rect.min.x + (w.x_min / page_width) * rect.width();
                                let screen_x_max = rect.min.x + (w.x_max / page_width) * rect.width();
                                let screen_y_min = rect.min.y + (w.y_min / page_height) * rect.height();
                                let screen_y_max = rect.min.y + (w.y_max / page_height) * rect.height();

                                let word_rect = egui::Rect::from_min_max(
                                    egui::pos2(screen_x_min, screen_y_min),
                                    egui::pos2(screen_x_max, screen_y_max),
                                );

                                ui.painter().rect(word_rect, 2.0, fill_color, stroke_color);
                            }
                        }
                    });
            } else {
                self.rendered_page_data = None;
            }
        } else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(10.0);
                    ui.label("Đang dựng hình ảnh trang PDF...");
                });
            });
        }
    }
}

async fn render_pdf_page_task(
    file_path: &std::path::Path,
    page_index: usize,
) -> Result<(egui::ColorImage, crate::document::PageLayout), String> {
    // 1. Create temp directory
    let temp_dir = std::env::temp_dir().join(format!(
        "my_reader_render_{}_{}_{}",
        std::process::id(),
        page_index,
        tokio::time::Instant::now().elapsed().as_micros()
    ));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Không thể tạo thư mục tạm: {}", e))?;

    // 2. Render PNG using pdftoppm
    let page_num_str = (page_index + 1).to_string();
    let ppm_status = tokio::process::Command::new("pdftoppm")
        .arg("-png")
        .arg("-r")
        .arg("150")
        .arg("-f")
        .arg(&page_num_str)
        .arg("-l")
        .arg(&page_num_str)
        .arg(file_path)
        .arg(temp_dir.join("page"))
        .status()
        .await
        .map_err(|e| format!("Không thể chạy pdftoppm: {}", e))?;

    if !ppm_status.success() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err("pdftoppm chạy thất bại".to_string());
    }

    // 3. Find the rendered PNG file
    let mut png_path = None;
    if let Ok(entries) = std::fs::read_dir(&temp_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "png").unwrap_or(false) {
                png_path = Some(path);
                break;
            }
        }
    }

    let png_path = match png_path {
        Some(p) => p,
        None => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err("Không tìm thấy file ảnh được sinh ra bởi pdftoppm".to_string());
        }
    };

    // 4. Load and decode image
    let img_bytes = std::fs::read(&png_path)
        .map_err(|e| format!("Không thể đọc file ảnh: {}", e))?;
    
    let image = image::load_from_memory_with_format(&img_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Không thể decode file ảnh: {}", e))?;
    
    let size = [image.width() as usize, image.height() as usize];
    let image_buffer = image.to_rgba8();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        size,
        image_buffer.as_flat_samples().as_slice(),
    );

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&temp_dir);

    // 5. Extract layout (bbox) using pdftotext
    let text_output = tokio::process::Command::new("pdftotext")
        .arg("-f")
        .arg(&page_num_str)
        .arg("-l")
        .arg(&page_num_str)
        .arg("-bbox")
        .arg(file_path)
        .arg("-")
        .output()
        .await
        .map_err(|e| format!("Không thể chạy pdftotext: {}", e))?;

    if !text_output.status.success() {
        return Err("pdftotext chạy thất bại".to_string());
    }

    let html = String::from_utf8_lossy(&text_output.stdout);
    let layout = crate::document::parse_bbox_html(&html)
        .ok_or_else(|| "Không thể phân tích dữ liệu layout của trang".to_string())?;

    Ok((color_image, layout))
}
