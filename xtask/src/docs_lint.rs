//! Chinese technical documentation style linter (strict zero-baseline mode).
//! Replaces `scripts/docs-lint.mjs`, `scripts/lib/docs-lint-rules.mjs`, and the empty baseline file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use regex::Regex;
use sha1::{Digest, Sha1};

use crate::markdown::is_fence;
use crate::{err_line, out_line};

const USAGE: &str = "usage: cargo xtask docs-lint [--all] [--summary] [paths...]";
const ROOT_DOCS: &[&str] = &[
    "README.md",
    "README.en.md",
    "AGENTS.md",
    "CONTRIBUTING.md",
    "CHANGELOG.md",
];
const WALK_SKIP: &[&str] = &["prototype", "node_modules", "assets"];

const PARA_ERROR: usize = 300;
const PARA_WARN: usize = 200;
const SENTENCE_ERROR: usize = 120;
const SENTENCE_WARN: usize = 80;
const CELL_ERROR: usize = 80;
const CELL_WARN: usize = 60;
const DUP_MIN_CHARS: usize = 24;
const DENIAL_RUN: usize = 3;
const DENIAL_RATIO: f64 = 0.15;
const DENIAL_PCT: usize = 15;
const PAREN_PER_K: f64 = 3.0;
const SEMI_PER_K: f64 = 3.0;
const PREVIEW_CHARS: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    Adr,
    Changelog,
    Glossary,
    Data,
    Proposal,
    Spec,
}

fn kind_of(f: &str) -> DocKind {
    if f == "docs/decisions.md" {
        DocKind::Adr
    } else if f == "CHANGELOG.md" {
        DocKind::Changelog
    } else if f == "docs/reference/glossary.md" {
        DocKind::Glossary
    } else if f.contains("smoke-checklist") || f.contains("content-bible") {
        DocKind::Data
    } else if f.starts_with("docs/design/") && !f.starts_with("docs/design/content-bible") {
        DocKind::Proposal
    } else {
        DocKind::Spec
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Blank,
    Head,
    Table,
    List,
    Para,
}

struct Row {
    n: usize,
    text: String,
    k: RowKind,
}

struct Block {
    n: usize,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub id: &'static str,
    pub sev: Severity,
    pub msg: String,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Any,
    Prose,
    Head,
    Thead,
}

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

fn cp(s: &str) -> usize {
    s.chars().count()
}

fn preview(s: &str) -> String {
    s.trim().chars().take(PREVIEW_CHARS).collect()
}

fn unlink(s: &str, link_re: &Regex) -> String {
    link_re.replace_all(s, "]").replace("**", "")
}

fn strip_code(s: &str, code_re: &Regex) -> String {
    code_re.replace_all(s, "").into_owned()
}

fn classify_rows(lines: &[&str]) -> Vec<Row> {
    let Ok(head_re) = Regex::new(r"^#{1,6}\s") else {
        return Vec::new();
    };
    let Ok(list_re) = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+") else {
        return Vec::new();
    };
    let mut in_code = false;
    let mut rows = Vec::with_capacity(lines.len());
    for (i, &text) in lines.iter().enumerate() {
        if is_fence(text) {
            in_code = !in_code;
            continue;
        }
        let k = if in_code || text.trim().is_empty() {
            RowKind::Blank
        } else if head_re.is_match(text) {
            RowKind::Head
        } else if text.trim_start().starts_with('|') {
            RowKind::Table
        } else if list_re.is_match(text) {
            RowKind::List
        } else {
            RowKind::Para
        };
        rows.push(Row {
            n: i + 1,
            text: text.to_owned(),
            k,
        });
    }
    rows
}

