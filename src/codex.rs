use crate::flows::{FlowRegistry, FlowState};
use crate::jwt;
use crate::models::{Account, Outcome, now_ms};
use crate::provider::Backend;
use crate::store::Store;
use http::HeaderMap;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
pub const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const USER_AGENT: &str = concat!(
    "opencode/",
    env!("CARGO_PKG_VERSION"),
    " (underclass-pool-proxy)"
);

pub fn codex_endpoint() -> String {
    std::env::var("UNDERCLASS_CODEX_UPSTREAM").unwrap_or_else(|_| CODEX_ENDPOINT.to_string())
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeInit {
    #[serde(rename = "device_auth_id")]
    pub device_auth_id: String,
    pub user_code: String,
    pub interval: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeToken {
    pub authorization_code: String,
    pub code_verifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub id_token: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected status {0}")]
    Status(u16),
    #[error("authorization failed: {0}")]
    Failed(String),
}

async fn init_device_code(client: &Client) -> Result<DeviceCodeInit, AuthError> {
    let resp = client
        .post(format!("{ISSUER}/api/accounts/deviceauth/usercode"))
        .header("User-Agent", USER_AGENT)
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AuthError::Status(resp.status().as_u16()));
    }
    Ok(resp.json().await?)
}

async fn poll_device_token(
    client: &Client,
    init: &DeviceCodeInit,
) -> Result<DeviceCodeToken, AuthError> {
    let resp = client
        .post(format!("{ISSUER}/api/accounts/deviceauth/token"))
        .header("User-Agent", USER_AGENT)
        .json(&json!({
            "device_auth_id": init.device_auth_id,
            "user_code": init.user_code,
        }))
        .send()
        .await?;
    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 404 {
        return Err(AuthError::Failed("pending".into()));
    }
    if !status.is_success() {
        return Err(AuthError::Status(status.as_u16()));
    }
    Ok(resp.json().await?)
}

