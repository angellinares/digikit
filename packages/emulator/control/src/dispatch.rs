//! One request in, one response out. The transport (socket, `postMessage`)
//! only moves lines or objects; everything else is here.

use serde_json::{Value, json};

use crate::{
    controller::Handle,
    proto::{self, Params, Request, RpcError, SCHEMA_VERSION, code, error_response, ok_response},
};

/// The methods the controller serves (`hello` lists them).
pub const METHODS: &[&str] = &[
    "hello",
    "status",
    "firmware.load",
    "restart",
    "run.mode",
    "step",
    "run_until",
    "input.button",
    "input.turn",
    "input.tap",
    "input.schedule",
    "inputs.recorded",
    "framebuffer.get",
    "leds.get",
    "diagnostics",
    "audio.report",
    "state.digest",
    "pcm.tap",
    "cf.profile",
    "dsp.patch",
    "dsp.peek",
    "dsp.peek_word",
    "dsp.ddr_monitor",
    "guard.set",
    "guard.report",
    "guard.clear",
    "events.subscribe",
    "quit",
];

#[derive(Clone)]
pub struct Dispatcher {
    handle: Handle,
    host: String,
}

impl Dispatcher {
    pub fn new(handle: Handle, host: impl Into<String>) -> Self {
        Self {
            handle,
            host: host.into(),
        }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    fn hello(&self, params: &Value) -> Result<Value, RpcError> {
        let p = Params(params);
        if let Some(min) = p.opt_u64("min_schema")?
            && min > u64::from(SCHEMA_VERSION)
        {
            return Err(RpcError::bad_params(format!(
                "schema {min} is newer than this host's {SCHEMA_VERSION}"
            )));
        }
        Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "host": self.host,
            "features": {
                "coupled_audio": cfg!(feature = "coupled-audio"),
                "server": cfg!(feature = "server"),
            },
            "methods": METHODS,
        }))
    }

    /// Run a parsed request. `None` for a notification (no `id`).
    pub fn handle_request(&self, request: Request) -> Option<Value> {
        let result = match request.method.as_str() {
            "hello" => self.hello(&request.params),
            method => self.handle.call(method, request.params),
        };
        let id = request.id?;
        Some(match result {
            Ok(value) => ok_response(id, value),
            Err(error) => error_response(id, &error),
        })
    }

    /// One JSON-RPC line (or object text) to its response text, if any.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let response = match proto::parse_request(line) {
            Ok(request) => self.handle_request(request)?,
            Err(response) => response,
        };
        Some(response.to_string())
    }
}

/// The error for a method that only a transport can serve (`events.subscribe`
/// reached a dispatcher without a stream to push to).
pub fn needs_stream(method: &str) -> RpcError {
    RpcError::new(
        code::METHOD_NOT_FOUND,
        format!("{method} needs a streaming transport"),
    )
}
