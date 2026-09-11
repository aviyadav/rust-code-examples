#[derive(Debug, Default)]
struct HttpRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout_ms: u64,
}

struct HttpRequestBuilder {
    request: HttpRequest,
}

impl HttpRequestBuilder {
    fn new(url: String) -> Self {
        HttpRequestBuilder {
            request: HttpRequest {
                url,
                method: "GET".to_string(),
                timeout_ms: 5000,
                ..Default::default()
            },
        }
    }

    fn method(mut self, method: &str) -> Self {
        self.request.method = method.to_string();
        self
    }

    fn header(mut self, key: &str, value: &str) -> Self {
        self.request
            .headers
            .push((key.to_string(), value.to_string()));
        self
    }

    fn body(mut self, body: &str) -> Self {
        self.request.body = Some(body.to_string());
        self
    }

    fn timeout(mut self, timeout_ms: u64) -> Self {
        self.request.timeout_ms = timeout_ms;
        self
    }

    fn build(self) -> HttpRequest {
        assert!(
            !self.request.url.is_empty(),
            "url must be set before build()"
        );
        self.request
    }
}

fn main() {
    let request = HttpRequestBuilder::new("https://api.example.com/data".to_string())
        .method("POST")
        .header("Content-Type", "application/json")
        .body(r#"{"key": "value"}"#)
        .timeout(10000)
        .build();

    println!("Built HTTP Request: {:?}", request);
}