fn blocks_of(rows: &[Row]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur: Option<Block> = None;
    for r in rows {
        match r.k {
            RowKind::Blank | RowKind::Head | RowKind::Table => {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
            }
            RowKind::List => {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
                cur = Some(Block {
                    n: r.n,
                    text: r.text.trim().to_owned(),
                });
            }
            RowKind::Para => {
                if let Some(ref mut b) = cur {
                    b.text.push('\n');
                    b.text.push_str(r.text.trim());
                } else {
                    cur = Some(Block {
                        n: r.n,
                        text: r.text.trim().to_owned(),
                    });
                }
            }
        }
    }
    if let Some(b) = cur {
        blocks.push(b);
    }
    blocks
}

fn add_hit(
    hits: &mut Vec<Hit>,
    file: &str,
    line: usize,
    id: &'static str,
    sev: Severity,
    msg: String,
    text: &str,
) {
    // Keep deterministic hash logic available if ever needed for reporting.
    let _ = Sha1::digest(text.trim().as_bytes());
    hits.push(Hit {
        file: file.to_owned(),
        line,
        id,
        sev,
        msg,
        text: preview(text),
    });
}

struct LengthLimit {
    id: &'static str,
    error: usize,
    warn: usize,
    label: &'static str,
}

const W01_LIMIT: LengthLimit = LengthLimit {
    id: "W01",
    error: PARA_ERROR,
    warn: PARA_WARN,
    label: "段落/列表项",
};
const W02_LIMIT: LengthLimit = LengthLimit {
    id: "W02",
    error: SENTENCE_ERROR,
    warn: SENTENCE_WARN,
    label: "句子",
};
const C01_LIMIT: LengthLimit = LengthLimit {
    id: "C01",
    error: CELL_ERROR,
    warn: CELL_WARN,
    label: "表格单元格",
};

fn add_length_hit(
    hits: &mut Vec<Hit>,
    file: &str,
    line: usize,
    len: usize,
    limit: &LengthLimit,
    text: &str,
) {
    if len > limit.error {
        add_hit(
            hits,
            file,
            line,
            limit.id,
            Severity::Error,
            format!("{} {len} 字（上限 {}）", limit.label, limit.error),
            text,
        );
    } else if len > limit.warn {
        add_hit(
            hits,
            file,
            line,
            limit.id,
            Severity::Warn,
            format!("{} {len} 字（建议 ≤{}）", limit.label, limit.warn),
            text,
        );
    }
}

/// Lookaround-free exact matchers for the few rules that originally used JS lookahead/lookbehind.
fn matches_d01(t: &str, base_re: &Regex) -> bool {
    if base_re.is_match(t) {
        return true;
    }
    // `(?:^|[，。；（(])本文(?!件)`
    for (idx, _) in t.match_indices("本文") {
        let prev_ok = idx == 0
            || t[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| matches!(c, '，' | '。' | '；' | '（' | '('));
        let next_is_jian = t[idx + "本文".len()..].starts_with('件');
        if prev_ok && !next_is_jian {
            return true;
        }
    }
    false
}

fn matches_d02(t: &str, base_re: &Regex) -> bool {
    if base_re.is_match(t) {
        return true;
    }
    // `(?<![上以])下文(?![件本字案档章书学化明])`
    const SKIP_NEXT: &[char] = &['件', '本', '字', '案', '档', '章', '书', '学', '化', '明'];
    for (idx, _) in t.match_indices("下文") {
        let prev_skip = t[..idx]
            .chars()
            .next_back()
            .is_some_and(|c| matches!(c, '上' | '以'));
        let next_skip = t[idx + "下文".len()..]
            .chars()
            .next()
            .is_some_and(|c| SKIP_NEXT.contains(&c));
        if !prev_skip && !next_skip {
            return true;
        }
    }
    false
}

fn matches_b01(t: &str, b01_re: &Regex) -> bool {
    let Some(caps) = b01_re.captures(t) else {
        return false;
    };
    let word = caps[1].trim();
    !matches!(word, "限制" | "原因" | "影响" | "绕过")
}

