// QuickLSP evaluation fixture: a consumer file that references symbols
// defined in sample_rust.rs. Used to test cross-file LSP features
// (hover, goto-def, completion) WITHOUT same-file ranking advantage.

/// Uses Config from sample_rust.rs — cross-file reference.
fn start_server(config: &Config) { // @mark XFILE_USE_Config
    let _status = Status::Active; // @mark XFILE_USE_Status
    let _req = Request { method: "POST".to_string(), path: "/api".to_string(), body: None }; // @mark XFILE_USE_Request
    let _resp = process_request(config, &_req); // @mark XFILE_CALL_process_request
}

/// Another cross-file Config usage in return type position.
fn get_config() -> Config { // @mark XFILE_Config_return
    create_config()
}

/// Cross-file trait usage.
fn register_handler(server: &mut Server, handler: Box<dyn Handler>) { // @mark XFILE_USE_Handler
    server.add_handler(handler);
}

/// Call site for signature help testing across files.
fn do_work() {
    let cfg = create_config(); // @mark XFILE_CALL_create_config
    let req = Request { method: "GET".to_string(), path: "/".to_string(), body: None };
    let _resp = process_request(&cfg, &req); // @mark XFILE_CALL_process_request2
    let _val = validate_request(&req); // @mark XFILE_CALL_validate_request
}

/// Completion test anchor: type prefix on a fresh line.
fn completion_anchor() {
    let _c: Conf; // @mark XFILE_COMPLETION_Conf
}
