use serde::{Deserialize, Serialize};
use reqwest::Client;
use crate::config::AppConfig;

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
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
    text: String,
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
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Không thể tạo HTTP client: {}", e))?;

    let mut url = config.base_url.clone();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str("chat/completions");

    let system_prompt = format!(
        "Bạn là một trợ lý AI chuyên nghiệp phân tích tài liệu chuyên sâu. \
        Hãy phân tích đoạn văn bản được chọn (Target Segment) dựa trên toàn bộ ngữ cảnh xung quanh (trang trước, trang hiện tại, và trang sau) được cung cấp. \
        Đưa ra giải thích chi tiết, tóm tắt ý chính, giải nghĩa thuật ngữ nếu cần thiết. \
        Phản hồi hoàn toàn bằng {}, định dạng bằng Markdown ngắn gọn, rõ ràng, dễ hiểu, trực quan.",
        config.language
    );

    let user_prompt = format!(
        "Dưới đây là ngữ cảnh tài liệu (gồm trang trước, trang hiện tại và trang sau nếu có):\n\n\
        {}\n\n\
        Đoạn văn bản được chọn cần phân tích sâu (Target Segment):\n\n\
        >>> {} <<<\n\n\
        Hãy phân tích đoạn văn bản trên dựa trên ngữ cảnh này.",
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
    };

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Lỗi gửi yêu cầu đến API: {}", e))?;

    let status = response.status();
    let body_text = response
        .text()
        .await
        .map_err(|e| format!("Lỗi đọc phản hồi từ API: {}", e))?;

    if !status.is_success() {
        if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(err_msg) = json_err["error"]["message"].as_str() {
                return Err(format!("API Error ({}): {}", status, err_msg));
            }
        }
        return Err(format!("API Error ({}): {}", status, body_text));
    }

    let response_data: ChatCompletionResponse = serde_json::from_str(&body_text)
        .map_err(|e| format!("Lỗi parse dữ liệu JSON từ API ({}): {}", e, body_text))?;

    if response_data.choices.is_empty() {
        return Err("API không trả về bất kỳ kết quả phân tích nào.".to_string());
    }

    Ok(response_data.choices[0].message.content.clone())
}

/// Send request to OpenAI/DeepSeek compatible API to segment a given text using JSON structured output
pub async fn segment_text(
    config: &AppConfig,
    combined_text: &str,
) -> Result<Vec<String>, String> {
    if config.api_key.trim().is_empty() {
        return Err("API Key trống. Vui lòng cấu hình API Key trong phần Cài đặt.".to_string());
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Không thể tạo HTTP client: {}", e))?;

    let mut url = config.base_url.clone();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str("chat/completions");

    let system_prompt = "Bạn là một trợ lý AI chuyên nghiệp phân tích cấu trúc văn bản. \
        Nhiệm vụ của bạn là phân mảnh (segment) nội dung văn bản được cung cấp thành các đoạn thông tin logic, mạch lạc (Document Segments). \
        Hãy gộp các câu hoặc dòng có cùng chủ đề hoặc logic liền mạch thành một segment. \
        Bạn PHẢI trả về kết quả dưới dạng JSON có cấu trúc chính xác theo schema sau:\n\
        {\n\
          \"segments\": [\n\
            { \"id\": 1, \"text\": \"Nội dung thô của phân đoạn 1\" },\n\
            ...\n\
          ]\n\
        }\n\
        Hãy đảm bảo giá trị của trường 'text' là chính xác các phần trích xuất từ văn bản đầu vào. Không thêm bất kỳ chữ nào ngoài JSON block."
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
                content: format!("Vui lòng phân đoạn văn bản sau đây:\n\n{}", combined_text),
            },
        ],
        temperature: 0.2,
    };

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Lỗi gửi yêu cầu phân đoạn: {}", e))?;

    let status = response.status();
    let body_text = response
        .text()
        .await
        .map_err(|e| format!("Lỗi đọc phản hồi từ API: {}", e))?;

    if !status.is_success() {
        if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(err_msg) = json_err["error"]["message"].as_str() {
                return Err(format!("API Error ({}): {}", status, err_msg));
            }
        }
        return Err(format!("API Error ({}): {}", status, body_text));
    }

    let response_data: ChatCompletionResponse = serde_json::from_str(&body_text)
        .map_err(|e| format!("Lỗi parse dữ liệu JSON từ API: {}", e))?;

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

    // Deserialize
    let parsed: SegmentationResponseJson = serde_json::from_str(json_str)
        .map_err(|e| format!("Không thể phân tích cấu trúc JSON phân đoạn. Phản hồi thô của AI:\n{}\n\nLỗi: {}", content_text, e))?;

    let mut segments = parsed.segments;
    segments.sort_by_key(|s| s.id);

    let result = segments.into_iter().map(|s| s.text).collect();
    Ok(result)
}
