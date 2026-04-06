/**
 * main.c — Full-featured C source exercising every syntax element from
 *          types.h and server.h.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>

#include "types.h"
#include "server.h"

/* ── File-scope globals ───────────────────────────────────────── */

static LogLevel g_log_level = LOG_INFO;  /* @mark g_log_level_DEF  @mark USE_LOG_INFO_global */
static int      g_request_count = 0;

/* ── Helper: format a method enum as a string ─────────────────── */

static const char *method_to_string(enum HttpMethod method) {  /* @mark method_to_string_DEF */
    switch (method) {
    case HTTP_GET:    return "GET";    /* @mark USE_HTTP_GET_switch */
    case HTTP_POST:   return "POST";
    case HTTP_PUT:    return "PUT";
    case HTTP_DELETE: return "DELETE";
    default:          return "UNKNOWN";
    }
}

/* ── Helper: compute simple hash of a string ──────────────────── */

static uint32_t hash_string(const char *s) {  /* @mark hash_string_DEF */
    uint32_t h = 5381;
    while (*s) {
        h = ((h << 5) + h) + (uint32_t)*s++;
    }
    return h;
}

/* ── Buffer helpers (implementations) ─────────────────────────── */

void buffer_init(Buffer *buf) {  /* @mark buffer_init_IMPL */
    buf->data = (uint8_t *)malloc(BUFFER_INIT_CAP);
    buf->len  = 0;
    buf->cap  = BUFFER_INIT_CAP;
}

int buffer_append(Buffer *buf, const uint8_t *data, size_t len) {  /* @mark buffer_append_IMPL */
    if (!buf || !data) return -1;
    if (buf->len + len > buf->cap) {
        size_t new_cap = MAX(buf->cap * 2, buf->len + len);  /* @mark USE_MAX_macro */
        uint8_t *new_data = (uint8_t *)realloc(buf->data, new_cap);
        if (!new_data) return -1;
        buf->data = new_data;
        buf->cap  = new_cap;
    }
    memcpy(buf->data + buf->len, data, len);
    buf->len += len;
    return 0;
}

void buffer_clear(Buffer *buf) {
    if (buf) buf->len = 0;
}

void buffer_free(Buffer *buf) {
    if (buf) {
        free(buf->data);
        buf->data = NULL;
        buf->len  = 0;
        buf->cap  = 0;
    }
}

/* ── Address helpers ──────────────────────────────────────────── */

int address_parse(struct Address *addr, const char *str) {  /* @mark address_parse_IMPL */
    if (!addr || !str) return -1;
    const char *colon = strrchr(str, ':');
    if (!colon) return -1;
    size_t host_len = MIN((size_t)(colon - str), sizeof(addr->host) - 1);  /* @mark USE_MIN_macro */
    memcpy(addr->host, str, host_len);
    addr->host[host_len] = '\0';
    addr->port = (uint16_t)atoi(colon + 1);
    addr->is_ipv6 = (strchr(addr->host, ':') != NULL) ? 1 : 0;
    return 0;
}

int address_format(const struct Address *addr, char *out, size_t out_len) {  /* @mark address_format_IMPL */
    if (!addr || !out) return -1;
    return snprintf(out, out_len, "%s:%u", addr->host, addr->port);
}

int address_equal(const struct Address *a, const struct Address *b) {
    return a && b
        && strcmp(a->host, b->host) == 0
        && a->port == b->port;
}

/* ── Connection helpers ───────────────────────────────────────── */

void connection_init(Connection *conn, int fd, const struct Address *remote) {  /* @mark connection_init_IMPL */
    conn->fd         = fd;
    conn->remote     = *remote;
    conn->state      = CONN_IDLE;  /* @mark USE_CONN_IDLE */
    conn->bytes_sent = 0;
    conn->bytes_recv = 0;
}

void connection_close(Connection *conn) {  /* @mark connection_close_IMPL */
    if (conn && conn->state != CONN_CLOSED) {
        conn->state = CONN_CLOSED;
        conn->fd    = -1;
    }
}

/* ── Request / Response helpers ───────────────────────────────── */

void request_init(struct Request *req, enum HttpMethod method, const char *path) {  /* @mark request_init_IMPL */
    memset(req, 0, sizeof(*req));
    req->method = method;
    strncpy(req->path, path, sizeof(req->path) - 1);
    buffer_init(&req->body);  /* @mark CALL_buffer_init_in_request */
}