async fn exchange_code(
    client: &Client,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, AuthError> {
    let resp = client
        .post(format!("{ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &format!("{ISSUER}/deviceauth/callback")),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AuthError::Status(resp.status().as_u16()));
    }
    Ok(resp.json().await?)
}

pub fn identity_from_token(token: &str) -> Option<String> {
    let claims = jwt::parse_jwt_claims(token)?;
    jwt::extract_email(&claims).or_else(|| jwt::extract_display_name(&claims))
}

/// @cc [owner:ghuntley,label:auth] codex-refresh-rotates
/// A successful Codex token refresh MUST return a fresh `refresh_token` along with the access
/// token; the previous refresh token MUST be considered invalidated by the caller.
pub async fn refresh(client: &Client, refresh_token: &str) -> Result<TokenResponse, AuthError> {
    let resp = client
        .post(format!("{ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AuthError::Status(resp.status().as_u16()));
    }
    Ok(resp.json().await?)
}

/// @cc [owner:ghuntley,label:identity] codex-onboarding-identity-label
/// A completed Codex device flow MUST create (or replace the tokens of) a `Healthy` account
/// persisted to the store, labeled with the email or display name from the exchanged token's JWT
/// claims, falling back to `chatgpt` when no identity is present.
pub async fn start_flow(
    client: Client,
    store: Arc<Store>,
    flows: FlowRegistry,
    replace_account: Option<String>,
) -> Result<String, AuthError> {
    let init = init_device_code(&client).await?;
    let interval_ms = init
        .interval
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1)
        * 1000;
    let flow_id = flows.create(
        crate::models::BackendId::Codex,
        FlowState::Pending {
            user_code: init.user_code.clone(),
            verify_url: format!("{ISSUER}/codex/device"),
        },
        replace_account,
    );

    let flows = flows.clone();
    let store = store.clone();
    let flow_id_out = flow_id.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms + 3000)).await;
            match poll_device_token(&client, &init).await {
                Err(AuthError::Failed(_)) => continue,
                Err(_) => {
                    flows.set_state(
                        &flow_id,
                        FlowState::Failed {
                            message: "device authorization failed".into(),
                        },
                    );
                    return;
                }
                Ok(code) => {
                    match exchange_code(&client, &code.authorization_code, &code.code_verifier)
                        .await
                    {
                        Ok(tokens) => {
                            let claims = jwt::parse_jwt_claims(&tokens.access_token);
                            let account_id_claim =
                                claims.as_ref().and_then(|c| jwt::extract_account_id(c));
                            let residency = claims
                                .as_ref()
                                .and_then(|c| jwt::extract_residency(c))
                                .or_else(|| {
                                    tokens
                                        .id_token
                                        .as_deref()
                                        .and_then(jwt::parse_jwt_claims)
                                        .as_ref()
                                        .and_then(|c| jwt::extract_residency(c))
                                });
                            let identity =
                                identity_from_token(&tokens.access_token).or_else(|| {
                                    tokens.id_token.as_deref().and_then(identity_from_token)
                                });
                            let ts = now_ms();
                            match flows.get(&flow_id).and_then(|f| f.replace_account) {
                                Some(existing_id) => {
                                    if let Some(mut account) = store.get_account(&existing_id) {
                                        account.refresh_token = Some(tokens.refresh_token);
                                        account.access_token = Some(tokens.access_token.clone());
                                        account.expires_at =
                                            ts + (tokens.expires_in.unwrap_or(3600) as i64) * 1000;
                                        account.account_id =
                                            account_id_claim.or(account.account_id);
                                        account.residency = residency.or(account.residency);
                                        account.status = crate::models::AccountStatus::Healthy;
                                        account.reset_at = 0;
                                        account.token_refreshed_at = ts;
                                        account.updated_at = ts;
                                        store.upsert_account(&account);
                                        flows.set_state(
                                            &flow_id,
                                            FlowState::Authorized {
                                                account_id: account.id,
                                            },
                                        );
                                    } else {
                                        flows.set_state(
                                            &flow_id,
                                            FlowState::Failed {
                                                message: "account being replaced no longer exists"
                                                    .into(),
                                            },
                                        );
                                    }
                                }
                                None => {
                                    let mut account = crate::flows::new_account(
                                        crate::models::BackendId::Codex,
                                        identity.unwrap_or_else(|| "chatgpt".into()),
                                    );
                                    account.refresh_token = Some(tokens.refresh_token);
                                    account.access_token = Some(tokens.access_token.clone());
                                    account.expires_at =
                                        ts + (tokens.expires_in.unwrap_or(3600) as i64) * 1000;
                                    account.account_id = account_id_claim;
                                    account.residency = residency;
                                    account.token_refreshed_at = ts;
                                    store.upsert_account(&account);
                                    flows.set_state(
                                        &flow_id,
                                        FlowState::Authorized {
                                            account_id: account.id.clone(),
                                        },
                                    );
                                }
                            }
                            return;
                        }
                        Err(e) => {
                            flows.set_state(
                                &flow_id,
                                FlowState::Failed {
                                    message: e.to_string(),
                                },
                            );
                            return;
                        }
                    }
                }
            }
        }
    });
    Ok(flow_id_out)
}

pub struct CodexBackend {
    pub cooldown_ms: i64,
}

/// How long an account is benched after Codex refuses a model for its ChatGPT plan.
const UNSUPPORTED_MODEL_COOLDOWN_MS: i64 = 60 * 60 * 1000;

/// True for Codex's `{"detail":"The '<model>' model is not supported when using Codex with a
/// ChatGPT account."}` refusal.
fn is_unsupported_model_refusal(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|detail| {
            detail.contains("model is not supported when using Codex with a ChatGPT account")
        })
}

impl Backend for CodexBackend {
    fn usage_is_chat(&self, _path: &str) -> bool {
        false
    }
    fn id(&self) -> crate::models::BackendId {
        crate::models::BackendId::Codex
    }

