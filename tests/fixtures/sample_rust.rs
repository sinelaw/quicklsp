// QuickLSP evaluation fixture: a realistic multi-construct Rust file.
// This file is indexed by the LSP evaluation test to exercise all features.

/// Maximum number of retry attempts for server operations.
const MAX_RETRIES: u32 = 3; // @mark MAX_RETRIES_DEF

/// Default request timeout in milliseconds.
const DEFAULT_TIMEOUT: u64 = 5000; // @mark DEFAULT_TIMEOUT_DEF

/// Configuration for the server instance.
///
/// Holds connection parameters including host, port, and pool size.
struct Config { // @mark Config_DEF
    host: String,
    port: u16,
    max_connections: usize,
}

/// Represents the lifecycle status of a handler.
enum Status { // @mark Status_DEF
    Active,
    Inactive,
    Error(String),
}

/// Trait for request handlers.
///
/// Implementors must provide a handle method and a name identifier.
trait Handler { // @mark Handler_DEF
    fn handle(&self, request: &Request) -> Response;
    fn name(&self) -> &str;
}

/// An incoming HTTP request.
struct Request { // @mark Request_DEF
    method: String,
    path: String,
    body: Option<String>,
}

/// An HTTP response with status code and body.
struct Response { // @mark Response_DEF
    status: u16,
    body: String,
}

/// Create a new default configuration.
fn create_config() -> Config { // @mark create_config_DEF
    Config {
        host: "localhost".to_string(),
        port: 8080,
        max_connections: 100,
    }
}

/// Process an incoming request with the given configuration.
///
/// Routes the request and generates an appropriate response.
fn process_request(config: &Config, request: &Request) -> Response { // @mark process_request_DEF
    let status = if request.method == "GET" {
        Status::Active // @mark USE_Status_Active
    } else {
        Status::Inactive
    };

    let body = format!(
        "Handled {} {} on {}:{}",
        request.method, request.path, config.host, config.port
    );

    Response {
        status: 200,
        body,
    }
}

/// The main HTTP server that dispatches requests to handlers.
struct Server { // @mark Server_DEF
    config: Config,
    handlers: Vec<Box<dyn Handler>>, // @mark USE_Handler_dyn
}

impl Server {
    fn new(config: Config) -> Self { // @mark Server_new_DEF
        Server {
            config,
            handlers: Vec::new(),
        }
    }

    fn add_handler(&mut self, handler: Box<dyn Handler>) { // @mark Server_add_handler_DEF
        self.handlers.push(handler);
    }

    fn run(&self) { // @mark Server_run_DEF
        let config = &self.config;
        for i in 0..MAX_RETRIES { // @mark USE_MAX_RETRIES
            let timeout = DEFAULT_TIMEOUT * (i as u64 + 1); // @mark USE_DEFAULT_TIMEOUT
            println!("Attempt {} with timeout {}ms on port {}", i, timeout, config.port); // @mark USE_println
        }
    }
}

mod utils { // @mark utils_DEF
    pub fn sanitize_input(input: &str) -> String { // @mark sanitize_input_DEF
        input.trim().to_lowercase()
    }

    pub fn validate_port(port: u16) -> bool { // @mark validate_port_DEF
        port > 0 && port < 65535
    }
}

type StatusCode = u16; // @mark StatusCode_DEF
type HandlerResult = Result<Response, String>; // @mark HandlerResult_DEF

/// Validate an incoming request, returning an error if the path is empty.
fn validate_request(request: &Request) -> HandlerResult { // @mark validate_request_DEF
    if request.path.is_empty() {
        return Err("Empty path".to_string());
    }
    Ok(Response {
        status: 200,
        body: "OK".to_string(),
    })
}

// Unicode identifiers
fn données_utilisateur() -> String { // @mark unicode_fn_DEF
    "user data".to_string()
}

struct Über { // @mark unicode_struct_DEF
    wert: u32,
}

// Nested function-like constructs
fn outer() { // @mark outer_DEF
    fn inner() { // @mark inner_DEF
        let _ = 42;
    }
    inner(); // @mark CALL_inner
}

const FINAL_STATUS: &str = "complete"; // @mark FINAL_STATUS_DEF
static GLOBAL_COUNTER: u32 = 0; // @mark GLOBAL_COUNTER_DEF

/// A helper that calls process_request for testing references.
fn run_example() { // @mark run_example_DEF
    let cfg = create_config(); // @mark CALL_create_config
    let req = Request { method: "GET".to_string(), path: "/".to_string(), body: None };
    let _resp = process_request(&cfg, &req); // @mark CALL_process_request
    let _val = validate_request(&req); // @mark CALL_validate_request
    let _s = données_utilisateur(); // @mark CALL_unicode_fn
    let _ = "inside a string literal"; // @mark INSIDE_STRING
}
