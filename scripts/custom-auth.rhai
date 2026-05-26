// Example Rhai middleware: Bearer token authentication.
//
// Checks the Authorization header for "Bearer <token>".
// Returns 401 with WWW-Authenticate when the header is missing or invalid.

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

if token != "my-secret-token" {
    response.status = 403;
    response.body   = "Forbidden";
    return false;
}

true
