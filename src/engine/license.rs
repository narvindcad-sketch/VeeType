#![cfg_attr(feature = "test-bypass", allow(dead_code))]
use anyhow::{bail, Context};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::engine::KeyVault;

const LICENSE_ACCOUNT: &str = "VEETYPE_LICENSE_JWT";
const SESSION_ACCOUNT: &str = "VEETYPE_SUPABASE_SESSION";
const LICENSE_ISSUER: &str = "veetype-license";
const LICENSE_AUDIENCE: &str = "veetype-desktop";
const PRO_FEATURES: [&str; 3] = ["cloud_providers", "hands_free", "large_models"];

#[derive(Clone, Copy, Debug, Default)]
pub struct Entitlements {
    pub cloud_providers: bool,
    pub hands_free: bool,
    pub large_models: bool,
}

impl Entitlements {
    pub fn unlocked() -> Self {
        Self::all(true)
    }

    pub fn fallback() -> Self {
        Self::all(cfg!(feature = "test-bypass"))
    }

    fn all(enabled: bool) -> Self {
        Self {
            cloud_providers: enabled,
            hands_free: enabled,
            large_models: enabled,
        }
    }

    pub fn is_pro(self) -> bool {
        self.cloud_providers && self.hands_free && self.large_models
    }
}

pub fn effective_provider<'a>(
    configured_provider: &'a str,
    entitlements: &Entitlements,
) -> &'a str {
    if matches!(
        configured_provider.to_ascii_lowercase().as_str(),
        "groq" | "openai" | "anthropic"
    ) && !entitlements.cloud_providers
    {
        "local"
    } else {
        configured_provider
    }
}

pub fn hands_free_enabled(configured: bool, entitlements: &Entitlements) -> bool {
    configured && entitlements.hands_free
}

#[derive(Deserialize, Serialize)]
struct AuthSession {
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct LicenseClaims {
    iss: String,
    aud: String,
    sub: String,
    iat: i64,
    exp: i64,
    entitlements: Vec<String>,
}

pub struct LicenseManager;

impl LicenseManager {
    pub fn cached_entitlements() -> anyhow::Result<Option<Entitlements>> {
        if cfg!(feature = "test-bypass") {
            return Ok(Some(Entitlements::unlocked()));
        }
        let Some(token) = KeyVault::get_key(LICENSE_ACCOUNT)? else {
            return Ok(None);
        };
        let Some(claims) = verify_license(&token)? else {
            return Ok(None);
        };
        Ok(Some(entitlements_from_claims(&claims)))
    }

    pub fn account_status() -> anyhow::Result<String> {
        match Self::cached_entitlements()? {
            Some(entitlements) if entitlements.is_pro() => {
                Ok("VeeType Pro is active on this device.".to_string())
            }
            Some(_) => bail!("The saved license does not grant all VeeType Pro features"),
            None => Ok("No active Pro license is stored on this device.".to_string()),
        }
    }

    pub fn has_session() -> anyhow::Result<bool> {
        Ok(load_session()?.is_some())
    }

    pub fn sign_in(email: &str, password: &str, create_account: bool) -> anyhow::Result<String> {
        let config = SupabaseConfig::from_environment()?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Could not initialize the Supabase client")?;
        let endpoint = if create_account {
            format!("{}/auth/v1/signup", config.url)
        } else {
            format!("{}/auth/v1/token?grant_type=password", config.url)
        };
        let response = client
            .post(endpoint)
            .header("apikey", &config.anon_key)
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .context("Could not connect to Supabase Auth")?;
        let auth: Value = read_json_response(response, "Supabase Auth")?;

        let Some(access_token) = auth["access_token"].as_str() else {
            if create_account {
                KeyVault::delete_key(SESSION_ACCOUNT)?;
                KeyVault::delete_key(LICENSE_ACCOUNT)?;
                return Ok("Check your email to confirm the account, then sign in.".to_string());
            }
            bail!("Supabase Auth returned no access token");
        };
        let refresh_token = auth["refresh_token"]
            .as_str()
            .context("Supabase Auth returned no refresh token")?;
        let session = AuthSession {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
        };
        KeyVault::delete_key(LICENSE_ACCOUNT)?;
        save_session(&session)?;
        Self::issue_and_store_license(&client, &config, &session.access_token)
    }

    pub fn refresh_license() -> anyhow::Result<String> {
        let config = SupabaseConfig::from_environment()?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Could not initialize the Supabase client")?;
        let refreshed = Self::refresh_session(&client, &config)?;
        Self::issue_and_store_license(&client, &config, &refreshed.access_token)
    }

