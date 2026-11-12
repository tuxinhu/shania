use shania_core::SttConfig;

/// 录音转文字。
///
/// 前端把一小段 16k 单声道 wav（实时分句后的产物）编码成 base64 传过来，
/// 这里解码后转发给 OpenAI 兼容的 `/audio/transcriptions`。放在 Rust 侧有两个原因：
/// 一是绕开 WebView 的 CORS 限制，二是 apiKey 不必暴露给页面脚本。
///
/// 仍然保留 webm 分支：外部服务或旧前端可能直接发 MediaRecorder 的原始录音。
#[tauri::command]
pub async fn transcribe_audio(
    audio_base64: String,
    mime: String,
    config: SttConfig,
) -> Result<String, String> {
    let bytes = shania_core::decode_audio(&audio_base64)?;

    // 一分钟的 opus 录音约 500KB，20MB 已经是极长录音，多半是传错了数据
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(format!("录音过大（{} 字节），超过 20MB 限制", bytes.len()));
    }

    // 文件名影响部分服务端的格式嗅探：wav 对不上会被当成坏文件
    let filename = if mime.contains("wav") { "clip.wav" } else { "clip.webm" };
    shania_core::transcribe(bytes, &mime, filename, &config).await
}
