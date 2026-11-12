//! 繁体转简体。
//!
//! whisper 的中文模型没有「简体/繁体」这个概念——`language` 只有 `zh` 一个值，
//! 训练语料里繁体（台湾、香港的视频字幕）占比不低，所以它经常吐繁体，
//! 甚至同一句里简繁混用。
//!
//! 试过用 `prompt` 引导（社区常见做法），在 whisper.cpp 上**不可用**：
//! prompt 会被模型当成待续写的前文，短音频直接原样吐回来，音频反倒被忽略。
//! 实测三组样本（静音/提示音/英文语音），加了 prompt 后全部输出「请用简体中文转写。」
//!
//! 所以走确定性的后处理：查表逐字替换。表是 OpenCC 的 TSCharacters，
//! 见 `simplify_table.rs`。代价是极少数多音字会转错（如「乾隆」→「干隆」），
//! 但语音输入场景里这些字几乎不出现，换来的是 100% 稳定输出简体。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::simplify_table::PAIRS;

/// 表是 `繁简繁简…` 连续排布的，第一次用时拆成哈希表，之后常驻。
///
/// 建表要遍历 6442 个字符，大约几十微秒，放在 `OnceLock` 里只为省这一下；
/// 更重要的是别每转一次写一遍。
fn table() -> &'static HashMap<char, char> {
    static TABLE: OnceLock<HashMap<char, char>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let chars: Vec<char> = PAIRS.chars().collect();
        let mut map = HashMap::with_capacity(chars.len() / 2 + 1);
        for pair in chars.chunks_exact(2) {
            map.insert(pair[0], pair[1]);
        }
        map
    })
}

/// 把整段文字里的繁体字换成简体。本来就是简体的部分原样返回。
pub fn to_simplified(text: &str) -> String {
    let map = table();
    // 快路径：一个繁体字都没有（英文、数字、纯简体）就不用重排
    if !text.chars().any(|c| map.contains_key(&c)) {
        return text.to_string();
    }
    text.chars()
        .map(|c| map.get(&c).copied().unwrap_or(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::to_simplified;

    #[test]
    fn converts_the_measured_hallucination() {
        // 3 秒静音喂给中文 small 模型的原始输出，全是繁体
        assert_eq!(
            to_simplified("你不要再說了,我會把你送到你家裡的房間"),
            "你不要再说了,我会把你送到你家里的房间"
        );
    }

    #[test]
    fn converts_common_mixed_text() {
        assert_eq!(
            to_simplified("這個檔案的時間戳記不對，幫我改成系統時區"),
            "这个档案的时间戳记不对，帮我改成系统时区"
        );
    }

    #[test]
    fn leaves_simplified_and_english_alone() {
        assert_eq!(to_simplified("帮我把登录页的按钮改大一点"), "帮我把登录页的按钮改大一点");
        assert_eq!(
            to_simplified("And so, my fellow Americans,"),
            "And so, my fellow Americans,"
        );
        assert_eq!(to_simplified("版本 v1.2.3 的 4000 端口"), "版本 v1.2.3 的 4000 端口");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(to_simplified(""), "");
    }
}
