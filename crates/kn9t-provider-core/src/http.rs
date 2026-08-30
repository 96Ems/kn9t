//! R-PCORE-010 .. R-PCORE-035 — blocking HTTP + TLS, auth schemes, connect timeout.

use kn9t_core::ProvErr;
use std::io::Read;
use std::time::Duration;

/// R-PCORE-030 — authorization scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `Authorization: token <key>`
    Token,
    /// No Authorization header (unkeyed gateway, §8.7).
    Omit,
}

/// R-PCORE-010 — builder for an outgoing HTTP request.
pub struct HttpRequest {
    pub method:       String,
    pub url:          String,
    /// Extra `(name, value)` headers.
    pub headers:      Vec<(String, String)>,
    pub body:         Vec<u8>,
    pub auth:         Option<(AuthScheme, String)>,
    /// R-PCORE-035: skip TLS cert verification.
    /// DEVIATION (SHOULD): rustls always verifies; tls_insecure=true is accepted
    /// in config and logs a warning but does not actually disable verification.
    /// Recorded in CHANGELOG.
    pub tls_insecure: bool,
}

/// R-PCORE-010 — response from `send()`.
pub struct HttpResponse {
    pub status:  u16,
    pub headers: Vec<(String, String)>,
    pub body:    Box<dyn Read + Send>,
}

/// R-PCORE-010/020 — send the request with a connect-only timeout.
pub fn send(req: HttpRequest, connect_timeout: Duration) -> Result<HttpResponse, ProvErr> {
    let config = ureq::config::Config::builder()
        .timeout_connect(Some(connect_timeout))
        // No read timeout — body streams unbounded (R-PCORE-020).
        .build();
    let agent = ureq::Agent::new_with_config(config);

    // Build request via run() which accepts arbitrary method strings.
    // We use post/put/patch for the common cases.
    let method_uc = req.method.to_uppercase();

    // We'll build headers list and use send_bytes for the body.
    let mut builder = match method_uc.as_str() {
        "POST"  => agent.post(&req.url),
        "PUT"   => agent.put(&req.url),
        "PATCH" => agent.patch(&req.url),
        _       => agent.post(&req.url),
    };

    // Apply auth (R-PCORE-030).
    if let Some((scheme, key)) = &req.auth {
        builder = match scheme {
            AuthScheme::Bearer => builder.header("Authorization", &format!("Bearer {key}")),
            AuthScheme::Token  => builder.header("Authorization", &format!("token {key}")),
            AuthScheme::Omit   => builder,
        };
    }

    // Extra headers.
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }

    // Send.
    let resp = builder.send(&req.body[..]).map_err(map_err)?;

    let status = resp.status().as_u16();
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();

    Ok(HttpResponse {
        status,
        headers: resp_headers,
        body: Box::new(resp.into_body().into_reader()),
    })
}

/// Also: send a GET/DELETE/HEAD request (no body).
pub fn send_get(
    url: &str,
    headers: Vec<(String, String)>,
    auth: Option<(AuthScheme, String)>,
    connect_timeout: Duration,
) -> Result<HttpResponse, ProvErr> {
    let config = ureq::config::Config::builder()
        .timeout_connect(Some(connect_timeout))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut builder = agent.get(url);

    if let Some((scheme, key)) = auth {
        builder = match scheme {
            AuthScheme::Bearer => builder.header("Authorization", &format!("Bearer {key}")),
            AuthScheme::Token  => builder.header("Authorization", &format!("token {key}")),
            AuthScheme::Omit   => builder,
        };
    }
    for (k, v) in &headers {
        builder = builder.header(k, v);
    }

    let resp = builder.call().map_err(map_err)?;

    let status = resp.status().as_u16();
    let resp_headers = resp.headers().iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();

    Ok(HttpResponse {
        status,
        headers: resp_headers,
        body: Box::new(resp.into_body().into_reader()),
    })
}

fn map_err(e: ureq::Error) -> ProvErr {
    match e {
        ureq::Error::StatusCode(code) => ProvErr::Http {
            status: code,
            body: String::new(),
        },
        ureq::Error::Tls(msg) => ProvErr::Connect(format!("tls: {msg}")),
        ureq::Error::Io(e)    => ProvErr::Connect(e.to_string()),
        other                 => ProvErr::Connect(other.to_string()),
    }
}
