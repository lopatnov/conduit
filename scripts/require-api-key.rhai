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

// Optional: validate the key against a known value.
// For production, use conduit's $ENV_VAR interpolation to inject secrets:
//   "middleware": [{ "type": "script", "path": "./scripts/require-api-key.rhai" }]
// and store the expected key in the script or in a side-car config file.
if key != "changeme" {
    response.status = 403;
    response.body   = "Invalid API key";
    return false;
}

// Pass the request through.
true
