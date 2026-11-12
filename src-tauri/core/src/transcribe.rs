use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 语音识别（STT）接入参数——**HTTP 那套引擎用**（本地 whisper-server，或任何
/// OpenAI 兼容的 `/audio/transcriptions` 服务：faster-whisper、FunASR 网关、云端 OpenAI）。
///
/// 另一套引擎 X-ASR 在 `stream_stt.rs`，进程内推理、不走 HTTP，也就不需要这里的
/// base_url / model / api_key。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttConfig {
    /// 服务根地址，例如 http://127.0.0.1:28080/v1
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// 模型名，如 whisper-1 / whisper-large-v3（本地 whisper-server 忽略它）
    #[serde(default = "default_model")]
    pub model: String,
    /// 口音提示，如 zh / en；留空由服务端自动判断
    #[serde(default)]
    pub language: Option<String>,
}

fn default_model() -> String {
    "whisper-1".to_string()
}

/// 把一段录音转成文字。
///
/// `audio` 是原始字节（webm/ogg/wav/mp3 均可，取决于服务端支持），
/// 这里不做转码——转码要引入 ffmpeg，代价远大于收益。
pub async fn transcribe(
    audio: Vec<u8>,
    mime: &str,
    filename: &str,
    cfg: &SttConfig,
) -> Result<String, String> {
    let base = cfg.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("未配置语音识别服务地址".to_string());
    }
    if audio.is_empty() {
        return Err("录音为空".to_string());
    }

    let endpoint = if base.ends_with("/audio/transcriptions") {
        base.to_string()
    } else {
        format!("{base}/audio/transcriptions")
    };

    let model = if cfg.model.trim().is_empty() {
        default_model()
    } else {
        cfg.model.trim().to_string()
    };

    let part = reqwest::multipart::Part::bytes(audio)
        .file_name(filename.to_string())
        .mime_str(if mime.trim().is_empty() {
            "audio/webm"
        } else {
            mime
        })
        .map_err(|e| format!("音频格式无效：{e}"))?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model)
        .text("response_format", "json");
    if let Some(lang) = cfg.language.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        form = form.text("language", lang.to_string());
    }

    let client = crate::chat::build_client(base).map_err(|e| e.to_string())?;
    let mut req = client
        .post(&endpoint)
        .timeout(Duration::from_secs(120))
        .multipart(form);
    if !cfg.api_key.trim().is_empty() {
        req = req.bearer_auth(cfg.api_key.trim());
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("语音识别请求失败：{e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取语音识别响应失败：{e}"))?;

    if !status.is_success() {
        let brief: String = body.chars().take(300).collect();
        return Err(format!("语音识别服务返回 {status}：{brief}"));
    }

    // 标准返回是 {"text": "..."}；兼容个别实现直接返回纯文本
    let text = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => body,
    };

    // 清干净再返回。**空串不是错误**：一段咳嗽、一次误触发、一句被判定为
    // 幻觉的噪声，结果本来就该是什么都不插入，不该在界面上弹红字。
    let cleaned = clean_transcript(&text);

    // 只有「有内容但被清空了」才记一笔——用户说「我明明说话了却没出字」时，
    // 这份原文是唯一能判断「是 VAD 丢了、还是清洗误杀」的线索。
    // 正常识别不写日志，否则每句话都刷一行，主日志立刻没法看。
    if cleaned.is_empty() && !text.trim().is_empty() {
        let brief: String = text.trim().chars().take(80).collect();
        crate::log_info!("语音识别结果按幻觉丢弃：{brief}");
    }
    Ok(cleaned)
}

