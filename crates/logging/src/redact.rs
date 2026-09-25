//! 日志输出脱敏器，在写入前替换日志文本中的敏感值。

use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

use regex::{Regex, RegexSet};
use tracing_subscriber::fmt::writer::MakeWriter;

/// 脱敏规则：正则 + 替换模板。
///
/// 模板保留键名并替换对应值。
const RULES: [(&str, &str); 4] = [
    (
        r"(?i)(cookie_token|ltoken|stoken|ltuid|ltmid_v2|account_id_v2)(_v2)?\s*[=:]\s*[^\s;,]+",
        "${1}${2}=***",
    ),
    (r"(?im)\b(cookie|set-cookie)\s*[:=]\s*.+$", "${1}=***"),
    (r"(?im)\b(authorization)\s*[:=]\s*.+$", "${1}=***"),
    (r"(?i)\b(password|passwd|pwd)\s*[:=]\s*\S+", "${1}=***"),
];

/// 预编译的规则集。
///
/// 通过 `RegexSet` 预检文本，再执行匹配的替换规则。
pub struct Redactor {
    matcher: RegexSet,
    /// `(替换模板, 正则)`；顺序即 `RULES` 的顺序。
    rules: Vec<(&'static str, Regex)>,
}

impl Redactor {
    fn new() -> Self {
        Self {
            matcher: RegexSet::new(RULES.iter().map(|(pattern, _)| *pattern))
                .expect("脱敏规则是仓库内的常量，必须是合法正则"),
            rules: RULES
                .iter()
                .map(|(pattern, template)| {
                    (
                        *template,
                        Regex::new(pattern).expect("脱敏规则是仓库内的常量，必须是合法正则"),
                    )
                })
                .collect(),
        }
    }

    /// 获取进程内共享的预编译规则集。
    fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<Redactor>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Redactor::new())))
    }

    /// 返回脱敏后的文本；**未命中任何规则时返回 `None`**，调用方据此原样透传。
    pub fn redact(&self, line: &str) -> Option<String> {
        let matched = self.matcher.matches(line);
        if !matched.matched_any() {
            return None;
        }
        let mut redacted = line.to_owned();
        for index in matched.iter() {
            let (template, rule) = &self.rules[index];
            redacted = rule.replace_all(&redacted, *template).into_owned();
        }
        Some(redacted)
    }
}

/// 逐行脱敏的 writer：未命中规则时按原字节透传，保证普通行逐字不变。
pub struct RedactingWriter<W> {
    inner: W,
    redactor: Arc<Redactor>,
}

impl<W> RedactingWriter<W> {
    pub fn new(inner: W, redactor: Arc<Redactor>) -> Self {
        Self { inner, redactor }
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.redactor.redact(&String::from_utf8_lossy(buf)) {
            None => self.inner.write(buf),
            Some(redacted) => {
                self.inner.write_all(redacted.as_bytes())?;
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// 把脱敏 writer 挂到 `fmt` 层上；两条通道（文件 / 控制台）都要经它。
pub struct RedactingMakeWriter<M> {
    inner: M,
    redactor: Arc<Redactor>,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M) -> Self {
        Self {
            inner,
            redactor: Redactor::shared(),
        }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.inner.make_writer(), Arc::clone(&self.redactor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_through(line: &str) -> String {
        let mut out = Vec::new();
        let mut writer = RedactingWriter::new(&mut out, Redactor::shared());
        writer.write_all(line.as_bytes()).unwrap();
        writer.flush().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn redacts_passport_cookie_pairs() {
        let line = "2026-09-20T10:11:12Z  WARN wonderland_account: 捕获凭据 cookie_token=abc123 ltoken=def456 ltuid=42;\n";
        let redacted = write_through(line);
        for secret in ["abc123", "def456"] {
            assert!(!redacted.contains(secret), "值必须被盖掉：{redacted}");
        }
        assert!(redacted.contains("cookie_token=***"));
        assert!(redacted.contains("ltoken=***"));
        assert!(redacted.contains("ltuid=***"));
        assert!(redacted.starts_with("2026-09-20T10:11:12Z  WARN wonderland_account:"));
    }

    #[test]
    fn redacts_headers_and_passwords() {
        let header = write_through("cookie: ltoken=def456; other=kept\n");
        assert!(!header.contains("def456"));
        assert!(header.contains("cookie=***"));

        let authorization = write_through("authorization: Bearer sk-abc123\n");
        assert!(!authorization.contains("sk-abc123"));
        assert!(authorization.contains("authorization=***"));

        let password = write_through("password=hunter2 pwd:qwerty\n");
        assert!(!password.contains("hunter2"));
        assert!(!password.contains("qwerty"));
        assert!(password.contains("password=***"));
        assert!(password.contains("pwd=***"));
    }

    #[test]
    fn every_rule_keeps_its_key_name() {
        for (pattern, template) in RULES {
            let rule = Regex::new(pattern).unwrap();
            assert!(rule.captures_len() > 1, "{pattern} 必须捕获键名");
            assert!(template.contains("${1}"), "{pattern} 的模板必须保留键名");
        }
    }

    #[test]
    fn plain_lines_are_untouched() {
        let line = "2026-09-20T10:11:12Z  INFO wonderland_net: 请求失败 status=500 retries=2\n";
        assert!(Redactor::shared().redact(line).is_none());
        assert_eq!(write_through(line), line);
        let text = "cookie count is fine, authorized=false\n";
        assert_eq!(write_through(text), text);
    }
}
