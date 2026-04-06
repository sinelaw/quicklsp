/**
 * types.h — Core type definitions for the network library.
 */
#ifndef TYPES_H
#define TYPES_H

#include <stdint.h>
#include <stddef.h>

/* ── Enumerations ─────────────────────────────────────────────── */

/** Log severity levels. */
typedef enum {
    LOG_DEBUG   = 0,  /* @mark LOG_DEBUG_DEF */
    LOG_INFO    = 1,  /* @mark LOG_INFO_DEF */
    LOG_WARNING = 2,
    LOG_ERROR   = 3,  /* @mark LOG_ERROR_DEF */
    LOG_FATAL   = 4
} LogLevel;  /* @mark LogLevel_DEF */

/** Connection state machine. */
enum ConnState {
    CONN_IDLE,          /* @mark CONN_IDLE_DEF */
    CONN_CONNECTING,
    CONN_ESTABLISHED,   /* @mark CONN_ESTABLISHED_DEF */
    CONN_CLOSING,
    CONN_CLOSED
};

/** Request methods. */
enum HttpMethod {
    HTTP_GET,     /* @mark HTTP_GET_DEF */
    HTTP_POST,
    HTTP_PUT,
    HTTP_DELETE
};

/* ── Structures ───────────────────────────────────────────────── */

/** A growable byte buffer. */
typedef struct {
    uint8_t *data;
    size_t   len;
    size_t   cap;
} Buffer;  /* @mark Buffer_DEF */

/** IP address — supports both IPv4 and IPv6. */
struct Address {  /* @mark Address_DEF */
    char     host[256];
    uint16_t port;
    int      is_ipv6;
};

/** Single key-value header. */
struct Header {  /* @mark Header_DEF */
    char key[128];
    char value[512];
};

/** HTTP request with headers and body. */
struct Request {  /* @mark Request_DEF */
    enum HttpMethod   method;
    char              path[1024];
    struct Header     headers[32];
    int               header_count;
    Buffer            body;         /* @mark Request_body_field */
};

/** HTTP response. */
struct Response {  /* @mark Response_DEF */
    int               status_code;
    struct Header     headers[32];
    int               header_count;
    Buffer            body;
};

/** Connection handle. */
typedef struct {
    int              fd;
    struct Address   remote;       /* @mark Connection_remote_field */
    enum ConnState   state;        /* @mark Connection_state_field */
    uint64_t         bytes_sent;
    uint64_t         bytes_recv;
} Connection;  /* @mark Connection_DEF */

/* ── Typedefs ─────────────────────────────────────────────────── */

typedef void (*RequestHandler)(const struct Request *req, struct Response *resp);  /* @mark RequestHandler_DEF */
typedef int  (*Validator)(const char *input, size_t len);  /* @mark Validator_DEF */
typedef uint32_t StatusCode;  /* @mark StatusCode_DEF */

/* ── Macros ───────────────────────────────────────────────────── */

#define MAX_CONNECTIONS   1024  /* @mark MAX_CONNECTIONS_DEF */
#define MAX_HEADERS       32    /* @mark MAX_HEADERS_DEF */
#define BUFFER_INIT_CAP   4096
#define HTTP_OK           200   /* @mark HTTP_OK_DEF */
#define HTTP_NOT_FOUND    404
#define HTTP_SERVER_ERROR  500

/** Safe minimum. */
#define MIN(a, b) ((a) < (b) ? (a) : (b))  /* @mark MIN_DEF */

/** Safe maximum. */
#define MAX(a, b) ((a) > (b) ? (a) : (b))  /* @mark MAX_DEF */

/** Compile-time array length. */
#define ARRAY_LEN(arr) (sizeof(arr) / sizeof((arr)[0]))

/** Version string. */
#define VERSION_MAJOR  1
#define VERSION_MINOR  4
#define VERSION_PATCH  2
#define VERSION_STRING "1.4.2"  /* @mark VERSION_STRING_DEF */

#endif /* TYPES_H */
