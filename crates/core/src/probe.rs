use super::*;
use std::time::{Duration, Instant};

/// Result of an explicit, user-triggered network check. This is deliberately a
/// machine-readable classification instead of an HTTP body: provider error
/// bodies can echo credentials or request fragments and must not cross IPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub endpoint: String,
    pub latency_ms: u64,
    pub code: String,
}

pub(super) fn probe(base_url: &str, api_key: &str) -> ProbeResult {
    let endpoint = models_endpoint(base_url);
    let started = Instant::now();
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(15))
        // Never forward Authorization to a redirect target. A provider that
        // answers /models with a redirect is reported instead of followed.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("ai-config-manager/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => return result(false, None, &endpoint, started, "client"),
    };
    let mut request = client
        .get(&endpoint)
        .header(reqwest::header::ACCEPT, "application/json");
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    match request.send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let code = classify(status);
            result(code == "ok", Some(status), &endpoint, started, code)
        }
        Err(error) => {
            let code = if error.is_timeout() {
                "timeout"
            } else {
                "network"
            };
            result(false, None, &endpoint, started, code)
        }
    }
}

fn models_endpoint(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

fn classify(status: u16) -> &'static str {
    if (200..300).contains(&status) {
        "ok"
    } else if status == 401 || status == 403 {
        "unauthorized"
    } else if status == 404 || status == 405 {
        "endpoint_unsupported"
    } else if (300..400).contains(&status) {
        "redirect"
    } else if (500..600).contains(&status) {
        "server_error"
    } else {
        "rejected"
    }
}

fn result(
    ok: bool,
    status: Option<u16>,
    endpoint: &str,
    started: Instant,
    code: &str,
) -> ProbeResult {
    ProbeResult {
        ok,
        status,
        endpoint: endpoint.into(),
        latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        code: code.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_derived_without_double_slashes() {
        assert_eq!(
            models_endpoint("https://api.example.com/v1"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            models_endpoint("https://api.example.com/v1/"),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn status_codes_are_classified_without_exposing_bodies() {
        assert_eq!(classify(200), "ok");
        assert_eq!(classify(204), "ok");
        assert_eq!(classify(401), "unauthorized");
        assert_eq!(classify(403), "unauthorized");
        assert_eq!(classify(404), "endpoint_unsupported");
        assert_eq!(classify(302), "redirect");
        assert_eq!(classify(500), "server_error");
        assert_eq!(classify(429), "rejected");
    }
}
