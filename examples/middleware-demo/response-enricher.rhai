// response-enricher.rhai — response-phase middleware
//
// Adds observability headers and removes sensitive ones from every response.
//
// Plugin config (optional):
//   {
//     "service_name": "my-api",      // shown in X-Served-By
//     "hide_server":  true           // remove upstream Server header
//   }

let status = upstream.status;

// Always advertise which proxy served the request.
let svc = if config.service_name != () { config.service_name } else { "conduit" };
response.set_header("X-Served-By", svc);

// Categorise by HTTP status class.
if status >= 500 {
    response.set_header("X-Error-Category", "upstream-failure");
} else if status >= 400 {
    response.set_header("X-Error-Category", "client-error");
} else if status >= 300 {
    response.set_header("X-Error-Category", "redirect");
}

// Security hygiene — strip headers that leak implementation details.
response.remove_header("X-Powered-By");
response.remove_header("X-AspNet-Version");
response.remove_header("X-Generator");

if config.hide_server == true {
    response.remove_header("Server");
}