int request_add_header(struct Request *req, const char *key, const char *value) {
    if (!req || req->header_count >= MAX_HEADERS) return -1;  /* @mark USE_MAX_HEADERS */
    struct Header *h = &req->headers[req->header_count++];  /* @mark USE_Header_struct */
    strncpy(h->key, key, sizeof(h->key) - 1);
    strncpy(h->value, value, sizeof(h->value) - 1);
    return 0;
}

const char *request_get_header(const struct Request *req, const char *key) {  /* @mark request_get_header_IMPL */
    if (!req || !key) return NULL;
    for (int i = 0; i < req->header_count; i++) {
        if (strcasecmp(req->headers[i].key, key) == 0) {
            return req->headers[i].value;
        }
    }
    return NULL;
}

void response_init(struct Response *resp, int status_code) {  /* @mark response_init_IMPL */
    memset(resp, 0, sizeof(*resp));
    resp->status_code = status_code;
    buffer_init(&resp->body);
}

int response_set_body(struct Response *resp, const char *body) {  /* @mark response_set_body_IMPL */
    if (!resp || !body) return -1;
    buffer_clear(&resp->body);
    return buffer_append(&resp->body, (const uint8_t *)body, strlen(body));  /* @mark CALL_buffer_append_in_set_body */
}

/* ── Server implementation ────────────────────────────────────── */

int server_create(struct Server *server, const struct ServerConfig *config) {  /* @mark server_create_IMPL */
    if (!server || !config) return -1;
    server->config   = *config;
    server->conn_count = 0;
    server->running  = 0;
    server->handler  = NULL;
    server->connections = (Connection *)calloc(  /* @mark USE_Connection_in_calloc */
        (size_t)config->max_connections, sizeof(Connection));
    return server->connections ? 0 : -1;
}

void server_set_handler(struct Server *server, RequestHandler handler) {  /* @mark server_set_handler_IMPL */
    if (server) server->handler = handler;
}

void server_stop(struct Server *server) {
    if (server) server->running = 0;
}

void server_destroy(struct Server *server) {
    if (!server) return;
    for (int i = 0; i < server->conn_count; i++) {
        connection_close(&server->connections[i]);  /* @mark CALL_connection_close */
    }
    free(server->connections);
    server->connections = NULL;
    server->conn_count  = 0;
}

/* ── The main request handler callback ────────────────────────── */

static void handle_request(const struct Request *req, struct Response *resp) {  /* @mark handle_request_DEF */
    const char *method_str = method_to_string(req->method);  /* @mark CALL_method_to_string */
    StatusCode code = HTTP_OK;  /* @mark USE_StatusCode  @mark USE_HTTP_OK */

    /* Route by method + path */
    if (req->method == HTTP_GET) {  /* @mark IF_HTTP_GET */
        if (strcmp(req->path, "/") == 0 || strcmp(req->path, "/index") == 0) {
            response_init(resp, HTTP_OK);  /* @mark CALL_response_init_ok */
            response_set_body(resp, "<html><body>Welcome</body></html>");  /* @mark CALL_response_set_body */
        } else if (strcmp(req->path, "/status") == 0) {
            response_init(resp, HTTP_OK);
            char status_buf[256];
            snprintf(status_buf, sizeof(status_buf),
                     "{\"requests\": %d, \"version\": \"%s\"}",
                     g_request_count, VERSION_STRING);  /* @mark USE_VERSION_STRING */
        } else {
            code = HTTP_NOT_FOUND;  /* @mark USE_HTTP_NOT_FOUND */
            response_init(resp, (int)code);
            response_set_body(resp, "Not Found");
        }
    } else if (req->method == HTTP_POST) {
        if (validate_path(req->path) && validate_headers(req)) {  /* @mark CALL_validate_path  @mark CALL_validate_headers */
            const char *content_type = request_get_header(req, "Content-Type");  /* @mark CALL_request_get_header */
            if (content_type && strstr(content_type, "json")) {
                response_init(resp, HTTP_OK);
                response_set_body(resp, "{\"status\": \"accepted\"}");
            } else {
                response_init(resp, HTTP_OK);
                response_set_body(resp, "OK");
            }
        } else {
            response_init(resp, HTTP_SERVER_ERROR);  /* @mark USE_HTTP_SERVER_ERROR */
        }
    } else {
        response_init(resp, HTTP_SERVER_ERROR);
    }

    g_request_count++;
    printf("[%s] %s -> %d\n", method_str, req->path,
           resp->status_code);
}