fn matches_h01(t: &str, base_re: &Regex) -> bool {
    if base_re.is_match(t) {
        return true;
    }
    // `原来(?!的)`
    for (idx, _) in t.match_indices("原来") {
        if !t[idx + "原来".len()..].starts_with('的') {
            return true;
        }
    }
    false
}

fn matches_j01(t: &str, base_re: &Regex) -> bool {
    if base_re.is_match(t) {
        return true;
    }
    for (idx, _) in t.match_indices("拥有") {
        if !t[idx + "拥有".len()..].starts_with('者') {
            return true;
        }
    }
    for (idx, _) in t.match_indices("暴露") {
        if !t[idx + "暴露".len()..].starts_with('面') {
            return true;
        }
    }
    false
}

fn matches_t01(t: &str, base_re: &Regex) -> bool {
    if base_re.is_match(t) {
        return true;
    }
    const ALLOW: &[&str] = &["网络", "运行时", "上游", "数据库", "服务商", "客户端"];
    for (idx, _) in t.match_indices("真实") {
        let rest = &t[idx + "真实".len()..];
        if !ALLOW.iter().any(|w| rest.starts_with(w)) {
            return true;
        }
    }
    false
}

fn split_sentences(flat: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (idx, c) in flat.char_indices() {
        if matches!(c, '。' | '！' | '？') {
            let end = idx + c.len_utf8();
            let s = flat[start..end].trim();
            if has_cjk(s) {
                out.push(s);
            }
            start = end;
        } else if c == '\n' {
            let s = flat[start..idx].trim();
            if has_cjk(s) {
                out.push(s);
            }
            start = idx + 1;
        }
    }
    let tail = flat[start..].trim();
    if has_cjk(tail) {
        out.push(tail);
    }
    out
}

fn split_unescaped_pipes(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            parts.push(&s[start..i]);
            start = i + 1;
        }
    }
    parts.push(&s[start..]);
    parts
}

struct CompiledRules {
    link_re: Regex,
    code_re: Regex,
    quote_re: Regex,
    table_rule_re: Regex,
    paren_re: Regex,
    paren_content_re: Regex,
    norm_strip_re: Regex,
    denial_re: Regex,
    d01_base: Regex,
    d02_base: Regex,
    n01: Regex,
    n02: Regex,
    n03: Regex,
    n04: Regex,
    n05: Regex,
    e01: Regex,
    b01: Regex,
    p01: Regex,
    s01: Regex,
    h01_base: Regex,
    j01_base: Regex,
    l01: Regex,
    u01: Regex,
    l02: Regex,
    l03: Regex,
    t01_base: Regex,
}