/// whisper 的训练语料里有大量带字幕的视频，遇到噪声、静音、句子结尾时它会
/// 「回忆」出语料里的字幕标记——`(字幕:J Chong)(字幕:貝爾)`、`[音乐]`、
/// 「请不吝点赞 订阅 转发」都是这么来的，和用户说的内容毫无关系。
///
/// 这里做三件事，顺序不能换（先剥标记，再按句删样板，最后归一空白）：
///   1. 剥掉括号里的音效/字幕标记，以及「整段话全被括号包着」的输出
///   2. 丢掉命中文案样板的分句
///   3. 压掉多余空白，只剩标点的话当没识别到内容
pub fn clean_transcript(raw: &str) -> String {
    let stripped = strip_noise_brackets(raw);
    let trimmed = drop_hallucinations(&stripped);
    // 繁转简要在幻听词匹配之后：词表里繁简两套都列了，先匹配才能全命中；
    // 转换之后剩下的正文统一成简体（whisper 的中文模型经常吐繁体）。
    let simplified = crate::simplify::to_simplified(&trimmed);
    let squeezed = normalize_punctuation(&squeeze_spaces(&simplified))
        .trim_end_matches("--")
        .trim()
        .to_string();

    // 只剩标点（「，」「。」「...」）等于没内容
    if !squeezed.chars().any(|c| !c.is_whitespace() && !is_punct(c)) {
        return String::new();
    }
    squeezed
}

/// 只做「空白归一 + 标点规范化」，**一句都不丢**。
///
/// 给流式识别用：那个模型自己带标点、也不会吐 `(字幕:xxx)`，所以用不上
/// [`clean_transcript`] 里按句丢幻听那套。而且流式吐的是半句话，
/// 按句删的规则套上去会把半句连同正文一起清掉。
///
/// 只有开了 `stream-stt` 才有调用方，命令行构建下会报 dead_code。
#[cfg_attr(not(feature = "stream-stt"), allow(dead_code))]
pub(crate) fn tidy(text: &str) -> String {
    normalize_punctuation(&squeeze_spaces(text)).trim().to_string()
}