/* ── Process all pending connections ──────────────────────────── */

static int process_connections(struct Server *server) {  /* @mark process_connections_DEF */
    int processed = 0;
    struct Request  req;
    struct Response resp;

    for (int i = 0; i < server->conn_count; i++) {
        Connection *conn = &server->connections[i];  /* @mark USE_Connection_in_loop */

        if (conn->state != CONN_ESTABLISHED) {  /* @mark USE_CONN_ESTABLISHED_in_if */
            continue;
        }

        /* Simulate reading a request */
        request_init(&req, HTTP_GET, "/");  /* @mark CALL_request_init */
        request_add_header(&req, "Host", conn->remote.host);  /* @mark ACCESS_conn_remote_host */

        /* Format remote address for logging */
        char addr_buf[280];
        address_format(&conn->remote, addr_buf, sizeof(addr_buf));  /* @mark CALL_address_format */

        /* Validate before processing */
        if (!validate_port(conn->remote.port)) {  /* @mark CALL_validate_port */
            server_log(LOG_WARNING, "Invalid port on connection %d", i);  /* @mark CALL_server_log_warning */
            connection_close(conn);
            continue;
        }

        /* Dispatch to handler */
        if (server->handler) {
            server->handler(&req, &resp);  /* @mark CALL_handler_fnptr */
        } else {
            handle_request(&req, &resp);  /* @mark CALL_handle_request_fallback */
        }

        /* Track bytes */
        conn->bytes_sent += resp.body.len;  /* @mark ACCESS_bytes_sent */
        conn->bytes_recv += req.body.len;

        /* Clean up per-request buffers */
        buffer_free(&req.body);  /* @mark CALL_buffer_free */
        buffer_free(&resp.body);

        processed++;
    }

    return processed;
}

/* ── Simulate the server main loop ────────────────────────────── */

static void run_loop(struct Server *server) {  /* @mark run_loop_DEF */
    int iteration = 0;

    while (server->running) {
        int n = process_connections(server);  /* @mark CALL_process_connections */

        if (n == 0 && iteration > 0) {
            if (iteration >= MAX_RETRIES) {  /* @mark USE_MAX_RETRIES -- not in types.h but shows macro usage */
                server_log(LOG_INFO, "Max retries reached, stopping");  /* @mark CALL_server_log_info */
                break;
            }
        }

        /* Periodic health check */
        if (iteration % 10 == 0) {
            int active = 0;
            for (int j = 0; j < server->conn_count; j++) {
                enum ConnState st = server->connections[j].state;  /* @mark USE_ConnState_in_loop */
                if (st == CONN_ESTABLISHED || st == CONN_CONNECTING) {
                    active++;
                }
            }
            server_log(LOG_DEBUG, "Tick %d: %d active connections", iteration, active);
        }

        iteration++;

        /* Simulate backoff with do-while */
        int backoff_ms = 100;  /* @mark backoff_ms_local_var */
        do {
            backoff_ms = MIN(backoff_ms * 2, server->config.timeout_ms);  /* @mark USE_MIN_in_dowhile */
        } while (backoff_ms < 1000 && server->running);
    }
}

int server_run(struct Server *server) {  /* @mark server_run_IMPL */
    if (!server) return -1;
    server->running = 1;
    server_log(LOG_INFO, "Server starting on port %u",
               server->config.listen_addr.port);  /* @mark ACCESS_listen_addr_port */
    run_loop(server);  /* @mark CALL_run_loop */
    server_log(LOG_INFO, "Server stopped");
    return 0;
}

/* ── Logging implementation ───────────────────────────────────── */

void server_log(LogLevel level, const char *fmt, ...) {  /* @mark server_log_IMPL */
    if (level < g_log_level) return;
    static const char *level_names[] = {
        "DEBUG", "INFO", "WARN", "ERROR", "FATAL"
    };
    const char *name = (level >= 0 && level <= LOG_FATAL)  /* @mark USE_LOG_FATAL */
        ? level_names[level] : "???";

    va_list args;
    va_start(args, fmt);
    fprintf(stderr, "[%s] ", name);
    vfprintf(stderr, fmt, args);
    fputc('\n', stderr);
    va_end(args);
}