impl CompiledRules {
    fn compile() -> Result<Self, regex::Error> {
        Ok(Self {
            link_re: Regex::new(r"\]\([^)]*\)")?,
            code_re: Regex::new(r"`[^`]*`")?,
            quote_re: Regex::new(r"「[^」\n]*」")?,
            table_rule_re: Regex::new(r"^\s*\|[\s:|-]+\|?\s*$")?,
            paren_re: Regex::new(r"（[^（）\n]*）|\([^()\n]*\)")?,
            paren_content_re: Regex::new(r"[^\s（）()、，,；;和与或及]")?,
            norm_strip_re: Regex::new(
                r#"[，。；：、！？（）()「」“”"'‘’《》\[\]{}<>·…—~,.;:!?/\\*\-\s|#>_]"#,
            )?,
            denial_re: Regex::new(
                r"不是|不能|不会|不再|不做|不要|不得|不应|不可|不需要|不支持|不提供|不等于|不代表|不保证|不承诺|没有|无法|并非|绝不|也不|不负责|不覆盖|不包含|不依赖|不影响|不改变|不处理|不构成|不意味|不证明|不替代",
            )?,
            d01_base: Regex::new(
                r"(?:^|[，。；（(])(?:本节|本章|本方案|本设计|本基线|本篇|本说明|这份文件|这份文档|本文件)",
            )?,
            d02_base: Regex::new(
                r"上文|前文|后文|见下[^\u{4E00}-\u{9FFF}]|见上[^\u{4E00}-\u{9FFF}]|下表|上表",
            )?,
            n01: Regex::new(r"不是[^。！？\n；]{1,60}?而是|并非")?,
            n02: Regex::new(r"不等于|不等同|不代表|不意味|不同于")?,
            n03: Regex::new(
                r"(?:不能|不得|不应|不要|不可|无法)(?:把|将)?[^。！？\n；，]{0,30}?(?:当作|当成|等同于?|冒充|视为|算作|宣称|宣传|称为)",
            )?,
            n04: Regex::new(r"不承诺|不保证|无法保证|不证明|不构成|不替代|不能证明|不能替代")?,
            n05: Regex::new(
                r"^(?:不意味着什么|不应推断|不负责|不承担的职责|不能保证的部分|不能证明|不该放什么|不自动发生什么)$",
            )?,
            e01: Regex::new(
                r"^\s*(?:[-*]\s+)?\*{0,2}(?:证据|查证入口|查证|证据入口|验收|当前实现|反例)\*{0,2}[：:]|(?:^|。|；)\s*(?:证据为|对应证据在|查证：)|^\s*(?:[^\s。]{0,20}的)?测试(?:（[^）]*）)?[：:]\s*$",
            )?,
            b01: Regex::new(
                r"^\s*(?:(?:[-*+]|\d+[.)])\s+)?\*\*([^*\n]{1,30})(?:[：:。，、]\*\*|\*\*\s*[：:。，、])",
            )?,
            p01: Regex::new(r"（[^（）\n]{16,}）|\([^()\n]{16,}\)")?,
            s01: Regex::new(r"(?:[^；\n]*；){3,}")?,
            h01_base: Regex::new(
                r"原先|此前|以前|旧版|旧的|现已|已删除|已移除|已被取代|取代了|不再|改为|落地时|实施时|本轮|这一轮|当时|后来|(?:从|自) ?(?:schema )?v\d{2,3}(?: 起)?|schema v\d{2,3}|迁移 v\d{2,3}|v\d{2,3} 加的|和以前|照旧",
            )?,
            j01_base: Regex::new(
                r"闭环|作用域|承诺等级|证明边界|证明范围|唯一职责|事实源|口径|载体|粒度|落地|收口|收敛|归一|兜底|保真|有界|取证|可复核|钉住|所有权|不变量|判据|承载|收窄",
            )?,
            l01: Regex::new(
                r"(?:另一位|其他|别的|负责[^，。；]{0,10}的) ?worker|worker 负责|worker 修改|任务要求|评审稿|批准时的方案|另一位|另一个工作流|用户同意|用户确认了|frontend-design|STATUS\.md|本轮不做|这次不做|\b[A-Z]{2,}-D\d+\b|\bTRM-\d+\b|\.overhaul/",
            )?,
            u01: Regex::new(r"未核实|待核实|没有核实|未逐.核对|未对照代码")?,
            l02: Regex::new(r"\.(?:rs|ts|tsx|md|mjs|css):\d+|行号是|第 ?\d+ 行")?,
            l03: Regex::new(r"/tmp/[\w.<>*-]+(?:/[\w.<>*-]+)*\.(?:png|jpg|jpeg|svg|md|json|log)")?,
            t01_base: Regex::new(r"到底|先分清|先问|不是|不拥有|不共享|不假装|不宣传|分别取证")?,
        })
    }
}

struct FileStats {
    file: String,
    kind: DocKind,
    denial: f64,
    paren: f64,
    semi: f64,
}

