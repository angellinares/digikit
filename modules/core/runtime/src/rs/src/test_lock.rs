//! One lock for the tests that drive the runtime statics (card, arena, registry, transport buffers).
use std::sync::Mutex;

pub(crate) static LOCK: Mutex<()> = Mutex::new(());