    fn surfaces(&self) -> &'static [crate::models::Surface] {
        &[crate::models::Surface::OpenAI]
    }

    fn default_cooldown_ms(&self) -> i64 {
        self.cooldown_ms
    }

    /// @cc [owner:ghuntley,label:pool] codex-quota-deadline
    /// A Codex usage-limit error MUST cool the account until the later valid future deadline from
    /// `error.resets_at` (epoch seconds) and Retry-After. Without either, it MUST use the configured
    /// cooldown. Overload errors MUST remain transient. Legacy plain-text quota indicators MUST
    /// remain recognized by this backend. A "model is not supported when using Codex with a
    /// ChatGPT account" refusal MUST cool the account for one hour.
    fn classify(&self, status: u16, body: &str, headers: &HeaderMap, now_ms: i64) -> Outcome {
        if is_unsupported_model_refusal(body) {
            return Outcome::QuotaExhausted {
                until_ms: now_ms.saturating_add(UNSUPPORTED_MODEL_COOLDOWN_MS),
            };
        }
        let error = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("error").cloned());
        let error_type = error
            .as_ref()
            .and_then(|e| e.get("type"))
            .and_then(Value::as_str);
        let error_code = error
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str);
        if error_type == Some("server_is_overloaded") || error_code == Some("server_is_overloaded")
        {
            return Outcome::Transient;
        }
        let legacy_quota = [
            "usage_limit_reached",
            "insufficient_quota",
            "usage_not_included",
            "FreeUsageLimitError",
            "rate_limit",
            "too_many_requests",
        ];
        let quota_body = error_type
            .into_iter()
            .chain(error_code)
            .any(|v| legacy_quota.iter().any(|q| v.eq_ignore_ascii_case(q)))
            || (error.is_none() && {
                let lower = body.to_ascii_lowercase();
                legacy_quota
                    .iter()
                    .any(|q| lower.contains(&q.to_ascii_lowercase()))
            });
        if status == 429 || quota_body {
            let body_deadline = if error_type == Some("usage_limit_reached")
                || error_code == Some("usage_limit_reached")
            {
                error
                    .as_ref()
                    .and_then(|e| e.get("resets_at"))
                    .and_then(Value::as_i64)
                    .and_then(|seconds| seconds.checked_mul(1000))
                    .filter(|deadline| *deadline > now_ms)
            } else {
                None
            };
            let header_deadline = crate::health::parse_retry_after_headers(headers, now_ms)
                .filter(|deadline| *deadline > now_ms);
            return Outcome::QuotaExhausted {
                until_ms: body_deadline
                    .into_iter()
                    .chain(header_deadline)
                    .max()
                    .unwrap_or(now_ms.saturating_add(self.cooldown_ms)),
            };
        }
        crate::health::classify(status, headers, self.cooldown_ms, now_ms)
    }

    /// @cc [owner:ghuntley,label:proxy] codex-store-false
    /// Every outbound Codex request body MUST have `store` set to `false`; the upstream endpoint
    /// rejects requests otherwise.
    fn prepare_body(&self, body: &mut Value) {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("store".to_string(), Value::Bool(false));
        }
    }

    fn rewrite_url(&self, path: &str, _account: &Account) -> String {
        if path.ends_with("/responses") || path.ends_with("/chat/completions") {
            codex_endpoint()
        } else {
            format!("https://api.openai.com{path}")
        }
    }

    /// @cc [owner:edwrdc,label:auth] codex-header-injection
    /// Outbound Codex requests MUST carry `Authorization: Bearer <account access token>`, and,
    /// when known on the account, the `ChatGPT-Account-Id` and
    /// `x-openai-internal-codex-residency` headers, plus `originator: opencode`. No
    /// client-supplied credential may survive into these headers.
    ///
    /// A non-empty sticky key valid as an HTTP header value MUST be sent as `session-id`.
    /// Missing, empty, or invalid sticky keys MUST NOT produce a `session-id` header.
    ///
    /// Only client `x-codex-beta-features` and `x-openai-internal-codex-responses-lite`
    /// headers MUST be copied verbatim into Codex requests. Missing headers MUST NOT be invented,
    /// and all other client headers MUST remain excluded from this injection.
    fn inject_headers(
        &self,
        account: &Account,
        token: &str,
        sticky: Option<&str>,
        _body: &Value,
        client: &HeaderMap,
        headers: &mut HeaderMap,
    ) {
        for name in [
            "x-codex-beta-features",
            "x-openai-internal-codex-responses-lite",
        ] {
            if let Some(value) = client.get(name) {
                headers.insert(name, value.clone());
            }
        }
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}")).expect("bearer"),
        );
        if let Some(account_id) = &account.account_id {
            if let Ok(v) = http::HeaderValue::from_str(account_id) {
                headers.insert("ChatGPT-Account-Id", v);
            }
        }
        if let Some(residency) = &account.residency {
            if let Ok(v) = http::HeaderValue::from_str(residency) {
                headers.insert("x-openai-internal-codex-residency", v);
            }
        }
        headers.insert("originator", http::HeaderValue::from_static("opencode"));
        if let Some(sticky) = sticky.filter(|key| !key.is_empty()) {
            if let Ok(value) = http::HeaderValue::from_str(sticky) {
                headers.insert("session-id", value);
            }
        }
        if let Ok(v) = http::HeaderValue::from_str(USER_AGENT) {
            headers.insert(http::header::USER_AGENT, v);
        }
    }
}