fn check_rule_line(
    rules: &CompiledRules,
    file: &str,
    kind: DocKind,
    rows: &[Row],
    hits: &mut Vec<Hit>,
) {
    type Matcher = fn(&CompiledRules, &str) -> bool;
    struct RuleDef {
        id: &'static str,
        sev: Severity,
        target: Target,
        strip: bool,
        raw: bool,
        skip: &'static [DocKind],
        msg: &'static str,
        check: Matcher,
    }

    const DEFS: &[RuleDef] = &[
        RuleDef {
            id: "D01",
            sev: Severity::Error,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "文档谈论自己（本文/本节/本方案…）",
            check: |r, t| matches_d01(t, &r.d01_base),
        },
        RuleDef {
            id: "D02",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "章节指代（下文/上文/见下/下表）",
            check: |r, t| matches_d02(t, &r.d02_base),
        },
        RuleDef {
            id: "N01",
            sev: Severity::Error,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "否定框架：不是…而是 / 并非",
            check: |r, t| r.n01.is_match(t),
        },
        RuleDef {
            id: "N02",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "否定框架：不等于/不代表/不意味/不同于",
            check: |r, t| r.n02.is_match(t),
        },
        RuleDef {
            id: "N03",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "否定框架：不能把…当作/冒充/视为/宣称",
            check: |r, t| r.n03.is_match(t),
        },
        RuleDef {
            id: "N04",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "否定框架：不承诺/不保证/不证明/不构成/不替代",
            check: |r, t| r.n04.is_match(t),
        },
        RuleDef {
            id: "N05",
            sev: Severity::Error,
            target: Target::Thead,
            strip: true,
            raw: false,
            skip: &[],
            msg: "表头里的否定列",
            check: |r, t| r.n05.is_match(t),
        },
        RuleDef {
            id: "E01",
            sev: Severity::Error,
            target: Target::Any,
            strip: false,
            raw: false,
            skip: &[],
            msg: "证据脚注（证据：/查证：/验收：/当前实现：）",
            check: |r, t| r.e01.is_match(t),
        },
        RuleDef {
            id: "B01",
            sev: Severity::Warn,
            target: Target::Prose,
            strip: false,
            raw: true,
            skip: &[DocKind::Adr],
            msg: "粗体开头的段落或列表项",
            check: |r, t| matches_b01(t, &r.b01),
        },
        RuleDef {
            id: "P01",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "长括号旁白（≥16 字）",
            check: |r, t| r.p01.is_match(t),
        },
        RuleDef {
            id: "S01",
            sev: Severity::Warn,
            target: Target::Prose,
            strip: true,
            raw: false,
            skip: &[DocKind::Data],
            msg: "分号链（一行 ≥3 个全角分号）",
            check: |r, t| r.s01.is_match(t),
        },
        RuleDef {
            id: "H01",
            sev: Severity::Error,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[DocKind::Adr, DocKind::Changelog],
            msg: "版本史措辞（应放 CHANGELOG 或决策记录）",
            check: |r, t| matches_h01(t, &r.h01_base),
        },
        RuleDef {
            id: "J01",
            sev: Severity::Warn,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[DocKind::Glossary],
            msg: "抽象黑话（换成具体说法，或登记进术语表）",
            check: |r, t| matches_j01(t, &r.j01_base),
        },
        RuleDef {
            id: "L01",
            sev: Severity::Error,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "过程日志泄漏（worker/任务要求/评审稿/用户同意…）",
            check: |r, t| r.l01.is_match(t),
        },
        RuleDef {
            id: "U01",
            sev: Severity::Error,
            target: Target::Any,
            strip: true,
            raw: false,
            skip: &[],
            msg: "作者的核实状态（未核实/待核实）进了入库文档，按指南 §14 改写或删掉",
            check: |r, t| r.u01.is_match(t),
        },
        RuleDef {
            id: "L02",
            sev: Severity::Error,
            target: Target::Any,
            strip: false,
            raw: false,
            skip: &[],
            msg: "行号引用（会漂移）",
            check: |r, t| r.l02.is_match(t),
        },
        RuleDef {
            id: "L03",
            sev: Severity::Error,
            target: Target::Any,
            strip: false,
            raw: false,
            skip: &[],
            msg: "引用仓库外的临时文件",
            check: |r, t| r.l03.is_match(t),
        },
        RuleDef {
            id: "T01",
            sev: Severity::Warn,
            target: Target::Head,
            strip: true,
            raw: false,
            skip: &[],
            msg: "标题腔调（到底/先分清/先问/不是/真实…）",
            check: |r, t| matches_t01(t, &r.t01_base),
        },
    ];

    for (idx, r) in rows.iter().enumerate() {
        if r.k == RowKind::Blank {
            continue;
        }
        for def in DEFS {
            if def.skip.contains(&kind) {
                continue;
            }
            if def.target == Target::Head && r.k != RowKind::Head {
                continue;
            }
            if def.target == Target::Prose && !matches!(r.k, RowKind::Para | RowKind::List) {
                continue;
            }
            let mut t = if def.raw {
                r.text.clone()
            } else {
                unlink(&r.text, &rules.link_re)
            };
            if def.strip {
                let no_code = strip_code(&t, &rules.code_re);
                t = rules.quote_re.replace_all(&no_code, "「」").into_owned();
            }
            if def.target == Target::Thead {
                if r.k != RowKind::Table || (idx > 0 && rows[idx - 1].k == RowKind::Table) {
                    continue;
                }
                let trimmed = t.trim().trim_start_matches('|').trim_end_matches('|');
                if trimmed.split('|').any(|c| (def.check)(rules, c.trim())) {
                    add_hit(
                        hits,
                        file,
                        r.n,
                        def.id,
                        def.sev,
                        def.msg.to_owned(),
                        &r.text,
                    );
                }
                continue;
            }
            if (def.check)(rules, &t) {
                add_hit(
                    hits,
                    file,
                    r.n,
                    def.id,
                    def.sev,
                    def.msg.to_owned(),
                    &r.text,
                );
            }
        }
    }
}

