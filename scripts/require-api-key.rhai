// Example Rhai middleware: require an X-API-Key header.
//
// Copy this file and adjust the key check to suit your application.
// Place the script path in conduit.json:
//
//   "middleware": [{ "type": "script", "path": "./scripts/require-api-key.rhai" }]

let key = request.header("X-API-Key");

if key == "" {
    response.status = 401;
    response.body   = "Missing X-API-Key header";
    return false;
}

// Validate the key against an expected value.
// Inject the expected value via conduit's $ENV_VAR interpolation in conduit.json:
//
//   "headers": { "X-Expected-Api-Key": "$MY_API_KEY" }
//
// then read it here with: request.header("X-Expected-Api-Key")
let expected = request.header("X-Expected-Api-Key");
if expected == "" || key != expected {
    response.status = 403;
    response.body   = "Invalid API key";
    return false;
}

// Pass the request through.
true
