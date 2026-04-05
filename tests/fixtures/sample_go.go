// QuickLSP evaluation fixture: a realistic Go file.
// Exercises func, type, var, const definitions.

package main

import (
	"fmt"
	"strings"
)

// MaxRetries is the maximum number of retry attempts.
const MaxRetries = 3

// DefaultTimeout is the timeout in milliseconds.
const DefaultTimeout = 5000

// Config holds the server configuration.
type Config struct {
	Host           string
	Port           int
	MaxConnections int
}

// Status represents the handler lifecycle state.
type Status int

const (
	StatusActive   Status = iota
	StatusInactive
	StatusError
)

// HandlerFunc defines the handler function signature.
type HandlerFunc func(request *Request) *Response

// Request represents an incoming HTTP request.
type Request struct {
	Method string
	Path   string
	Body   string
}

// Response represents an HTTP response.
type Response struct {
	StatusCode int
	Body       string
}

// NewConfig creates a default configuration.
func NewConfig() *Config {
	return &Config{
		Host:           "localhost",
		Port:           8080,
		MaxConnections: 100,
	}
}

// ValidatePort checks whether a port number is valid.
func ValidatePort(port int) bool {
	return port > 0 && port < 65535
}

// ProcessRequest handles an incoming request with the given config.
func ProcessRequest(config *Config, request *Request) *Response {
	body := fmt.Sprintf("Handled %s %s on %s:%d",
		request.Method, request.Path, config.Host, config.Port)
	return &Response{StatusCode: 200, Body: body}
}

// Server is the main HTTP server.
type Server struct {
	config   *Config
	handlers []HandlerFunc
	running  bool
}

// NewServer creates a server with the given config.
func NewServer(config *Config) *Server {
	return &Server{
		config:   config,
		handlers: make([]HandlerFunc, 0),
	}
}

// AddHandler registers a new handler with the server.
func (s *Server) AddHandler(handler HandlerFunc) {
	s.handlers = append(s.handlers, handler)
}

// Run starts the server loop.
func (s *Server) Run() {
	s.running = true
	for i := 0; i < MaxRetries; i++ {
		timeout := DefaultTimeout * (i + 1)
		fmt.Printf("Attempt %d with timeout %dms on port %d\n",
			i, timeout, s.config.Port)
	}
}

// SanitizeInput trims and lowercases the input string.
func SanitizeInput(input string) string {
	return strings.TrimSpace(strings.ToLower(input))
}

var globalCounter int

func main() {
	config := NewConfig()
	server := NewServer(config)
	server.Run()
}