/* ── Batch processing helper using function pointer ───────────── */

static int process_batch(  /* @mark process_batch_DEF */
    Connection  *conns,
    int          count,
    Validator    path_validator,  /* @mark USE_Validator_param */
    RequestHandler handler  /* @mark USE_RequestHandler_param */
) {
    int ok = 0;
    for (int i = 0; i < count; i++) {
        if (conns[i].state != CONN_ESTABLISHED) continue;

        struct Request  req;
        struct Response resp;
        request_init(&req, HTTP_GET, "/batch");

        /* Use the validator function pointer */
        if (path_validator && !path_validator(req.path, strlen(req.path))) {  /* @mark CALL_path_validator_fnptr */
            buffer_free(&req.body);
            continue;
        }

        /* Dispatch */
        handler(&req, &resp);  /* @mark CALL_handler_param_fnptr */
        ok++;

        buffer_free(&req.body);
        buffer_free(&resp.body);
    }
    return ok;
}

/* ── Entry point ──────────────────────────────────────────────── */

int main(int argc, char *argv[]) {  /* @mark main_DEF */
    /* Parse configuration from command line or defaults */
    struct ServerConfig config = {  /* @mark USE_ServerConfig */
        .listen_addr     = { .host = "0.0.0.0", .port = 8080, .is_ipv6 = 0 },
        .max_connections = MAX_CONNECTIONS,  /* @mark USE_MAX_CONNECTIONS */
        .backlog         = 128,
        .timeout_ms      = 5000,
        .log_level       = LOG_INFO,
    };

    /* Override port from argv */
    if (argc > 1) {
        uint16_t port = (uint16_t)atoi(argv[1]);
        if (validate_port(port)) {  /* @mark CALL_validate_port_in_main */
            config.listen_addr.port = port;
        } else {
            server_log(LOG_ERROR, "Invalid port: %s", argv[1]);  /* @mark CALL_server_log_error */
            return 1;
        }
    }

    /* Override log level */
    if (argc > 2) {
        int level = atoi(argv[2]);
        config.log_level = (LogLevel)level;  /* @mark CAST_LogLevel */
        g_log_level = config.log_level;
    }

    /* Create & configure server */
    struct Server server;  /* @mark USE_Server_struct */
    if (server_create(&server, &config) != 0) {  /* @mark CALL_server_create */
        server_log(LOG_FATAL, "Failed to create server");
        return 1;
    }
    server_set_handler(&server, handle_request);  /* @mark CALL_server_set_handler */

    /* Log configuration */
    char addr_str[280];
    address_format(&config.listen_addr, addr_str, sizeof(addr_str));  /* @mark CALL_address_format_in_main */
    server_log(LOG_INFO, "Listening on %s (max %d conns, version %s)",
               addr_str, config.max_connections, VERSION_STRING);  /* @mark USE_VERSION_STRING_in_main */

    /* Simulate adding some connections */
    for (int i = 0; i < MIN(3, config.max_connections); i++) {  /* @mark USE_MIN_in_for */
        struct Address remote;
        char addr_buf[32];
        snprintf(addr_buf, sizeof(addr_buf), "10.0.0.%d:9000", i + 1);
        address_parse(&remote, addr_buf);  /* @mark CALL_address_parse */
        connection_init(&server.connections[i], i + 10, &remote);  /* @mark CALL_connection_init */
        server.connections[i].state = CONN_ESTABLISHED;  /* @mark SET_CONN_ESTABLISHED */
        server.conn_count++;
    }

    /* Process a batch using function pointers */
    int batch_ok = process_batch(  /* @mark CALL_process_batch */
        server.connections, server.conn_count,
        (Validator)validate_path,  /* @mark CAST_Validator */
        handle_request  /* @mark PASS_handle_request_as_fnptr */
    );
    server_log(LOG_INFO, "Batch processed %d requests", batch_ok);

    /* Run the server */
    server_run(&server);  /* @mark CALL_server_run */

    /* Clean up */
    server_destroy(&server);  /* @mark CALL_server_destroy */
    server_log(LOG_INFO, "Goodbye (processed %d total requests)", g_request_count);

    return 0;
}
