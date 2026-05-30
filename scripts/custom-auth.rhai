// Example Rhai middleware: Bearer token authentication.
//
// Checks the Authorization header for "Bearer <token>".
// Returns 401 with WWW-Authenticate when the header is missing or invalid.
//
// IMPORTANT: Replace EXPECTED_TOKEN with your actual secret.
// Use the header() middleware to inject the expected value via an env var:
//
//   "headers": { "X-Expected-Token": "$MY_API_TOKEN" }
//
// then read it with: request.header("X-Expected-Token")

let auth = request.header("Authorization");

if auth == "" {
    response.status = 401;
    response.body   = "Unauthorized";
    response.header("WWW-Authenticate", "Bearer realm=\"conduit\"");
    return false;
}

if !auth.starts_with("Bearer ") {
    response.status = 401;
    response.body   = "Unauthorized: expected Bearer token";
    response.header("WWW-Authenticate", "Bearer realm=\"conduit\"");
    return false;
}

// Extract the token (substring after "Bearer ").
let token = auth.sub_string(7);

// Compare against the expected token injected via a request header.
// Set EXPECTED_TOKEN in your conduit.json "headers" block using $ENV_VAR syntax.
let expected = request.header("X-Expected-Token");
if expected == "" || token != expected {
    response.status = 403;
    response.body   = "Forbidden";
    return false;
}

true