/// 中文文本里的标点规范化。whisper 的中文输出有三个固定毛病：
///   `,` 用半角（实测「幫我把燈錄業改一下,按鈕太大了。」）、
///   `，。` 这类叠在一起的标点、
///   标点后面跟一个空格（换行被压成空格后留下的）。
///
/// 英文句子完全不动：半角标点在英文里就是正确写法，把 `,` 改成 `，` 是错的。
fn normalize_punctuation(text: &str) -> String {
    if !text.chars().any(is_cjk) {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    for (i, &raw) in chars.iter().enumerate() {
        // 只有紧跟在中文后面的半角标点才转全角。`So, my fellow` 里的逗号
        // 是英文的正确写法，跟着一起改就成了 `So， my fellow`。
        let c = if matches!(raw, ',' | '?' | '!' | ';')
            && out.iter().rev().find(|p| **p != ' ').is_some_and(|p| is_cjk(*p))
        {
            match raw {
                ',' => '，',
                '?' => '？',
                '!' => '！',
                _ => '；',
            }
        } else {
            raw
        };

        if c == ' ' {
            // 中文标点前后不该有空格：「改一下， 按钮」读起来是断的
            if out.last().is_some_and(|p| is_cjk_punct(*p)) {
                continue;
            }
            // 中文之间不留空格。whisper 偶尔在语气词后补一个空格
            // （实测「哈啰，你好啊 你是谁」），中文不靠空格分词，留着很扎眼。
            // 中英之间要留：「我要用 VSCode」。
            let prev_zh = out.last().is_some_and(|p| is_cjk(*p));
            let next_zh = chars[i + 1..]
                .iter()
                .find(|x| **x != ' ')
                .is_some_and(|x| is_cjk(*x));
            if prev_zh && next_zh {
                continue;
            }
            out.push(' ');
            continue;
        }

        if is_cjk_punct(c) {
            // 标点前刚压进来的空格要去掉
            while out.last() == Some(&' ') {
                out.pop();
            }
            // 相邻标点只留语气最强的一个：`，。` → `。`、`，，` → `，`
            if let Some(prev) = out.last().copied() {
                if let Some(rank) = punct_rank(prev) {
                    if punct_rank(c).unwrap_or(0) <= rank {
                        continue;
                    }
                    out.pop();
                }
            }
        }

        out.push(c);
    }

    // 中文句子以半角句号收尾时换成全角。只在**文末、且前面是中文**时换：
    // 句中的 `.` 可能是小数点（3.5）或省略号（`...` 是截断标记，前端还要用它判断），
    // 英文句子的 `.` 更是本来就该是半角。
    while out.last() == Some(&' ') {
        out.pop();
    }
    let n = out.len();
    if n > 1 && out[n - 1] == '.' && out[n - 2] != '.' && is_cjk(out[n - 2]) {
        out[n - 1] = '。';
    }

    out.into_iter().collect()
}

/// 语气强度，用来在叠在一起的标点里挑一个
fn punct_rank(c: char) -> Option<u8> {
    match c {
        '。' => Some(3),
        '！' | '？' => Some(2),
        '，' | '；' | '：' => Some(1),
        '、' => Some(0),
        _ => None,
    }
}

fn is_cjk_punct(c: char) -> bool {
    punct_rank(c).is_some() || matches!(c, '（' | '）' | '【' | '】' | '「' | '」' | '『' | '』' | '…' | '—')
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// 括号里出现这些词，说明是幻觉出来的标记而不是人说的话
const NOISE_MARKERS: &[&str] = &[
    "字幕",
    "音乐",
    "背景音",
    "笑",
    "掌声",
    "欢呼",
    "咳嗽",
    "静音",
    "无声",
    "沉默",
    "music",
    "musique",
    "laughter",
    "laughing",
    "applause",
    "silence",
    "blank_audio",
    "inaudible",
    "sound effect",
    "subtitle",
    "背景音乐",
];

/// 整句命中这些片段的，是 whisper 在无语音处的「咒语」，不是用户说的。
///
/// 这些不是凭空猜的：3 秒静音喂进 small-q5_1（中文）会稳定输出
/// 「你不要再說了，我會把你送到你家裡的房間」，440Hz 提示音会输出
/// 「(字幕製作:貝爾)」，白噪声会输出「字幕君心情好」。服务端的
/// `-sns` / `-nth 0.7` 对这些毫无作用（实测），只能在文本层面拦。
///
/// 繁简都列，因为中文 whisper 经常吐繁体。
const HALLUCINATION_PHRASES: &[&str] = &[
    // 字幕组 / 视频尾板
    "字幕制作",
    "字幕製作",
    "字幕君",
    "字幕由",
    "字幕提供",
    "字幕组",
    "字幕組",
    "本字幕",
    "subtitle by",
    "subtitles by",
    "amara.org",
    // 求三连
    "请不吝",
    "請不吝",
    "不吝点赞",
    "不吝點贊",
    "打赏支持",
    "打賞支持",
    "明镜与点点",
    "明鏡與點點",
    "点赞订阅",
    "點贊訂閱",
    "點讚訂閱",
    "订阅频道",
    "訂閱頻道",
    "please subscribe",
    // 视频结尾
    "谢谢观看",
    "謝謝觀看",
    "感谢观看",
    "感謝觀看",
    "谢谢收看",
    "謝謝收看",
    "多谢收看",
    "多謝收看",
    "感谢收看",
    "感謝收看",
    "大家收看",
    "大家的观看",
    "大家的觀看",
    "下期再见",
    "下期再見",
    "我们下期",
    "我們下期",
    "thank you for watching",
    "thanks for watching",
    // 静音段常见的整句幻听
    "你不要再说",
    "你不要再說",
    "不要再说",
    "不要再說",
    "送到你家里的房间",
    "送到你家裡的房間",
];

fn closing_for(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '（' => Some('）'),
        '[' => Some(']'),
        '【' => Some('】'),
        '〔' => Some('〕'),
        _ => None,
    }
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '、'
                | '！'
                | '？'
                | '；'
                | '：'
                | '…'
                | '—'
                | '～'
                | '·'
                | '「'
                | '」'
                | '『'
                | '』'
                | '《'
                | '》'
                | '〈'
                | '〉'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
}

/// 逐字符扫描配对括号，命中关键词的整对删掉。
///
/// 找不到右括号就把左括号当普通字符留下——宁可留着也不吃掉后面的正文。
fn strip_noise_brackets(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    // 括号**外**的非空白字符数。为 0 说明整段输出都包在括号里，
    // 那是幻觉（`(J Chong)(貝爾)`）而不是人说的话。
    let mut outside = 0usize;
    let mut i = 0;

    while i < chars.len() {
        if let Some(close) = closing_for(chars[i]) {
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == close) {
                let inner: String = chars[i + 1..i + 1 + offset].iter().collect();
                if is_noise_marker(&inner) {
                    i += offset + 2;
                    continue;
                }
                // 不是标记：原样保留，但不计入「括号外的正文」
                out.push(chars[i]);
                out.push_str(&inner);
                out.push(close);
                i += offset + 2;
                continue;
            }
        }
        if !chars[i].is_whitespace() {
            outside += 1;
        }
        out.push(chars[i]);
        i += 1;
    }

    // 括号外一个实字都没有：口语里没人这么说话，一定是幻觉；
    // 只要括号外还有内容（`VSCode（不是 Cursor）`）就一个字都不动
    if outside == 0 {
        return String::new();
    }
    out
}