pub fn lint_docs(docs: &[(String, String)]) -> Result<Vec<Hit>, String> {
    let rules = CompiledRules::compile().map_err(|e| format!("docs-lint regex error: {e}"))?;
    let mut hits = Vec::new();
    let mut dup_index: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    let mut file_stats = Vec::new();

    for (file, text) in docs {
        let kind = kind_of(file);
        let lines: Vec<&str> = text.split('\n').collect();
        let rows = classify_rows(&lines);
        let non_blank: String = rows
            .iter()
            .filter(|r| r.k != RowKind::Blank)
            .map(|r| r.text.as_str())
            .collect();
        let k_raw = cp(&strip_code(&non_blank, &rules.code_re)) as f64 / 1000.0;
        let k_units = if k_raw > 0.0 { k_raw } else { 1.0 };

        check_rule_line(&rules, file, kind, &rows, &mut hits);

        // Blocks: W01, W02, N06, R01, parens/semis density.
        let mut sentences_count = 0usize;
        let mut denial_count = 0usize;
        let mut parens_count = 0usize;
        let mut semis_count = 0usize;
        for b in blocks_of(&rows) {
            let flat = unlink(&b.text, &rules.link_re);
            let flat_no_nl = flat.replace('\n', "");
            add_length_hit(&mut hits, file, b.n, cp(&flat_no_nl), &W01_LIMIT, &b.text);

            let mut run = 0usize;
            for s in split_sentences(&flat) {
                sentences_count += 1;
                let s_counted = rules.code_re.replace_all(s, "码");
                add_length_hit(&mut hits, file, b.n, cp(&s_counted), &W02_LIMIT, s);
                if rules.denial_re.is_match(s) {
                    denial_count += 1;
                    run += 1;
                    if run == DENIAL_RUN {
                        add_hit(
                            &mut hits,
                            file,
                            b.n,
                            "N06",
                            Severity::Warn,
                            format!("连续 {DENIAL_RUN} 句含否认词"),
                            s,
                        );
                    }
                } else {
                    run = 0;
                }
                let norm = rules
                    .norm_strip_re
                    .replace_all(&strip_code(s, &rules.code_re), "")
                    .into_owned();
                if cp(&norm) >= DUP_MIN_CHARS && kind != DocKind::Adr {
                    dup_index.entry(norm).or_default().push((file.clone(), b.n));
                }
            }
            let nc = strip_code(&flat, &rules.code_re);
            parens_count += rules
                .paren_re
                .find_iter(&nc)
                .filter(|m| rules.paren_content_re.is_match(m.as_str()))
                .count();
            semis_count += nc.matches('；').count();
        }

        // Table cells: C01.
        for r in &rows {
            if r.k != RowKind::Table || rules.table_rule_re.is_match(&r.text) {
                continue;
            }
            let row_unlinked = unlink(&r.text, &rules.link_re);
            let trimmed = row_unlinked
                .trim()
                .trim_start_matches('|')
                .trim_end_matches('|');
            for cell in split_unescaped_pipes(trimmed) {
                add_length_hit(&mut hits, file, r.n, cp(cell.trim()), &C01_LIMIT, cell);
            }
        }

        file_stats.push(FileStats {
            file: file.clone(),
            kind,
            denial: denial_count as f64 / sentences_count.max(1) as f64,
            paren: parens_count as f64 / k_units,
            semi: semis_count as f64 / k_units,
        });
    }

    // R01 cross-file duplicates.
    for (_norm, where_list) in dup_index {
        let distinct_files: HashSet<&str> = where_list.iter().map(|(f, _)| f.as_str()).collect();
        if distinct_files.len() < 2 {
            continue;
        }
        let shown = where_list
            .iter()
            .map(|(f, n)| format!("{f}:{n}"))
            .collect::<Vec<_>>()
            .join(" ⇄ ");
        for (f, n) in &where_list {
            add_hit(
                &mut hits,
                f,
                *n,
                "R01",
                Severity::Error,
                "同一句话出现在多个文件".to_owned(),
                &shown,
            );
        }
    }

    // Per-file density warnings.
    for st in file_stats {
        if !matches!(st.kind, DocKind::Spec | DocKind::Proposal) {
            continue;
        }
        if st.denial > DENIAL_RATIO {
            add_hit(
                &mut hits,
                &st.file,
                1,
                "N07",
                Severity::Warn,
                format!(
                    "含否认词的句子占 {:.0}%（建议 ≤{DENIAL_PCT}%）",
                    100.0 * st.denial
                ),
                &st.file,
            );
        }
        if st.paren > PAREN_PER_K {
            add_hit(
                &mut hits,
                &st.file,
                1,
                "P02",
                Severity::Warn,
                format!("括号 {:.1}/千字（建议 ≤{PAREN_PER_K:.0}）", st.paren),
                &st.file,
            );
        }
        if st.semi > SEMI_PER_K {
            add_hit(
                &mut hits,
                &st.file,
                1,
                "S02",
                Severity::Warn,
                format!("分号 {:.1}/千字（建议 ≤{SEMI_PER_K:.0}）", st.semi),
                &st.file,
            );
        }
    }

    Ok(hits)
}

