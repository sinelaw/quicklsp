// QuickLSP evaluation fixture: a realistic C file.
// Exercises struct, enum, typedef, and function definitions.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/// Maximum number of retry attempts.
#define MAX_RETRIES 3

/// Default timeout in milliseconds.
#define DEFAULT_TIMEOUT 5000

/// Server configuration.
struct Config {
    char host[256];
    int port;
    int max_connections;
};

/// Status codes for handler lifecycle.
enum Status {
    STATUS_ACTIVE,
    STATUS_INACTIVE,
    STATUS_ERROR
};

typedef unsigned int StatusCode;
typedef struct Config ServerConfig;

/// Create a default configuration.
void init_config(struct Config *config) {
    strcpy(config->host, "localhost");
    config->port = 8080;
    config->max_connections = 100;
}

/// Validate a port number.
int validate_port(int port) {
    return port > 0 && port < 65535;
}

/// Process an incoming request using the given config.
/// Returns 0 on success, -1 on failure.
int process_request(struct Config *config, const char *method, const char *path) {
    if (!config || !method || !path) {
        return -1;
    }
    printf("Handled %s %s on %s:%d\n", method, path, config->host, config->port);
    return 0;
}

/// The main server structure.
struct Server {
    struct Config config;
    int running;
    int handler_count;
};

/// Initialize the server with a config.
void server_init(struct Server *server, struct Config config) {
    server->config = config;
    server->running = 0;
    server->handler_count = 0;
}

/// Start the server loop.
static void server_run(struct Server *server) {
    server->running = 1;
    for (int i = 0; i < MAX_RETRIES; i++) {
        int timeout = DEFAULT_TIMEOUT * (i + 1);
        printf("Attempt %d with timeout %dms on port %d\n",
               i, timeout, server->config.port);
    }
}

/// Stop the server gracefully.
void server_stop(struct Server *server) {
    server->running = 0;
}

/// Log a message at the given level.
void server_log(int level, const char *msg) {
    const char *levels[] = {"DEBUG", "INFO", "WARN", "ERROR"};
    if (level >= 0 && level < 4) {
        printf("[%s] %s\n", levels[level], msg);
    }
}

/// Helper to sanitize input strings.
char *sanitize_input(const char *input) {
    if (!input) return NULL;
    size_t len = strlen(input);
    char *result = malloc(len + 1);
    if (result) {
        strncpy(result, input, len + 1);
    }
    return result;
}

int main(int argc, char *argv[]) {
    struct Config config;
    init_config(&config);

    if (argc > 1) {
        config.port = atoi(argv[1]);
        if (!validate_port(config.port)) {
            server_log(3, "Invalid port");
            return 1;
        }
    }

    struct Server server;
    server_init(&server, config);
    server_run(&server);
    server_stop(&server);

    process_request(&config, "GET", "/index.html");
    server_log(1, "Server stopped");

    return 0;
}
