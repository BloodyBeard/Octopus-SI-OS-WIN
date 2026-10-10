use std::env;
use std::process;
use std::time::Duration;

const PROBE_TLS13_ENV: &str = "CODEX_CUSTOM_CA_PROBE_TLS13";
const PROBE_PROXY_ENV: &str = "CODEX_CUSTOM_CA_PROBE_PROXY";
const PROBE_URL_ENV: &str = "CODEX_CUSTOM_CA_PROBE_URL";

fn main() {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to create probe runtime: {error}");
            process::exit(1);
        }
    };

    match runtime.block_on(run_probe()) {
        Ok(()) => println!("ok"),
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

async fn run_probe() -> Result<(), String> {
    let proxy_url = env::var(PROBE_PROXY_ENV).ok();
    let target_url = env::var(PROBE_URL_ENV).ok();
    let mut builder = reqwest::Client::builder();
    if target_url.is_some() {
        builder = builder.timeout(Duration::from_secs(5));
    }
    if env::var_os(PROBE_TLS13_ENV).is_some() {
        builder = builder.min_tls_version(reqwest::tls::Version::TLS_1_3);
    }

    let client = build_probe_client(builder, proxy_url.as_deref())?;
    if let Some(url) = target_url {
        post_probe_request(&client, &url).await?;
    }
    Ok(())
}

fn build_probe_client(
    builder: reqwest::ClientBuilder,
    proxy_url: Option<&str>,
) -> Result<reqwest::Client, String> {
    if let Some(proxy_url) = proxy_url {
        let proxy = reqwest::Proxy::https(proxy_url)
            .map_err(|error| format!("failed to configure probe proxy {proxy_url}: {error}"))?;
        return octopus_client::build_reqwest_client_with_custom_ca(builder.proxy(proxy))
            .map_err(|error| error.to_string());
    }

    octopus_client::build_reqwest_client_for_subprocess_tests(builder)
        .map_err(|error| error.to_string())
}

async fn post_probe_request(client: &reqwest::Client, url: &str) -> Result<(), String> {
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("grant_type=authorization_code&code=test")
        .send()
        .await
        .map_err(|error| format!("probe request failed: {error:?}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to read probe response body: {error}"))?;
    if !status.is_success() {
        return Err(format!("probe request returned {status}: {body}"));
    }
    if body != "ok" {
        return Err(format!("probe response body mismatch: {body}"));
    }
    Ok(())
}