/// @cc [owner:edwrdc,label:catalog] codex-default-model-limits
/// The default catalog MUST include GPT-6 Luna, GPT-6.1 Sol/Terra, and both Daybreak models.
/// GPT-6.1 entries MUST advertise 500,000 context/input tokens and 128,000 output tokens.
/// Existing older model IDs MUST remain available for clients that still select them.
pub fn default_catalog() -> Vec<crate::models::ModelInfo> {
    // Limits follow Codex Conversion 4.0.5's effective model catalog; older entries stay unchanged.
    // Use its contextWindow as the context/input ceiling, without adding the output allowance.
    let entries: &[(&str, &str, i64, i64, i64)] = &[
        ("gpt-5.4", "GPT-5.4", 400_000, 272_000, 128_000),
        ("gpt-5.4-mini", "GPT-5.4 Mini", 400_000, 272_000, 128_000),
        (
            "gpt-5.3-codex-spark",
            "GPT-5.3 Codex Spark",
            400_000,
            272_000,
            128_000,
        ),
        ("gpt-5.5", "GPT-5.5", 400_000, 272_000, 128_000),
        ("gpt-5.6-sol", "GPT-5.6 Sol", 272_000, 272_000, 128_000),
        ("gpt-5.6-terra", "GPT-5.6 Terra", 272_000, 272_000, 128_000),
        ("gpt-5.6-luna", "GPT-5.6 Luna", 272_000, 272_000, 128_000),
        ("gpt-6-astra", "GPT-6 Astra", 272_000, 272_000, 128_000),
        ("gpt-6-luna", "GPT-6 Luna", 272_000, 272_000, 128_000),
        ("gpt-6-sol", "GPT-6 Sol", 272_000, 272_000, 128_000),
        ("gpt-6.1-sol", "GPT-6.1 Sol", 500_000, 500_000, 128_000),
        ("gpt-6.1-terra", "GPT-6.1 Terra", 500_000, 500_000, 128_000),
        (
            "gpt-daybreak-blue-latest",
            "Daybreak Blue",
            272_000,
            272_000,
            128_000,
        ),
        (
            "gpt-daybreak-red-latest",
            "Daybreak Red",
            372_000,
            372_000,
            128_000,
        ),
    ];
    entries
        .iter()
        .map(
            |(id, name, context, input, output)| crate::models::ModelInfo {
                id: id.to_string(),
                name: name.to_string(),
                context: *context,
                input: *input,
                output: *output,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_body_deadline_outlasts_short_header() {
        let backend = CodexBackend {
            cooldown_ms: 30_000,
        };
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", http::HeaderValue::from_static("10"));
        let body = r#"{"error":{"type":"usage_limit_reached","resets_at":100}}"#;
        assert!(matches!(
            backend.classify(429, body, &headers, 1_000),
            Outcome::QuotaExhausted { until_ms: 100_000 }
        ));
        assert!(matches!(
            backend.classify(429, body, &headers, 200_000),
            Outcome::QuotaExhausted { until_ms: 210_000 }
        ));
        headers.insert("retry-after", http::HeaderValue::from_static("200"));
        assert!(matches!(
            backend.classify(429, body, &headers, 1_000),
            Outcome::QuotaExhausted { until_ms: 201_000 }
        ));
    }

    #[test]
    fn overload_is_transient_and_legacy_quota_remains_supported() {
        let backend = CodexBackend {
            cooldown_ms: 30_000,
        };
        let headers = HeaderMap::new();
        assert!(matches!(
            backend.classify(
                429,
                r#"{"error":{"type":"server_is_overloaded"}}"#,
                &headers,
                1_000
            ),
            Outcome::Transient
        ));
        assert!(matches!(
            backend.classify(
                400,
                r#"{"error":{"code":"usage_not_included"}}"#,
                &headers,
                1_000
            ),
            Outcome::QuotaExhausted { until_ms: 31_000 }
        ));
        assert!(matches!(
            backend.classify(400, "FreeUsageLimitError oops", &headers, 1_000),
            Outcome::QuotaExhausted { until_ms: 31_000 }
        ));
    }

    #[test]
    fn unsupported_model_refusal_cools_account_for_an_hour() {
        let backend = CodexBackend {
            cooldown_ms: 30_000,
        };
        let body = r#"{"detail":"The 'gpt-6-astra' model is not supported when using Codex with a ChatGPT account."}"#;
        assert!(matches!(
            backend.classify(400, body, &HeaderMap::new(), 1_000),
            Outcome::QuotaExhausted {
                until_ms: 3_601_000
            }
        ));
        assert!(matches!(
            backend.classify(400, r#"{"detail":"Bad request"}"#, &HeaderMap::new(), 1_000),
            Outcome::Ok
        ));
    }

    #[test]
    fn rewrites_completion_paths_to_codex_endpoint() {
        let backend = CodexBackend { cooldown_ms: 1000 };
        let account = crate::flows::new_account(crate::models::BackendId::Codex, "x".into());
        assert_eq!(
            backend.rewrite_url("/v1/responses", &account),
            CODEX_ENDPOINT
        );
        assert_eq!(
            backend.rewrite_url("/v1/chat/completions", &account),
            CODEX_ENDPOINT
        );
        assert_eq!(
            backend.rewrite_url("/v1/other", &account),
            "https://api.openai.com/v1/other"
        );
    }

    #[test]
    fn default_catalog_matches_codex_conversion_limits_and_retains_older_models() {
        let catalog = default_catalog();
        let expected = [
            ("gpt-5.4", 400_000, 272_000),
            ("gpt-5.4-mini", 400_000, 272_000),
            ("gpt-5.3-codex-spark", 400_000, 272_000),
            ("gpt-5.5", 400_000, 272_000),
            ("gpt-5.6-sol", 272_000, 272_000),
            ("gpt-5.6-terra", 272_000, 272_000),
            ("gpt-5.6-luna", 272_000, 272_000),
            ("gpt-6-astra", 272_000, 272_000),
            ("gpt-6-luna", 272_000, 272_000),
            ("gpt-6-sol", 272_000, 272_000),
            ("gpt-6.1-sol", 500_000, 500_000),
            ("gpt-6.1-terra", 500_000, 500_000),
            ("gpt-daybreak-blue-latest", 272_000, 272_000),
            ("gpt-daybreak-red-latest", 372_000, 372_000),
        ];
        assert_eq!(catalog.len(), expected.len());
        for (id, context, input) in expected {
            let model = catalog.iter().find(|model| model.id == id).expect(id);
            assert_eq!(
                (model.context, model.input, model.output),
                (context, input, 128_000),
                "{id}"
            );
        }
    }

    #[test]
    fn injects_codex_headers() {
        let backend = CodexBackend { cooldown_ms: 1000 };
        let mut account = crate::flows::new_account(crate::models::BackendId::Codex, "x".into());
        account.account_id = Some("acct-123".into());
        account.residency = Some("eu".into());
        let mut headers = HeaderMap::new();
        backend.inject_headers(
            &account,
            "tok",
            None,
            &Value::Null,
            &HeaderMap::new(),
            &mut headers,
        );
        assert_eq!(headers.get("authorization").unwrap(), "Bearer tok");
        assert_eq!(headers.get("ChatGPT-Account-Id").unwrap(), "acct-123");
        assert_eq!(
            headers.get("x-openai-internal-codex-residency").unwrap(),
            "eu"
        );
        assert_eq!(headers.get("originator").unwrap(), "opencode");
    }

    #[test]
    fn forwards_only_valid_sticky_keys_as_session_id() {
        let backend = CodexBackend { cooldown_ms: 1000 };
        let account = crate::flows::new_account(crate::models::BackendId::Codex, "x".into());
        for sticky in [None, Some(""), Some("bad\nvalue"), Some("cache-abc")] {
            let mut headers = HeaderMap::new();
            backend.inject_headers(
                &account,
                "tok",
                sticky,
                &Value::Null,
                &HeaderMap::new(),
                &mut headers,
            );
            assert_eq!(
                headers
                    .get("session-id")
                    .and_then(|value| value.to_str().ok()),
                sticky.filter(|key| *key == "cache-abc"),
            );
            assert_eq!(headers.get("authorization").unwrap(), "Bearer tok");
        }
    }
}
