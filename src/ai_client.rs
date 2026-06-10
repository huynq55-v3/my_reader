use serde::{Deserialize, Serialize};
use reqwest::Client;
use crate::config::AppConfig;

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct SegmentItem {
    id: usize,
    prefix: String,
    suffix: String,
}

#[derive(Deserialize)]
struct SegmentationResponseJson {
    segments: Vec<SegmentItem>,
}

/// Send request to OpenAI/DeepSeek compatible API for contextual analysis
pub async fn analyze_segment(
    config: &AppConfig,
    context: &str,
    target_segment: &str,
) -> Result<String, String> {
    if config.api_key.trim().is_empty() {
        return Err("API Key trống. Vui lòng cấu hình API Key trong phần Cài đặt.".to_string());
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("Không thể tạo HTTP client: {}", e))?;

    let mut url = config.base_url.clone();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str("chat/completions");

    let system_prompt = format!(
        "You are a professional AI assistant specializing in in-depth document analysis. \
        Analyze the selected text (Target Segment) based on the provided surrounding context (previous page, current page, and next page). \
        Provide detailed explanations, summarize main points, and define terminology if necessary. \
        Respond completely in {}, formatted using concise, clear, easy-to-understand, and visual Markdown.",
        config.language
    );

    let user_prompt = format!(
        "Here is the document context (including previous, current, and next pages if available):\n\n\
        {}\n\n\
        The selected text that needs deep analysis (Target Segment):\n\n\
        >>> {} <<<\n\n\
        Please analyze the selected text based on this context.",
        context, target_segment
    );

    let payload = ChatCompletionRequest {
        model: config.model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_prompt,
            },
        ],
        temperature: 0.2,
        response_format: None,
    };

    let masked_key = if config.api_key.len() > 8 {
        format!("{}...{}", &config.api_key[..4], &config.api_key[config.api_key.len()-4..])
    } else {
        "***".to_string()
    };

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi gửi yêu cầu phân tích đến API: {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - Error: {:?}",
                e, url, config.model, masked_key, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    let status = response.status();
    let headers = response.headers().clone();

    // Read response as raw bytes to prevent decoding issues (e.g. gzip compression or non-UTF-8 characters)
    let bytes = response
        .bytes()
        .await
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi nhận bytes phản hồi phân tích từ API (Status: {}): {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - Response Headers: {:?}\n\
                - Error: {:?}",
                status, e, url, config.model, masked_key, headers, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    let body_text = String::from_utf8_lossy(&bytes).into_owned();

    if !status.is_success() {
        if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(err_msg) = json_err["error"]["message"].as_str() {
                return Err(format!("API Error ({}): {}", status, err_msg));
            }
        }
        return Err(format!(
            "API Error ({}):\n{}\n\
            [DEBUG INFO]\n\
            - URL: {}\n\
            - Model: {}\n\
            - API Key: {}\n\
            - Headers: {:?}",
            status, body_text, url, config.model, masked_key, headers
        ));
    }

    let response_data: ChatCompletionResponse = serde_json::from_str(&body_text)
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi parse dữ liệu JSON từ API: {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - HTTP Status: {}\n\
                - Response Headers: {:?}\n\
                - Body thô:\n{}\n\
                - Error: {:?}",
                e, url, config.model, masked_key, status, headers, body_text, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    if response_data.choices.is_empty() {
        return Err("API không trả về bất kỳ kết quả phân tích nào.".to_string());
    }

    Ok(response_data.choices[0].message.content.clone())
}

/// Send request to OpenAI/DeepSeek compatible API to segment a given text using JSON structured output.
/// Employs a 3-word prefix/suffix token saving optimization mechanism.
pub async fn segment_text(
    config: &AppConfig,
    combined_text: &str,
) -> Result<Vec<(String, usize, usize)>, String> {
    if config.api_key.trim().is_empty() {
        return Err("API Key trống. Vui lòng cấu hình API Key trong phần Cài đặt.".to_string());
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("Không thể tạo HTTP client: {}", e))?;

    let mut url = config.base_url.clone();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str("chat/completions");

    let system_prompt = "You are a professional AI assistant specializing in analyzing document structure. \
        Your task is to segment the provided document content into logical, coherent information blocks (Document Segments). \
        To optimize speed and minimize response tokens from the LLM, you do NOT need to rewrite the full content of the segments. \
        Instead, for each segment, you only need to provide the exact first 3 words (field 'prefix') and the exact last 3 words (field 'suffix') of that segment. \
        \
        LENGTH REQUIREMENT: Each segment must be at most 1 paragraph long. If you encounter a paragraph boundary (end of paragraph / new line), \
        you MUST end that segment and start a new one. Do not combine multiple paragraphs into a single segment. \
        \
        IMPORTANT: If a paragraph is split across page boundaries (end of the previous page and beginning of the next page), you MUST merge them into a single, complete, and continuous segment: \
        use the first 3 words from the end of the previous page as the 'prefix', and the last 3 words from the start of the next page as the 'suffix' (ignoring any headers/footers in between). \
        \
        Example: For the segment 'Literature is humanity which is wonderful', the prefix will be 'Literature is humanity' and the suffix will be 'which is wonderful'. \
        Make sure to extract the words exactly from the source text (preserving punctuation, casing). \
        \
        You MUST return the result in JSON format matching this schema:\n\
        {\n\
          \"segments\": [\n\
            { \"id\": 1, \"prefix\": \"first 3 words of segment 1\", \"suffix\": \"last 3 words of segment 1\" },\n\
            ...\n\
          ]\n\
        }\n\
        Do not add any text outside of the JSON block."
        .to_string();

    let payload = ChatCompletionRequest {
        model: config.model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".to_string(),
                content: format!("Please segment the following text:\n\n{}", combined_text),
            },
        ],
        temperature: 0.2,
        response_format: Some(ResponseFormat {
            format_type: "json_object".to_string(),
        }),
    };

    let masked_key = if config.api_key.len() > 8 {
        format!("{}...{}", &config.api_key[..4], &config.api_key[config.api_key.len()-4..])
    } else {
        "***".to_string()
    };

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi gửi yêu cầu phân đoạn: {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - Error: {:?}",
                e, url, config.model, masked_key, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    let status = response.status();
    let headers = response.headers().clone();

    // Read response as raw bytes to prevent decoding issues (e.g. gzip compression or non-UTF-8 characters)
    let bytes = response
        .bytes()
        .await
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi nhận bytes phân đoạn từ API (Status: {}): {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - Response Headers: {:?}\n\
                - Error: {:?}",
                status, e, url, config.model, masked_key, headers, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    let body_text = String::from_utf8_lossy(&bytes).into_owned();

    if !status.is_success() {
        eprintln!("[DEBUG] HTTP Error Status (Segment): {}", status);
        eprintln!("[DEBUG] HTTP Error Headers (Segment): {:?}", headers);
        eprintln!("[DEBUG] HTTP Error Body (Segment): {}", body_text);

        if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(err_msg) = json_err["error"]["message"].as_str() {
                return Err(format!("API Error ({}): {}", status, err_msg));
            }
        }
        return Err(format!(
            "API Error ({}):\n{}\n\
            [DEBUG INFO]\n\
            - URL: {}\n\
            - Model: {}\n\
            - API Key: {}\n\
            - Headers: {:?}",
            status, body_text, url, config.model, masked_key, headers
        ));
    }

    let response_data: ChatCompletionResponse = serde_json::from_str(&body_text)
        .map_err(|e| {
            let err_msg = format!(
                "Lỗi parse dữ liệu JSON từ API phân đoạn: {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - HTTP Status: {}\n\
                - Response Headers: {:?}\n\
                - Body thô:\n{}\n\
                - Error: {:?}",
                e, url, config.model, masked_key, status, headers, body_text, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    if response_data.choices.is_empty() {
        return Err("API không trả về kết quả phân đoạn.".to_string());
    }

    let content_text = &response_data.choices[0].message.content;

    // Clean JSON block markers if present
    let mut json_str = content_text.trim();
    if json_str.starts_with("```json") {
        json_str = json_str.strip_prefix("```json").unwrap_or(json_str);
    } else if json_str.starts_with("```") {
        json_str = json_str.strip_prefix("```").unwrap_or(json_str);
    }
    if json_str.ends_with("```") {
        json_str = json_str.strip_suffix("```").unwrap_or(json_str);
    }
    let json_str = json_str.trim();

    // Deserialize prefix/suffix metadata
    let parsed: SegmentationResponseJson = serde_json::from_str(json_str)
        .map_err(|e| {
            let err_msg = format!(
                "Không thể phân tích cấu trúc JSON phân đoạn: {}\n\
                [DEBUG INFO]\n\
                - URL: {}\n\
                - Model: {}\n\
                - API Key: {}\n\
                - HTTP Status: {}\n\
                - Phản hồi thô của AI:\n{}\n\
                - JSON trích xuất:\n{}\n\
                - Error: {:?}",
                e, url, config.model, masked_key, status, content_text, json_str, e
            );
            eprintln!("{}", err_msg);
            err_msg
        })?;

    let mut items = parsed.segments;
    items.sort_by_key(|s| s.id);

    // Reconstruct full text chunks from 3-word prefixes and suffixes sequentially
    let mut reconstructed_chunks = Vec::new();
    let mut current_pos = 0;
    let num_items = items.len();

    for i in 0..num_items {
        let item = &items[i];
        let prefix = &item.prefix;
        let suffix = &item.suffix;

        if prefix.is_empty() || suffix.is_empty() {
            continue;
        }

        // Find start of current segment
        let start_idx = if let Some((start, _)) = find_normalized(&combined_text[current_pos..], prefix) {
            current_pos + start
        } else {
            current_pos
        };

        // Find start of next segment to bound our suffix search space
        let next_start_idx = if i + 1 < num_items {
            let next_prefix = &items[i + 1].prefix;
            if !next_prefix.is_empty() {
                if let Some((next_start, _)) = find_normalized(&combined_text[start_idx + prefix.len()..], next_prefix) {
                    start_idx + prefix.len() + next_start
                } else {
                    combined_text.len()
                }
            } else {
                combined_text.len()
            }
        } else {
            combined_text.len()
        };

        // Look for suffix in the range [start_idx + prefix.len() .. next_start_idx]
        let mut end_idx = next_start_idx;
        if start_idx + prefix.len() < next_start_idx {
            let search_range = &combined_text[start_idx + prefix.len()..next_start_idx];
            if let Some((_, suffix_end)) = find_normalized(search_range, suffix) {
                end_idx = start_idx + prefix.len() + suffix_end;
            }
        }

        // Extract segment text
        let segment_text = combined_text[start_idx..end_idx].trim().to_string();
        if !segment_text.is_empty() {
            reconstructed_chunks.push((segment_text, start_idx, end_idx));
        }
        
        current_pos = end_idx;
    }

    Ok(reconstructed_chunks)
}

/// Whitespace-insensitive matching to find the start and end byte indices of a needle within a haystack.
pub fn find_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle_tokens: Vec<String> = needle
        .split_whitespace()
        .map(|w| normalize_token(w))
        .filter(|w| !w.is_empty())
        .collect();

    if needle_tokens.is_empty() {
        return None;
    }

    // Tokenize haystack with byte indices
    let mut haystack_tokens = Vec::new();
    let mut current_token_start = None;

    for (byte_idx, ch) in haystack.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = current_token_start {
                let end = byte_idx;
                let word = &haystack[start..end];
                haystack_tokens.push((start, end, normalize_token(word)));
                current_token_start = None;
            }
        } else if current_token_start.is_none() {
            current_token_start = Some(byte_idx);
        }
    }
    if let Some(start) = current_token_start {
        let end = haystack.len();
        let word = &haystack[start..end];
        haystack_tokens.push((start, end, normalize_token(word)));
    }

    if haystack_tokens.len() < needle_tokens.len() {
        return None;
    }

    // Slide a window of size `needle_tokens.len()` over `haystack_tokens`
    let window_size = needle_tokens.len();
    for i in 0..=(haystack_tokens.len() - window_size) {
        let mut matches = true;
        for j in 0..window_size {
            if haystack_tokens[i + j].2 != needle_tokens[j] {
                matches = false;
                break;
            }
        }
        if matches {
            let start_byte = haystack_tokens[i].0;
            let end_byte = haystack_tokens[i + window_size - 1].1;
            return Some((start_byte, end_byte));
        }
    }

    // Fallback: whitespace-insensitive substring search mapping
    let clean_haystack: String = haystack.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let clean_needle: String = needle.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    if let Some(idx) = clean_haystack.find(&clean_needle) {
        let mut h_chars = haystack.char_indices().peekable();
        let mut clean_idx = 0;
        let mut start_byte = None;
        let mut end_byte = None;

        while let Some(&(b_idx, ch)) = h_chars.peek() {
            if ch.is_whitespace() {
                h_chars.next();
                continue;
            }
            if clean_idx == idx {
                start_byte = Some(b_idx);
            }
            if clean_idx == idx + clean_needle.chars().count() - 1 {
                end_byte = Some(b_idx + ch.len_utf8());
                break;
            }
            clean_idx += 1;
            h_chars.next();
        }

        if let (Some(s), Some(e)) = (start_byte, end_byte) {
            return Some((s, e));
        }
    }

    None
}

fn normalize_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