    pub fn open_checkout() -> anyhow::Result<String> {
        let config = SupabaseConfig::from_environment()?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Could not initialize the Supabase client")?;
        let session = Self::refresh_session(&client, &config)?;
        let response = client
            .post(format!("{}/functions/v1/create-checkout", config.url))
            .header("apikey", &config.anon_key)
            .bearer_auth(&session.access_token)
            .send()
            .context("Could not create a Lemon Squeezy checkout")?;
        let body = read_json_response(response, "Checkout service")?;
        let checkout_url = body["checkout_url"]
            .as_str()
            .context("Checkout service returned no checkout URL")?;
        if !checkout_url.starts_with("https://") {
            bail!("Checkout service returned a non-HTTPS URL");
        }

        let operation = Self::wide("open");
        let target = Self::wide(checkout_url);
        let result = unsafe {
            ShellExecuteW(
                0,
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize <= 32 {
            bail!("Windows could not open the checkout page (ShellExecuteW returned {result})");
        }
        Ok("Checkout opened in your browser. After subscribing, refresh your license.".into())
    }

    pub fn sign_out() -> anyhow::Result<()> {
        let session_result = KeyVault::delete_key(SESSION_ACCOUNT);
        let license_result = KeyVault::delete_key(LICENSE_ACCOUNT);
        session_result?;
        license_result
    }

    fn refresh_session(client: &Client, config: &SupabaseConfig) -> anyhow::Result<AuthSession> {
        let session = load_session()?.context("Sign in to Supabase before continuing")?;
        let response = client
            .post(format!(
                "{}/auth/v1/token?grant_type=refresh_token",
                config.url
            ))
            .header("apikey", &config.anon_key)
            .json(&serde_json::json!({"refresh_token": session.refresh_token}))
            .send()
            .context("Could not refresh the Supabase session")?;
        let auth: Value = read_json_response(response, "Supabase Auth")?;
        let refreshed = AuthSession {
            access_token: auth["access_token"]
                .as_str()
                .context("Supabase returned no refreshed access token")?
                .to_string(),
            refresh_token: auth["refresh_token"]
                .as_str()
                .unwrap_or(&session.refresh_token)
                .to_string(),
        };
        save_session(&refreshed)?;
        Ok(refreshed)
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn issue_and_store_license(
        client: &Client,
        config: &SupabaseConfig,
        access_token: &str,
    ) -> anyhow::Result<String> {
        let response = client
            .post(format!("{}/functions/v1/issue-license", config.url))
            .header("apikey", &config.anon_key)
            .bearer_auth(access_token)
            .send()
            .context("Could not request a license from VeeType")?;
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            KeyVault::delete_key(LICENSE_ACCOUNT)?;
        }
        let body: Value = read_json_response(response, "License service")?;
        let token = body["license"]
            .as_str()
            .context("License service returned no signed license")?;
        let claims =
            verify_license(token)?.context("The license service returned an expired license")?;
        KeyVault::save_key(LICENSE_ACCOUNT, token)?;
        Ok(format!(
            "VeeType Pro is active until {} (UTC). Restart VeeType to apply.",
            chrono::DateTime::<chrono::Utc>::from_timestamp(claims.exp, 0)
                .context("License has an invalid expiration time")?
                .format("%Y-%m-%d %H:%M")
        ))
    }
}

struct SupabaseConfig {
    url: String,
    anon_key: String,
}

impl SupabaseConfig {
    fn from_environment() -> anyhow::Result<Self> {
        let url = option_env!("VEETYPE_SUPABASE_URL")
            .filter(|value| !value.trim().is_empty())
            .context("VeeType was built without VEETYPE_SUPABASE_URL")?
            .trim_end_matches('/')
            .to_string();
        if !url.starts_with("https://") {
            bail!("VEETYPE_SUPABASE_URL must use HTTPS");
        }
        let anon_key = option_env!("VEETYPE_SUPABASE_ANON_KEY")
            .filter(|value| !value.trim().is_empty())
            .context("VeeType was built without VEETYPE_SUPABASE_ANON_KEY")?
            .to_string();
        Ok(Self { url, anon_key })
    }
}

fn save_session(session: &AuthSession) -> anyhow::Result<()> {
    KeyVault::save_key(
        SESSION_ACCOUNT,
        &serde_json::to_string(session).context("Serializing Supabase session")?,
    )
}

fn load_session() -> anyhow::Result<Option<AuthSession>> {
    KeyVault::get_key(SESSION_ACCOUNT)?
        .map(|session| serde_json::from_str(&session).context("Parsing saved Supabase session"))
        .transpose()
}

fn read_json_response(
    response: reqwest::blocking::Response,
    service: &str,
) -> anyhow::Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("Reading {service} response"))?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| {
                value["msg"]
                    .as_str()
                    .or_else(|| value["message"].as_str())
                    .or_else(|| value["error_description"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or(body);
        bail!("{service} returned HTTP {status}: {detail}");
    }
    serde_json::from_str(&body).with_context(|| format!("{service} returned invalid JSON"))
}

fn verify_license(token: &str) -> anyhow::Result<Option<LicenseClaims>> {
    let public_key = option_env!("VEETYPE_LICENSE_PUBLIC_KEY_B64")
        .filter(|value| !value.trim().is_empty())
        .context("VeeType was built without VEETYPE_LICENSE_PUBLIC_KEY_B64")?;
    let public_key = STANDARD
        .decode(public_key)
        .context("License public key is not valid base64")?;
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("License public key must be 32 bytes"))?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("License public key is invalid")?;

    verify_license_with_key(token, &verifying_key, chrono::Utc::now().timestamp())
}

fn verify_license_with_key(
    token: &str,
    verifying_key: &VerifyingKey,
    now: i64,
) -> anyhow::Result<Option<LicenseClaims>> {
    let mut segments = token.split('.');
    let header_segment = segments.next().context("License token has no header")?;
    let payload = segments.next().context("License token has no claims")?;
    let signature_segment = segments.next().context("License token has no signature")?;
    if segments.next().is_some() {
        bail!("License token has an invalid number of segments");
    }
    let header: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(header_segment)
            .context("License token header is invalid base64url")?,
    )
    .context("License token header is invalid JSON")?;
    if header["alg"].as_str() != Some("EdDSA") || header["typ"].as_str() != Some("JWT") {
        bail!("License token must use the EdDSA JWT algorithm");
    }
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_segment)
        .context("License signature is invalid base64url")?;
    let signature =
        Signature::from_slice(&signature_bytes).context("License signature has an invalid size")?;
    verifying_key
        .verify(format!("{header_segment}.{payload}").as_bytes(), &signature)
        .context("License signature verification failed")?;

