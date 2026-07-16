use std::path::PathBuf;

use reqwest::{Method, RequestBuilder, Url};

const LOCAL_TRANSPORT_BASE_URL: &str = "http://localhost/";

/// The transport used to connect to the Clash external controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    NamedPipe(PathBuf),
    UnixSocket(PathBuf),
    Http(Url),
}

impl Host {
    pub fn named_pipe(path: impl Into<PathBuf>) -> Self {
        Self::NamedPipe(path.into())
    }

    pub fn unix_socket(path: impl Into<PathBuf>) -> Self {
        Self::UnixSocket(path.into())
    }

    pub fn http(base_url: impl AsRef<str>) -> Result<Self> {
        let value = base_url.as_ref();
        let url = Url::parse(value).map_err(|source| Error::InvalidBaseUrl {
            value: value.to_owned(),
            source,
        })?;

        Ok(Self::Http(url))
    }
}

/// A Clash API client that supports HTTP and platform-local transports.
#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    host: Host,
    base_url: Url,
}

impl Client {
    /// Create a client for the selected transport.
    pub fn new(host: Host) -> Result<Self> {
        match host {
            Host::NamedPipe(path) => Self::new_named_pipe(path),
            Host::UnixSocket(path) => Self::new_unix_socket(path),
            Host::Http(base_url) => Self::from_http_url(base_url),
        }
    }

    /// Create a client that sends requests through a Windows named pipe.
    pub fn new_named_pipe(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();

        #[cfg(windows)]
        {
            let client = reqwest::Client::builder()
                .windows_named_pipe(path.as_path())
                .no_proxy()
                .build()?;

            Ok(Self::with_local_transport(client, Host::NamedPipe(path)))
        }

        #[cfg(not(windows))]
        {
            let _ = path;
            Err(Error::UnsupportedTransport {
                transport: "Windows named pipe",
                platform: std::env::consts::OS,
            })
        }
    }

    /// Create a client that sends requests through a Unix domain socket.
    pub fn new_unix_socket(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();

        #[cfg(unix)]
        {
            let client = reqwest::Client::builder()
                .unix_socket(path.as_path())
                .no_proxy()
                .build()?;

            Ok(Self::with_local_transport(client, Host::UnixSocket(path)))
        }

        #[cfg(not(unix))]
        {
            let _ = path;
            Err(Error::UnsupportedTransport {
                transport: "Unix domain socket",
                platform: std::env::consts::OS,
            })
        }
    }

    /// Alias for [`Client::new_unix_socket`].
    #[cfg(unix)]
    pub fn unix_socket(path: impl Into<PathBuf>) -> Result<Self> {
        Self::new_unix_socket(path)
    }

    /// Create a client that sends requests to an HTTP(S) base URL.
    pub fn new_http(base_url: impl AsRef<str>) -> Result<Self> {
        Self::new(Host::http(base_url)?)
    }

    /// Return the transport configuration used by this client.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Return the normalized base URL used to build request URLs.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Return the underlying reqwest client for advanced use cases.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Resolve an API endpoint against the configured base URL.
    ///
    /// Leading slashes are treated as relative to the configured base path. An
    /// absolute endpoint is rejected so requests cannot accidentally bypass the
    /// configured controller.
    pub fn endpoint(&self, endpoint: &str) -> Result<Url> {
        if Url::parse(endpoint).is_ok() {
            return Err(Error::AbsoluteEndpoint {
                endpoint: endpoint.to_owned(),
            });
        }

        self.base_url
            .join(endpoint.trim_start_matches('/'))
            .map_err(|source| Error::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
                source,
            })
    }

    /// Start building a request to a Clash API endpoint.
    pub fn request(&self, method: Method, endpoint: &str) -> Result<RequestBuilder> {
        Ok(self.client.request(method, self.endpoint(endpoint)?))
    }

    pub fn get(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::GET, endpoint)
    }

    pub fn post(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::POST, endpoint)
    }

    pub fn put(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::PUT, endpoint)
    }

    pub fn patch(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::PATCH, endpoint)
    }

    pub fn delete(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::DELETE, endpoint)
    }

    fn from_http_url(base_url: Url) -> Result<Self> {
        let base_url = normalize_base_url(base_url)?;
        let client = reqwest::Client::builder().no_proxy().build()?;

        Ok(Self {
            client,
            host: Host::Http(base_url.clone()),
            base_url,
        })
    }

    fn with_local_transport(client: reqwest::Client, host: Host) -> Self {
        let base_url = Url::parse(LOCAL_TRANSPORT_BASE_URL)
            .expect("LOCAL_TRANSPORT_BASE_URL must be a valid URL");

        Self {
            client,
            host,
            base_url,
        }
    }
}

