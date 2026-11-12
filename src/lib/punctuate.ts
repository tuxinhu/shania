/**
 * 语音输入的标点补全。
 *
 * whisper 对中文**不是不给标点**——实测 2 秒以上的段它基本都会给（`帮我把登录页
 * 改一下。`）。真正的缺口是这三种：
 *
 *   1. 1.5 秒以下的短段经常一个标点都没有
 *   2. 它给的是半角 `,`（Rust 侧 `normalize_punctuation` 已经转成全角）
 *   3. 段与段拼接时，两段都没标点就直接粘成一长串——用户看到的那句
 *      「你好你是谁你在干什么你好啊你是谁」就是这么来的
 *
 * 所以这里只做一件事：**每段定稿时补一个段末标点**，让拼接处天然断开。
 * 段内不会凭空插逗号（插错了比没有更糟），那部分交给模型自己。
 */

/** 结尾是这些，基本可以断定是在提问 */
const QUESTION_TAILS = [
  '吗',
  '嘛',
  '呢',
  '什么',
  '啥',
  '怎么',
  '怎么样',
  '怎样',
  '为什么',
  '为啥',
  '如何',
  '谁',
  '哪个',
  '哪里',
  '哪儿',
  '多少',
  '多久',
  '多大',
  '多远',
  '几点',
  '几个',
  '是不是',
  '能不能',
  '可不可以',
  '有没有',
  '对不对',
  '好不好',
  '行不行',
  '要不要',
  '会不会',
  '干什么',
  '干嘛',
  '干啥',
]

/** 结尾是这些感叹词，句末用叹号。刻意收得很窄——叹号用多了整段像在吵架 */
const EXCLAIM_TAILS = ['啊', '呀', '啦', '哇', '哟']

const CJK_END = /[\u4e00-\u9fff]$/u
const CJK_START = /^[\u4e00-\u9fff]/u
/** 中文句末/句中标点：以它们收尾说明这一段已经闭合，不用再补 */
const HAS_ANY_END = /[。！？，、；：…]$/u
const HAS_ASCII_END = /[.!?,;:]$/
/** 会话结束时该换成句号的「没说完」后缀 */
const OPEN_END = /[，、；：]$/u

export type Tone = 'question' | 'exclaim' | 'statement'

function tail(text: string, n: number): string {
  const chars = [...text]
  return chars.slice(Math.max(0, chars.length - n)).join('')
}

/** 判断这句话是什么语气。只看结尾——整句扫「什么」会把「我知道你在哪里」判成疑问 */
export function detectTone(text: string): Tone {
  const clean = text.replace(/[\s，、；：]+$/u, '')
  if (!clean) return 'statement'

  for (const q of QUESTION_TAILS) {
    if (clean.endsWith(q)) return 'question'
  }

  for (const e of EXCLAIM_TAILS) {
    if (clean.endsWith(e)) {
      // 「你说什么啊」是疑问不是感叹：前面带着疑问词就按疑问算
      const head = clean.slice(0, -e.length)
      if (QUESTION_TAILS.some((q) => tail(head, 8).endsWith(q))) return 'question'
      return 'exclaim'
    }
  }
  return 'statement'
}

/**
 * 一段话定稿时调用：末尾没有标点就补一个。
 *
 * 段短的时候补「，」而不是「。」——因为后面大概率还有话，用句号会把一句话
 * 切碎（用户断句感受比标点规范更重要）。说得够长的一整段才用句号收。
 */
export function closeSegment(text: string, durationMs = 0): string {
  const t = text.trim()
  if (!t) return ''
  if (HAS_ANY_END.test(t) || HAS_ASCII_END.test(t)) return t

  switch (detectTone(t)) {
    case 'question':
      return `${t}？`
    case 'exclaim':
      return `${t}！`
    default:
      break
  }

  // 说满 5 秒、字数也够，才认为是一整句说完了
  const chars = t.replace(/[^\u4e00-\u9fffA-Za-z0-9]/gu, '').length
  if (durationMs >= 5000 && chars >= 8) return `${t}。`
  return `${t}，`
}

/**
 * 一次语音输入结束时调用：把结尾那个「，」换成句号。
 *
 * 定稿时一律补「，」是因为当时不知道用户还会不会继续说；会话结束才确定
 * 这是最后一句，改回来即可（只动结尾一个字符，中间的不碰）。
 */
export function closeSession(text: string): string {
  const t = text.replace(/\s+$/u, '')
  if (!t) return ''
  if (HAS_ANY_END.test(t)) {
    if (!OPEN_END.test(t)) return t
    const head = t.slice(0, -1)
    return `${head}${detectTone(head) === 'question' ? '？' : '。'}`
  }
  if (HAS_ASCII_END.test(t)) return t
  return detectTone(t) === 'question' ? `${t}？` : `${t}。`
}

/**
 * 两段语音之间怎么接。
 *
 * 中文靠标点断句，不需要空格；英文必须补空格，否则 `American` 和 `And`
 * 会粘成 `AmericanAnd`。
 */
export function joinVoice(prev: string, next: string): string {
  if (!prev) return next
  if (!next) return prev
  // 中文标点收尾：直接接
  if (/[，。！？；：、…）】」』]$/u.test(prev)) return prev + next
  // 英文标点收尾：英文单词之间要空格
  if (/[,.;:!?]$/.test(prev)) return /^[A-Za-z0-9]/.test(next) ? `${prev} ${next}` : prev + next
  if (/^[，。！？；：、…（【「『]/u.test(next)) return prev + next
  // 两边都是中文字符：直接拼
  if (CJK_END.test(prev) && CJK_START.test(next)) return prev + next
  return `${prev} ${next}`
}
