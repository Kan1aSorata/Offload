//! 归档目录模板：用 `/` 分层，每层可混写文字与变量，如 `{year}/{month}/{day}/{camera}`。
//!
//! 预设的几种归档方式也是模板，见 [`crate::config::FolderLayout::preset`]。

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

/// 可用变量：(名称, 说明)。界面与报错共用这张表。
pub const VARIABLES: &[(&str, &str)] = &[
    ("year", "年，如 2026"),
    ("month", "月，如 10"),
    ("day", "日，如 20"),
    ("date", "日期，如 2026-10-20"),
    ("camera", "相机型号，如 Canon EOS R5"),
    ("card", "卡名，如 EOS_DIGITAL"),
];

/// 默认模板，也是模板无效时的兜底
pub const DEFAULT_TEMPLATE: &str = "{year}/{month}/{day}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Var {
    Year,
    Month,
    Day,
    Date,
    Camera,
    Card,
}

impl Var {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "year" => Self::Year,
            "month" => Self::Month,
            "day" => Self::Day,
            "date" => Self::Date,
            "camera" => Self::Camera,
            "card" => Self::Card,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Var(Var),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// `{` 没有对应的 `}`
    Unclosed,
    /// 多出来的 `}`
    StrayClose,
    UnknownVariable(String),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unclosed => write!(f, "有一个「{{」没有闭合"),
            Self::StrayClose => write!(f, "多了一个「}}」"),
            Self::UnknownVariable(v) => {
                let names: Vec<String> = VARIABLES.iter().map(|(n, _)| format!("{{{n}}}")).collect();
                write!(f, "不认识的变量 {{{v}}}，可用：{}", names.join(" "))
            }
        }
    }
}

impl std::error::Error for TemplateError {}

/// 一组文件的归档信息
pub struct Vars<'a> {
    pub date: NaiveDate,
    pub camera: &'a str,
    pub card: &'a str,
}

/// 解析后的模板。空层（如开头、结尾或连续的 `/`）会被忽略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTemplate {
    levels: Vec<Vec<Piece>>,
}

impl Default for FolderTemplate {
    fn default() -> Self {
        Self::parse(DEFAULT_TEMPLATE).expect("默认模板有效")
    }
}

impl FolderTemplate {
    pub fn parse(s: &str) -> Result<Self, TemplateError> {
        let mut levels = Vec::new();
        for level in s.split('/') {
            let pieces = parse_level(level)?;
            let blank = pieces.iter().all(|p| matches!(p, Piece::Text(t) if t.trim().is_empty()));
            if !blank {
                levels.push(pieces);
            }
        }
        Ok(Self { levels })
    }

    pub fn uses_camera(&self) -> bool {
        self.levels.iter().flatten().any(|p| *p == Piece::Var(Var::Camera))
    }

    /// 拼出 `<root>/<各层>`。每层单独清洗，变量值里的 `/` 不会多分出一层。
    pub fn render(&self, root: &Path, v: &Vars) -> PathBuf {
        let mut dir = root.to_path_buf();
        for level in &self.levels {
            let mut s = String::new();
            for p in level {
                match p {
                    Piece::Text(t) => s.push_str(t),
                    Piece::Var(Var::Year) => s.push_str(&v.date.format("%Y").to_string()),
                    Piece::Var(Var::Month) => s.push_str(&v.date.format("%m").to_string()),
                    Piece::Var(Var::Day) => s.push_str(&v.date.format("%d").to_string()),
                    Piece::Var(Var::Date) => s.push_str(&v.date.format("%Y-%m-%d").to_string()),
                    Piece::Var(Var::Camera) => s.push_str(v.camera),
                    Piece::Var(Var::Card) => s.push_str(v.card),
                }
            }
            dir.push(sanitize(&s));
        }
        dir
    }
}

fn parse_level(level: &str) -> Result<Vec<Piece>, TemplateError> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut chars = level.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(TemplateError::Unclosed),
                    }
                }
                let name = name.trim().to_ascii_lowercase();
                let var = Var::parse(&name).ok_or(TemplateError::UnknownVariable(name))?;
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Var(var));
            }
            '}' => return Err(TemplateError::StrayClose),
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

/// 目录名清洗：去掉路径分隔符和控制字符，不允许以 `.` 开头（避免隐藏目录和 `..`）
pub(crate) fn sanitize(s: &str) -> String {
    let t: String = s.chars().map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c }).collect();
    let t = t.trim().trim_start_matches('.').trim().to_string();
    if t.is_empty() { "未命名".to_string() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(t: &str) -> String {
        let v = Vars { date: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(), camera: "Canon EOS R5", card: "EOS_DIGITAL" };
        let p = FolderTemplate::parse(t).unwrap().render(Path::new("/r"), &v);
        p.strip_prefix("/r").unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn renders_levels_and_mixed_text() {
        assert_eq!(render(DEFAULT_TEMPLATE), "2026/10/20");
        assert_eq!(render("{year}/{month}/{day}/{camera}"), "2026/10/20/Canon EOS R5");
        assert_eq!(render("{card}/{date} {camera}"), "EOS_DIGITAL/2026-10-20 Canon EOS R5");
        assert_eq!(render("/{ YEAR }//{year}{month}{day}/"), "2026/20261020");
        assert_eq!(render(""), "");
    }

    #[test]
    fn values_cannot_escape_their_level() {
        let v = Vars { date: NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), camera: "../a/b", card: "" };
        let p = FolderTemplate::parse("{camera}/{card}").unwrap().render(Path::new("/r"), &v);
        assert_eq!(p, Path::new("/r/-a-b/未命名"));
        assert_eq!(render(".."), "未命名");
    }

    #[test]
    fn reports_errors() {
        assert_eq!(FolderTemplate::parse("{year"), Err(TemplateError::Unclosed));
        assert_eq!(FolderTemplate::parse("year}"), Err(TemplateError::StrayClose));
        assert_eq!(FolderTemplate::parse("{lens}"), Err(TemplateError::UnknownVariable("lens".into())));
        assert!(FolderTemplate::parse("{camera}/{year}").unwrap().uses_camera());
        assert!(!FolderTemplate::default().uses_camera());
    }
}