    let claims: LicenseClaims = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload)
            .context("License claims are invalid base64url")?,
    )
    .context("License claims are invalid JSON")?;
    if claims.iss != LICENSE_ISSUER || claims.aud != LICENSE_AUDIENCE || claims.sub.is_empty() {
        bail!("License issuer, audience, or subject is invalid");
    }
    if claims.iat > now + 300 {
        bail!("License was issued in the future");
    }
    if claims.exp <= claims.iat {
        bail!("License expiration must be after issuance");
    }
    if claims.exp <= now {
        return Ok(None);
    }
    if claims.exp - claims.iat > 7 * 24 * 60 * 60 + 300 {
        bail!("License validity exceeds the allowed seven-day period");
    }
    if !PRO_FEATURES
        .iter()
        .all(|feature| claims.entitlements.iter().any(|claim| claim == feature))
    {
        bail!("License does not grant the expected VeeType Pro entitlements");
    }
    Ok(Some(claims))
}

fn entitlements_from_claims(claims: &LicenseClaims) -> Entitlements {
    Entitlements {
        cloud_providers: claims
            .entitlements
            .iter()
            .any(|claim| claim == "cloud_providers"),
        hands_free: claims
            .entitlements
            .iter()
            .any(|claim| claim == "hands_free"),
        large_models: claims
            .entitlements
            .iter()
            .any(|claim| claim == "large_models"),
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_provider, hands_free_enabled, verify_license_with_key, Entitlements};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_token(signing_key: &SigningKey, exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#);
        let claims = URL_SAFE_NO_PAD.encode(format!(
            r#"{{"iss":"veetype-license","aud":"veetype-desktop","sub":"user-1","iat":1000,"exp":{exp},"entitlements":["cloud_providers","hands_free","large_models"]}}"#
        ));
        let message = format!("{header}.{claims}");
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
        format!("{message}.{signature}")
    }

    #[test]
    fn verifies_signed_unexpired_license_and_rejects_tampering() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let verifying_key = signing_key.verifying_key();
        let token = signed_token(&signing_key, 2000);

        assert!(verify_license_with_key(&token, &verifying_key, 1500)
            .expect("verify license")
            .is_some());
        let mut segments = token.split('.');
        let header = segments.next().expect("JWT header");
        let payload = segments.next().expect("JWT claims");
        let signature = segments.next().expect("JWT signature");
        let mut claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).expect("decode JWT claims"))
                .expect("parse JWT claims");
        claims["sub"] = serde_json::Value::String("user-2".into());
        let tampered = format!(
            "{header}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("encode modified claims"))
        );
        assert!(verify_license_with_key(&tampered, &verifying_key, 1500).is_err());
    }

    #[test]
    fn expired_license_does_not_grant_entitlements() {
        let signing_key = SigningKey::from_bytes(&[8; 32]);
        let token = signed_token(&signing_key, 1200);

        assert!(
            verify_license_with_key(&token, &signing_key.verifying_key(), 1200)
                .expect("verify expired license")
                .is_none()
        );
    }

    #[test]
    fn free_tier_keeps_local_dictation_but_disables_pro_features() {
        let free = Entitlements::default();

        assert_eq!(effective_provider("local", &free), "local");
        assert_eq!(effective_provider("openai", &free), "local");
        assert!(!hands_free_enabled(true, &free));
    }
}