fn is_noise_marker(inner: &str) -> bool {
    let lower = inner.to_lowercase();
    NOISE_MARKERS.iter().any(|m| lower.contains(m))
}

/// 按分句切，命中幻听片段的整句丢掉。
///
/// 半角逗号也要切：whisper 中文模型的输出经常是 `,` 而不是 `，`
/// （实测「你不要再說了,我會把你送到你家裡的房間」就是半角）。
///
/// 还要防「幻觉的尾巴」：`字幕君心情好,謝謝你!` 丢掉前半句后只剩一个
/// `謝謝你!`，看着像人话就留下了。所以再补一条：整段被丢掉的比例超过一半时，
/// 说明这段整体就是幻觉，剩下那点残渣也一起丢。
fn drop_hallucinations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut current = String::new();
    let mut kept_chars = 0usize;
    let mut dropped_chars = 0usize;

    for c in text.chars() {
        current.push(c);
        if matches!(
            c,
            '。' | '！' | '？' | '!' | '?' | '.' | '；' | ';' | '\n' | '，' | ',' | '、'
        ) {
            flush_sentence(&mut out, &mut current, &mut kept_chars, &mut dropped_chars);
        }
    }
    flush_sentence(&mut out, &mut current, &mut kept_chars, &mut dropped_chars);

    if dropped_chars > 0 && dropped_chars * 2 > kept_chars + dropped_chars {
        return String::new();
    }
    out
}

fn flush_sentence(
    out: &mut String,
    current: &mut String,
    kept_chars: &mut usize,
    dropped_chars: &mut usize,
) {
    let len = current.chars().filter(|c| !c.is_whitespace()).count();
    if len == 0 {
        current.clear();
        return;
    }
    let lower = current.to_lowercase();
    if HALLUCINATION_PHRASES.iter().any(|b| lower.contains(b)) {
        *dropped_chars += len;
    } else {
        *kept_chars += len;
        out.push_str(current);
    }
    current.clear();
}

fn squeeze_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        // 控制字符（whisper 偶尔吐 \u{0}）一并丢掉
        if c.is_control() && c != '\n' {
            continue;
        }
        if c.is_whitespace() {
            if last_space {
                continue;
            }
            last_space = true;
            out.push(' ');
            continue;
        }
        last_space = false;
        out.push(c);
    }
    out.trim().to_string()
}

