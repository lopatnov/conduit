use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::handler::response::write_response;

static HEALTH_BODY: &[u8] = b"{\"status\":\"ok\"}";

pub async fn handle_health(session: &mut Session, extra: &[(String, String)]) -> Result<()> {
    write_response(
        session,
        200,
        "application/json",
        Bytes::from_static(HEALTH_BODY),
        extra,
    )
    .await
}
