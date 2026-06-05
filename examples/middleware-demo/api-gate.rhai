// api-gate.rhai — request-phase middleware
//
// Enforces a simple API key gate.
//
// Plugin config (set in conduit.yaml middleware[].config):
//   {
//     "api_key":    "secret-key-here",
//     "api_header": "x-api-key"        // optional, default: "x-api-key"
//   }
//
// Behaviour:
//   - If the configured header is absent  → 401 Unauthorized
//   - If the key does not match           → 403 Forbidden
//   - If key matches                      → inject X-Authenticated: 1, continue

let header_name = if config.api_header != ()  { config.api_header } else { "x-api-key" };
let expected     = config.api_key;
let provided     = request.header(header_name);

if provided == "" {
    response.status = 401;
    response.body   = `{"error":"Missing ${header_name}","status":401}`;
    response.header("Content-Type", "application/json");
    response.header("WWW-Authenticate", `ApiKey realm="${header_name}"`);
    return false;
}

if provided != expected {
    response.status = 403;
    response.body   = `{"error":"Invalid ${header_name}","status":403}`;
    response.header("Content-Type", "application/json");
    return false;
}

// Key is valid — allow the request through.
// Header injection (X-Authenticated) happens via requestTransform in conduit.yaml, not here.
true
