//! 平台接口客户端：统一的 cookie 组装与调用入口。

use serde::de::DeserializeOwned;
use wonderland_kernel::{KernelError, StoredCookie};
use wonderland_net::HttpClient;

use crate::creator::{BasicInfo, CreatorProfile, Envelope, LevelDetail, non_zero};
use crate::endpoint::{
    API_MICREATOR, API_TAKUMI, GAME_BIZ_HK4E_CN, MICREATOR_QUERY_BASE, PATH_CREATOR_BASIC_INFO,
    PATH_CREATOR_LEVEL_DETAIL, PATH_GAME_ROLES,
};
use crate::role::{GameRole, RoleListResponse};

/// 米哈游平台接口客户端。
///
/// 只负责"怎么请求"，不持有凭据：调用方每次传入 cookie 快照。
pub struct MihoyoClient {
    http: HttpClient,
}

impl MihoyoClient {
    pub fn new() -> Result<Self, KernelError> {
        Ok(Self {
            http: HttpClient::new()?,
        })
    }

    /// 拉取该账号绑定的原神角色。
    pub async fn game_roles(&self, cookies: &[StoredCookie]) -> Result<Vec<GameRole>, KernelError> {
        if cookies.is_empty() {
            return Err(KernelError::NotLoggedIn);
        }

        let cookie = cookie_header_for(cookies, "api-takumi.mihoyo.com", PATH_GAME_ROLES);
        let url = format!("{API_TAKUMI}{PATH_GAME_ROLES}?game_biz={GAME_BIZ_HK4E_CN}");

        let response: RoleListResponse = self
            .http
            .get_json(
                &url,
                &[("cookie", cookie.as_str()), ("accept", ACCEPT_JSON)],
            )
            .await?;

        if response.retcode != 0 {
            return Err(KernelError::Transport(format!(
                "接口返回 {}：{}",
                response.retcode,
                response.message.unwrap_or_default()
            )));
        }

        Ok(response
            .data
            .map(|data| data.list)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.game_biz == GAME_BIZ_HK4E_CN)
            .map(GameRole::from)
            .collect())
    }

    /// 拉取创作者中心资料：游戏内昵称 / 头像与奇匠等级。
    ///
    /// 头像只能从这里拿到——原神战绩接口的 `AvatarUrl` 恒空，登录页图标又依赖 DOM 渲染时机，
    /// 因此账号页的头像以本接口为准。两次 GET 都是只读，失败由调用方决定是否降级。
    pub async fn creator_profile(
        &self,
        cookies: &[StoredCookie],
        uid: &str,
        region: &str,
    ) -> Result<CreatorProfile, KernelError> {
        if cookies.is_empty() {
            return Err(KernelError::NotLoggedIn);
        }

        let basic: BasicInfo = self
            .creator_get(
                cookies,
                PATH_CREATOR_BASIC_INFO,
                uid,
                region,
                &[("need_ra_game_info", "true")],
            )
            .await?;
        let level: LevelDetail = self
            .creator_get(cookies, PATH_CREATOR_LEVEL_DETAIL, uid, region, &[])
            .await?;

        let game = basic.ra_game_info.unwrap_or_default();
        let avatar_url = (!game.avatar_url.is_empty()).then_some(game.avatar_url);
        Ok(CreatorProfile {
            avatar_url,
            level: non_zero(level.cur_level).or_else(|| non_zero(basic.kolugc_level)),
            exp: non_zero(level.cur_exp),
            exp_total: non_zero(level.total_exp),
        })
    }

    /// 创作者中心的只读 GET。查询参数值只来自官方响应，无需再做转义。
    async fn creator_get<T: DeserializeOwned>(
        &self,
        cookies: &[StoredCookie],
        path: &str,
        uid: &str,
        region: &str,
        extra: &[(&str, &str)],
    ) -> Result<T, KernelError> {
        let cookie = cookie_header_for(cookies, "api-micreator.mihoyo.com", path);
        if cookie.is_empty() {
            return Err(KernelError::NotLoggedIn);
        }

        let mut url =
            format!("{API_MICREATOR}{path}?{MICREATOR_QUERY_BASE}&uid={uid}&region={region}");
        for (name, value) in extra {
            url.push('&');
            url.push_str(name);
            url.push('=');
            url.push_str(value);
        }

        let response: Envelope<T> = self
            .http
            .get_json(
                &url,
                &[
                    ("cookie", cookie.as_str()),
                    ("accept", ACCEPT_JSON),
                    ("origin", "https://act.mihoyo.com"),
                    ("referer", "https://act.mihoyo.com/"),
                ],
            )
            .await?;

        if response.retcode != 0 {
            return Err(KernelError::Business {
                code: response.retcode as i64,
                message: response.message.unwrap_or_default(),
            });
        }
        response.data.ok_or(KernelError::InvalidResponse)
    }
}

/// 米哈游接口普遍接受的 `Accept`。
pub(crate) const ACCEPT_JSON: &str = "application/json, text/plain, */*";

/// 把 cookie 快照拼成 `Cookie` 请求头。
///
/// 凭据只在这一处落成字符串，不向上层或日志暴露。
pub fn cookie_header_for(cookies: &[StoredCookie], host: &str, path: &str) -> String {
    let mut selected: Vec<_> = cookies
        .iter()
        .filter(|cookie| {
            let domain = cookie.domain.trim_start_matches('.');
            let domain_match = host == domain
                || ((cookie.domain.starts_with('.')
                    || domain == "mihoyo.com"
                    || domain == "miyoushe.com")
                    && host.ends_with(&format!(".{domain}")));
            let path_match = path == cookie.path
                || (path.starts_with(&cookie.path)
                    && (cookie.path.ends_with('/')
                        || path.as_bytes().get(cookie.path.len()) == Some(&b'/')));
            domain_match && path_match
        })
        .collect();
    selected.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
    selected
        .into_iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webview_normalized_parent_domain_and_path_boundaries() {
        let cookie = |name: &str, domain: &str, path: &str| StoredCookie {
            name: name.into(),
            value: "test-only".into(),
            domain: domain.into(),
            path: path.into(),
        };
        let cookies = vec![
            cookie("token", "mihoyo.com", "/"),
            cookie("other", "passport-api.mihoyo.com", "/"),
            cookie("wrong", "mihoyo.com", "/data"),
            cookie("specific", ".mihoyo.com", "/kolugc_hch"),
        ];
        assert_eq!(
            cookie_header_for(
                &cookies,
                "api-micreator.mihoyo.com",
                "/kolugc_hch/common/v1/data/get_user_info"
            ),
            "specific=test-only; token=test-only"
        );
        assert!(cookie_header_for(&cookies, "mihoyo.com.attacker.test", "/").is_empty());
        assert!(
            !cookie_header_for(&cookies, "api-micreator.mihoyo.com", "/database").contains("wrong")
        );
    }
}