fn normalize_base_url(mut base_url: Url) -> Result<Url> {
    if !matches!(base_url.scheme(), "http" | "https") {
        return Err(Error::UnsupportedUrlScheme {
            scheme: base_url.scheme().to_owned(),
        });
    }

    if base_url.cannot_be_a_base() {
        return Err(Error::UrlCannotBeABase { url: base_url });
    }

    if base_url.query().is_some() || base_url.fragment().is_some() {
        return Err(Error::BaseUrlHasQueryOrFragment { url: base_url });
    }

    if !base_url.path().ends_with('/') {
        let mut path = base_url.path().to_owned();
        path.push('/');
        base_url.set_path(&path);
    }

    Ok(base_url)
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid Clash API base URL `{value}`: {source}")]
    InvalidBaseUrl {
        value: String,
        #[source]
        source: url::ParseError,
    },

    #[error("unsupported Clash API URL scheme `{scheme}`")]
    UnsupportedUrlScheme { scheme: String },

    #[error("URL cannot be used as a Clash API base URL: {url}")]
    UrlCannotBeABase { url: Url },

    #[error("Clash API base URL must not contain a query or fragment: {url}")]
    BaseUrlHasQueryOrFragment { url: Url },

    #[error("Clash API endpoint must be relative, got `{endpoint}`")]
    AbsoluteEndpoint { endpoint: String },

    #[error("invalid Clash API endpoint `{endpoint}`: {source}")]
    InvalidEndpoint {
        endpoint: String,
        #[source]
        source: url::ParseError,
    },

    #[error("{transport} transport is not supported on {platform}")]
    UnsupportedTransport {
        transport: &'static str,
        platform: &'static str,
    },

    #[error("failed to build the Clash API HTTP client: {0}")]
    BuildClient(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_base_url_is_normalized_and_endpoints_are_relative_to_it() {
        let client = Client::new_http("http://127.0.0.1:9090/api").unwrap();

        assert_eq!(client.base_url().as_str(), "http://127.0.0.1:9090/api/");

        let request = client.get("/version").unwrap().build().unwrap();
        assert_eq!(request.url().as_str(), "http://127.0.0.1:9090/api/version");
    }

    #[test]
    fn http_constructor_rejects_invalid_base_urls() {
        assert!(matches!(
            Client::new_http("not a URL"),
            Err(Error::InvalidBaseUrl { .. })
        ));
        assert!(matches!(
            Client::new_http("ftp://127.0.0.1/api"),
            Err(Error::UnsupportedUrlScheme { .. })
        ));
        assert!(matches!(
            Client::new_http("http://127.0.0.1/api?secret=value"),
            Err(Error::BaseUrlHasQueryOrFragment { .. })
        ));
    }

    #[test]
    fn absolute_endpoints_are_rejected() {
        let client = Client::new_http("http://127.0.0.1:9090").unwrap();

        assert!(matches!(
            client.get("https://example.com/version"),
            Err(Error::AbsoluteEndpoint { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn named_pipe_client_uses_a_synthetic_local_base_url() {
        let path = PathBuf::from(r"\\.\pipe\clash-api-test");
        let client = Client::new(Host::NamedPipe(path.clone())).unwrap();

        assert_eq!(client.host(), &Host::NamedPipe(path));
        assert_eq!(
            client.endpoint("/version").unwrap().as_str(),
            "http://localhost/version"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn named_pipe_transport_is_rejected_off_windows() {
        assert!(matches!(
            Client::new_named_pipe("clash-api-test"),
            Err(Error::UnsupportedTransport { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_client_uses_a_synthetic_local_base_url() {
        let path = PathBuf::from("/tmp/clash-api-test.sock");
        let client = Client::new(Host::UnixSocket(path.clone())).unwrap();

        assert_eq!(client.host(), &Host::UnixSocket(path));
        assert_eq!(
            client.endpoint("/version").unwrap().as_str(),
            "http://localhost/version"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn unix_socket_transport_is_rejected_off_unix() {
        assert!(matches!(
            Client::new_unix_socket("clash-api-test.sock"),
            Err(Error::UnsupportedTransport { .. })
        ));
    }
}
