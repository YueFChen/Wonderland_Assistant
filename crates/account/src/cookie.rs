//! 凭据判定：从宿主读到的 cookie 里挑出米哈游域的部分，并在凭据齐全时给出账号主键。

use wonderland_kernel::StoredCookie;

/// 凭据齐全后提取出的账号标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// 米哈游通行证账号 ID。
    pub account_key: String,
    /// 米游社 mid；仅 V2 cookie 提供。
    pub mid: Option<String>,
}

/// cookie 是否属于米哈游域（含子域）。
pub fn is_mihoyo_domain(domain: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == "mihoyo.com"
        || domain.ends_with(".mihoyo.com")
        || domain == "miyoushe.com"
        || domain.ends_with(".miyoushe.com")
}

/// 只保留米哈游域的 cookie：登录窗口会访问多个域，业务只需要其中一部分。
pub fn select_mihoyo_cookies(cookies: &[StoredCookie]) -> Vec<StoredCookie> {
    cookies
        .iter()
        .filter(|cookie| is_mihoyo_domain(&cookie.domain))
        .cloned()
        .collect()
}

/// 账号标识键：`account_id` / `account_id_v2` / `ltuid` / `ltuid_v2`。
fn is_account_marker(name: &str) -> bool {
    name.starts_with("account_id") || name.starts_with("ltuid")
}

/// 令牌键：`cookie_token` / `ltoken` / `stoken` 及其 V2 变体。
fn is_token(name: &str) -> bool {
    name.starts_with("cookie_token") || name.starts_with("ltoken") || name == "stoken"
}

/// 凭据是否齐全；齐全时返回账号标识。
///
/// 同时存在账号标识和令牌时返回账号标识。键名按前缀匹配以支持 V1 和 V2 cookie。
pub fn evaluate(cookies: &[StoredCookie]) -> Option<Credentials> {
    let account_key = cookies
        .iter()
        .find(|cookie| is_account_marker(&cookie.name))?
        .value
        .clone();

    if account_key.is_empty() || !cookies.iter().any(|cookie| is_token(&cookie.name)) {
        return None;
    }

    let mid = cookies
        .iter()
        .find(|cookie| cookie.name == "account_mid_v2" || cookie.name == "mid")
        .map(|cookie| cookie.value.clone())
        .filter(|value| !value.is_empty());

    Some(Credentials { account_key, mid })
}
