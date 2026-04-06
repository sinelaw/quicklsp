/**
 * server.h — Server and utility function declarations.
 */
#ifndef SERVER_H
#define SERVER_H

#include "types.h"

/* ── Server configuration ─────────────────────────────────────── */

/** Server configuration with tunables. */
struct ServerConfig {  /* @mark ServerConfig_DEF */
    struct Address  listen_addr;
    int             max_connections;
    int             backlog;
    int             timeout_ms;
    LogLevel        log_level;
};

/** The main server handle. */
struct Server {  /* @mark Server_DEF */
    struct ServerConfig  config;
    Connection          *connections;
    int                  conn_count;
    int                  running;
    RequestHandler       handler;  /* @mark Server_handler_field */
};

/* ── Buffer operations ────────────────────────────────────────── */

/** Initialize buffer with default capacity. */
void buffer_init(Buffer *buf);  /* @mark buffer_init_DECL */

/** Append raw bytes to a buffer, growing if needed. */
int buffer_append(Buffer *buf, const uint8_t *data, size_t len);  /* @mark buffer_append_DECL */

/** Reset buffer length to zero without freeing memory. */
void buffer_clear(Buffer *buf);

/** Free buffer memory. */
void buffer_free(Buffer *buf);

/** Return current capacity. */
static inline size_t buffer_capacity(const Buffer *buf) {  /* @mark buffer_capacity_DEF */
    return buf ? buf->cap : 0;
}

/** Return non-zero if buffer is empty. */
static inline int buffer_empty(const Buffer *buf) {  /* @mark buffer_empty_DEF */
    return !buf || buf->len == 0;
}

/* ── Address operations ───────────────────────────────────────── */

/** Parse "host:port" string into an Address. */
int address_parse(struct Address *addr, const char *str);  /* @mark address_parse_DECL */

/** Format an Address as "host:port". */
int address_format(const struct Address *addr, char *out, size_t out_len);  /* @mark address_format_DECL */

/** Compare two addresses for equality. */
int address_equal(const struct Address *a, const struct Address *b);

/* ── Connection operations ────────────────────────────────────── */

/** Initialize a connection with a file descriptor. */
void connection_init(Connection *conn, int fd, const struct Address *remote);  /* @mark connection_init_DECL */

/** Close a connection and update its state. */
void connection_close(Connection *conn);

/** Send data on a connection. Returns bytes sent or -1. */
int connection_send(Connection *conn, const uint8_t *data, size_t len);

/** Receive data from a connection. Returns bytes received or -1. */
int connection_recv(Connection *conn, uint8_t *buf, size_t buf_len);

/* ── Request / Response helpers ───────────────────────────────── */

/** Initialize a request with defaults. */
void request_init(struct Request *req, enum HttpMethod method, const char *path);  /* @mark request_init_DECL */

/** Add a header to a request. */
int request_add_header(struct Request *req, const char *key, const char *value);

/** Find a header value by key (case-insensitive). Returns NULL if absent. */
const char *request_get_header(const struct Request *req, const char *key);  /* @mark request_get_header_DECL */

/** Initialize a response with a status code. */
void response_init(struct Response *resp, int status_code);

/** Set the response body from a string. */
int response_set_body(struct Response *resp, const char *body);  /* @mark response_set_body_DECL */

/* ── Server lifecycle ─────────────────────────────────────────── */

/** Create and configure a new server. */
int server_create(struct Server *server, const struct ServerConfig *config);  /* @mark server_create_DECL */

/** Start accepting connections. Blocks until server_stop() is called. */
int server_run(struct Server *server);

/** Signal the server to stop. */
void server_stop(struct Server *server);

/** Clean up all server resources. */
void server_destroy(struct Server *server);

/** Register a request handler. */
void server_set_handler(struct Server *server, RequestHandler handler);  /* @mark server_set_handler_DECL */

/* ── Logging ──────────────────────────────────────────────────── */

/** Log a message at the given level. */
void server_log(LogLevel level, const char *fmt, ...);  /* @mark server_log_DECL */

/* ── Validation utilities ─────────────────────────────────────── */

/** Validate a port number (1–65535). */
static inline int validate_port(uint16_t port) {  /* @mark validate_port_DEF */
    return port > 0;  /* port is unsigned, always >= 0 */
}

/** Validate that a path starts with '/'. */
static inline int validate_path(const char *path) {  /* @mark validate_path_DEF */
    return path && path[0] == '/';
}

/** Validate request headers count. */
static inline int validate_headers(const struct Request *req) {  /* @mark validate_headers_DEF */
    return req && req->header_count >= 0 && req->header_count <= MAX_HEADERS;  /* @mark validate_headers_MAX_HEADERS_USE */
}

#endif /* SERVER_H */
