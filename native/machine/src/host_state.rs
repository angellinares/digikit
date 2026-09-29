//! Validated Python v1 storage/UART host state. Pending UART traffic and TX
//! completions have no native implementation yet and must fail closed.

use std::collections::BTreeMap;

use emmc_card::CardCheckpoint;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostStateError {
    Invalid,
    Unsupported,
}

pub(crate) struct HostState {
    pub card: CardCheckpoint,
    pub pattern: u32,
    pub armed_dma59: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EsdhcV1 {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    pattern: u32,
    armed: Option<u8>,
    dma_bytes: u32,
    card_blocks: u32,
    card_rca: u16,
    card_selected: bool,
    card_overlay: BTreeMap<String, u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TxChannelV1 {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    chan: u32,
    vector: u32,
    pending: u32,
    bytes: u32,
    transfers: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UartInputV1 {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    values: Vec<u8>,
}

/// Only the exact four-component Python longrun v1 set is supported. The
/// timer component is validated separately by `timer_state::import_timers`.
pub(crate) fn parse(components: &Value) -> Result<HostState, HostStateError> {
    let values = components.as_object().ok_or(HostStateError::Invalid)?;
    if values.len() != 4
        || !["timers", "esdhc", "edma_tx", "uart_in"]
            .into_iter()
            .all(|name| values.contains_key(name))
    {
        return Err(HostStateError::Unsupported);
    }
    let card: EsdhcV1 =
        serde_json::from_value(values["esdhc"].clone()).map_err(|_| HostStateError::Invalid)?;
    let tx: TxChannelV1 =
        serde_json::from_value(values["edma_tx"].clone()).map_err(|_| HostStateError::Invalid)?;
    let uart: UartInputV1 =
        serde_json::from_value(values["uart_in"].clone()).map_err(|_| HostStateError::Invalid)?;
    if card.kind != "Esdhc"
        || card.version != 1
        || tx.kind != "TxChannel"
        || tx.version != 1
        || uart.kind != "deque"
        || uart.version != 1
    {
        return Err(HostStateError::Invalid);
    }
    if card.armed.is_some_and(|chan| chan >= 64) || tx.chan != 35 || tx.vector != 155 {
        return Err(HostStateError::Unsupported);
    }
    if card.dma_bytes != 0
        || tx.pending != 0
        || tx.bytes != 0
        || tx.transfers != 0
        || !uart.values.is_empty()
    {
        return Err(HostStateError::Unsupported);
    }
    let mut overlay = BTreeMap::new();
    for (key, value) in card.card_overlay {
        let offset: u64 = key.parse().map_err(|_| HostStateError::Invalid)?;
        if key != offset.to_string() {
            return Err(HostStateError::Invalid);
        }
        overlay.insert(offset, value);
    }
    Ok(HostState {
        card: CardCheckpoint {
            blocks: card.card_blocks,
            rca: card.card_rca,
            selected: card.card_selected,
            overlay,
        },
        pattern: card.pattern,
        // Python Esdhc.restore_checkpoint_state normalizes old channel-35
        // SERQ records to None: only channel 59 belongs to storage.
        armed_dma59: card.armed == Some(59),
    })
}
