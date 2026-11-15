//! The `ext_proc` phase of the response chain (issue #27): the response-headers exchange with the gRPC processors.
//!
//! It is an [`AsyncResponseFilter`] (it awaits the network) and the first stage of
//! [`ResponseFilterChain`](crate::filter::response_chain::ResponseFilterChain). Which phase of an answer is final, and
//! what a replaced response means for a retry, is decided by the chain (`run_async`), not here.

use std::sync::Arc;

use async_trait::async_trait;
use conduit_ext_proc::ExtProcSessions;
use pingora_core::Result;
use pingora_http::ResponseHeader;

use crate::filter::response_chain::{AsyncResponseFilter, ResponseRunState};
use crate::proxy::response_phase::{body_can_be_replaced, warn_body_not_replaceable};

/// Tells every `ext_proc` entry that handles the response the upstream's headers, and applies what they answer.
pub struct ExtProcResponseFilter {
    sessions: Arc<ExtProcSessions>,
}

impl ExtProcResponseFilter {
    pub fn new(sessions: Arc<ExtProcSessions>) -> Self {
        Self { sessions }
    }
}

#[async_trait]
impl AsyncResponseFilter for ExtProcResponseFilter {
    async fn apply(&self, resp: &mut ResponseHeader, state: &mut ResponseRunState) -> Result<()> {
        // Whether a body follows is not known here; one that can be replaced is treated as one that exists.
        let body_allowed = body_can_be_replaced(resp);
        if let Some(answer) =
            conduit_middleware::ext_proc::response_headers(&self.sessions, resp, !body_allowed)
                .await
        {
            if !body_allowed && !answer.body.is_empty() {
                warn_body_not_replaceable(answer.status);
            }
            state.replaced = true;
            state.replacement_body =
                conduit_middleware::ext_proc::replace_response(resp, &answer, body_allowed);
        }
        Ok(())
    }
}