fn walk_md(root: &Path, rel_dir: &str, out: &mut Vec<String>) {
    let abs = root.join(rel_dir);
    let Ok(entries) = fs::read_dir(&abs) else {
        return;
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let rel = format!("{rel_dir}/{name_str}");
        let path = entry.path();
        if path.is_dir() {
            if !WALK_SKIP.contains(&name_str) {
                walk_md(root, &rel, out);
            }
        } else if name_str.ends_with(".md") {
            out.push(rel);
        }
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut show_all = false;
    let mut summary = false;
    let mut paths: Vec<String> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--all" => show_all = true,
            "--summary" => summary = true,
            a if a.starts_with('-') => {
                err_line(&format!("unexpected flag '{a}'\n{USAGE}"));
                return ExitCode::from(2);
            }
            p => paths.push(p.to_owned()),
        }
    }

    let doc_paths: Vec<String> = if !paths.is_empty() {
        for p in &paths {
            if !Path::new(p).exists() && !root.join(p).exists() {
                err_line(&format!("no such file: {p}\n{USAGE}"));
                return ExitCode::from(2);
            }
        }
        paths
    } else {
        let mut all: Vec<String> = ROOT_DOCS
            .iter()
            .filter(|f| root.join(f).exists())
            .map(|&s| s.to_owned())
            .collect();
        walk_md(root, "docs", &mut all);
        all
    };

    let mut docs = Vec::with_capacity(doc_paths.len());
    for p in doc_paths {
        let abs = if Path::new(&p).exists() {
            Path::new(&p).to_path_buf()
        } else {
            root.join(&p)
        };
        match fs::read_to_string(&abs) {
            Ok(text) => docs.push((p, text)),
            Err(e) => {
                err_line(&format!("docs-lint: cannot read {p}: {e}"));
                return ExitCode::FAILURE;
            }
        }
    }

    let hits = match lint_docs(&docs) {
        Ok(h) => h,
        Err(e) => {
            err_line(&e);
            return ExitCode::FAILURE;
        }
    };
    let shown: Vec<&Hit> = hits
        .iter()
        .filter(|h| h.sev == Severity::Error || show_all)
        .collect();

    if !summary {
        for h in &shown {
            out_line(&format!(
                "{}:{}  [{}] {}  {}  | {}",
                h.file,
                h.line,
                h.id,
                h.sev.as_str(),
                h.msg,
                h.text
            ));
        }
    }
    if !shown.is_empty() {
        let mut by: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for h in &shown {
            let entry = by.entry(h.id).or_insert((0, 0));
            match h.sev {
                Severity::Error => entry.0 += 1,
                Severity::Warn => entry.1 += 1,
            }
        }
        err_line("");
        for (id, (e, w)) in by {
            err_line(&format!("{id}: {e} error / {w} warn"));
        }
    }
    let errors = shown.iter().filter(|h| h.sev == Severity::Error).count();
    err_line(&format!(
        "docs-lint: {errors} error(s) in {} files",
        docs.len()
    ));
    if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_ids(docs: &[(&str, &str)]) -> Vec<&'static str> {
        let owned: Vec<(String, String)> = docs
            .iter()
            .map(|(f, t)| ((*f).to_owned(), (*t).to_owned()))
            .collect();
        lint_docs(&owned)
            .unwrap()
            .into_iter()
            .filter(|h| h.sev == Severity::Error)
            .map(|h| h.id)
            .collect()
    }

    #[test]
    fn enforces_style_rules_and_exemptions() {
        assert!(error_ids(&[("docs/a.md", "本件按规则执行。\n")]).is_empty());
        assert_eq!(
            error_ids(&[("docs/a.md", "本文说明模块边界。\n")]),
            vec!["D01"]
        );
        assert_eq!(
            error_ids(&[("docs/a.md", "这不是演示而是核心。\n")]),
            vec!["N01"]
        );
        assert_eq!(
            error_ids(&[("docs/a.md", "旧版接口已经删除。\n")]),
            vec!["H01"]
        );
        // CHANGELOG exempts H01.
        assert!(error_ids(&[("CHANGELOG.md", "旧版接口已经删除。\n")]).is_empty());
        // Code spans and 「…」 UI copy are stripped for strip=true rules.
        assert!(error_ids(&[("docs/a.md", "界面显示「并非如此」，详见 `旧版`。\n")]).is_empty());
        // Cross-file duplicate R01.
        let dup = "这是一段超过二十四个字的完全相同的中文说明语句用来触发跨文件重复检测。";
        let ids = error_ids(&[("docs/a.md", dup), ("docs/b.md", dup)]);
        assert_eq!(ids, vec!["R01", "R01"]);
    }
}