/// base64 → 字节，顺手兼容前端直接传 data URL 的情况
pub fn decode_audio(data: &str) -> Result<Vec<u8>, String> {
    let payload = data
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(data);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("音频数据解码失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::clean_transcript;

    #[test]
    fn drops_subtitle_markers() {
        // 用户实际遇到的那条
        assert_eq!(clean_transcript("(字幕:J Chong)(字幕:貝爾)"), "");
        assert_eq!(clean_transcript("（字幕：貝爾）"), "");
        assert_eq!(clean_transcript("[字幕] 今天天气不错"), "今天天气不错");
    }

    #[test]
    fn keeps_real_words_around_markers() {
        // 剥掉 `(音乐)` `(掌声)` 之后两边的真话要连起来。原来这里留了一个空格，
        // 中文连排不需要——现在 `normalize_punctuation` 会把它删掉。
        assert_eq!(
            clean_transcript("(音乐) 我想改一下登录页 (掌声) 按钮太大了"),
            "我想改一下登录页按钮太大了"
        );
    }

    #[test]
    fn drops_boilerplate_sentences() {
        assert_eq!(clean_transcript("谢谢观看"), "");
        assert_eq!(
            clean_transcript("请不吝点赞 订阅 转发 打赏支持明镜与点点栏目"),
            ""
        );
        // 样板句丢掉，真心话留下
        assert_eq!(
            clean_transcript("帮我把这个函数拆开。谢谢观看"),
            "帮我把这个函数拆开。"
        );
    }

    #[test]
    fn only_brackets_counts_as_empty() {
        // 没有关键词、但整段都是括号里的人名，同样是幻觉
        assert_eq!(clean_transcript("(J Chong)(貝爾)"), "");
    }

    #[test]
    fn normal_speech_is_untouched() {
        assert_eq!(
            clean_transcript("  帮我加个登录页，要用邮箱验证  "),
            "帮我加个登录页，要用邮箱验证"
        );
        // 括号里是正常补充说明时不能误删
        assert_eq!(
            clean_transcript("我用的是 VSCode（不是 Cursor），你按这个来"),
            "我用的是 VSCode（不是 Cursor），你按这个来"
        );
    }

    #[test]
    fn pure_punctuation_is_empty() {
        assert_eq!(clean_transcript("，。..."), "");
    }

    /// 实测原文：3 秒中文语音喂给 small-q5_1，吐出来的是半角逗号
    #[test]
    fn half_width_punctuation_becomes_full_width() {
        assert_eq!(
            clean_transcript("帮我把登录页改一下,按钮太大了,顺便看看有没有别的问题."),
            "帮我把登录页改一下，按钮太大了，顺便看看有没有别的问题。"
        );
        assert_eq!(clean_transcript("真的吗?"), "真的吗？");
        assert_eq!(clean_transcript("太好了!"), "太好了！");
        // 英文句子不动：半角标点在英文里才是正确写法
        assert_eq!(
            clean_transcript("So, my fellow Americans"),
            "So, my fellow Americans"
        );
        // 中英混排：只改跟在中文后面的那个
        assert_eq!(
            clean_transcript("So, my fellow 张伟, how are you"),
            "So, my fellow 张伟，how are you"
        );
        // 中文里的小数点和截断标记不能被当成句号
        assert_eq!(clean_transcript("版本号是 3.5"), "版本号是 3.5");
    }

    /// 换行被压成空格后会留在中文标点后面，读起来是断的
    #[test]
    fn drops_space_after_chinese_punctuation() {
        assert_eq!(
            clean_transcript("帮我把登录页改一下, \n按钮太大了"),
            "帮我把登录页改一下，按钮太大了"
        );
        assert_eq!(clean_transcript("你好 。 你是谁"), "你好。你是谁");
    }

    /// 实测：whisper 在语气词后会补空格，「哈啰，你好啊 你是谁」
    #[test]
    fn drops_space_between_chinese() {
        assert_eq!(clean_transcript("哈啰，你好啊 你是谁"), "哈啰，你好啊你是谁");
        // 中英之间的空格要留着，否则「我要用VSCode」读起来更糟
        assert_eq!(clean_transcript("我要用 VSCode"), "我要用 VSCode");
        assert_eq!(clean_transcript("What are you doing"), "What are you doing");
    }

    #[test]
    fn collapses_doubled_punctuation() {
        assert_eq!(clean_transcript("你好，，世界"), "你好，世界");
        assert_eq!(clean_transcript("好的，。"), "好的。");
        assert_eq!(clean_transcript("真的吗？？"), "真的吗？");
    }

    /// 这几条是实测出来的：把 3 秒静音 / 440Hz 提示音 / 白噪声喂给
    /// small-q5_1（中文），模型稳定吐出这些东西。
    #[test]
    fn drops_measured_hallucinations() {
        assert_eq!(
            clean_transcript("你不要再說了,我會把你送到你家裡的房間"),
            ""
        );
        assert_eq!(clean_transcript("字幕君心情好,謝謝你!"), "");
        assert_eq!(clean_transcript("(好多水)"), "");
        assert_eq!(clean_transcript("(字幕製作:貝爾)"), "");
        // 幻觉后面跟着真话时，真话要留住
        assert_eq!(
            clean_transcript("谢谢观看。把登录页的按钮改成次要样式"),
            "把登录页的按钮改成次要样式"
        );
    }

    /// 真实语音不能被误杀——这条是英文样本转出来的原样
    #[test]
    fn keeps_real_english() {
        assert_eq!(
            clean_transcript("And so, my fellow Americans,"),
            "And so, my fellow Americans,"
        );
    }
}
