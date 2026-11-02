//! Dante's subscription statuses: the status word of an RX channel (ARC
//! 0x3000, descriptor bytes 12-15). Its low 16 bits are the subscription
//! status, its high 16 bits a receiver detail ([`receiver_detail`]):
//! netaudio 0.4.2 splits the word the same way and names `0x0101` CONNECTED.
//! Real transmitters also give `0x0301` on a receiving unicast channel
//! (dstack #44, a Tascam and a Yamaha card, audio flowing); its meaning is
//! not known, so it is passed on as a number. The table is netaudio's
//! (Dante Controller's words) plus inferno's `1`, unresolved. A channel
//! without a subscription reads 0 and has no status.

use SubStatus::*;

/// A subscribed channel's status ([`SubStatus::code`] is the word our own
/// server lists; [`SubStatus::from_code`] reads any device's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubStatus {
    /// 1 (`UNRESOLVED`): Unresolved.
    Unresolved,
    /// 2 (`RESOLVED`): Resolved.
    Resolved,
    /// 3 (`RESOLVE_FAIL`): Resolve failed.
    ResolveFail,
    /// 4 (`SUBSCRIBE_SELF`): Subscribed (self).
    SubscribedSelf,
    /// 5 (`RESOLVED_NONE`): Source not present.
    SourceMissing,
    /// 7 (`IDLE`): Idle.
    Idle,
    /// 8 (`IN_PROGRESS`): Establishing flow.
    InProgress,
    /// 9 (`DYNAMIC`): Subscribed (unicast).
    Receiving,
    /// 10 (`STATIC`): Subscribed (multicast).
    ReceivingMulticast,
    /// 14 (`MANUAL`): Manually configured.
    Manual,
    /// 15 (`NO_CONNECTION`): No connection.
    NoConnection,
    /// 16 (`CHANNEL_FORMAT`): Channel format mismatch.
    ChannelFormat,
    /// 17 (`BUNDLE_FORMAT`): Multicast flow format mismatch.
    BundleFormat,
    /// 18 (`NO_RX`): No more flows (Rx).
    TooManyRxFlows,
    /// 19 (`RX_FAIL`): Receiver setup failed.
    RxFail,
    /// 20 (`NO_TX`): No more flows (Tx).
    TooManyTxFlows,
    /// 21 (`TX_FAIL`): Transmitter setup failed.
    TxFail,
    /// 22 (`QOS_FAIL_RX`): Rx bandwidth exceeded.
    RxBandwidth,
    /// 23 (`QOS_FAIL_TX`): Tx bandwidth exceeded.
    TxBandwidth,
    /// 24 (`TX_REJECTED_ADDR`): Address rejected by transmitter.
    TxRejectedAddress,
    /// 25 (`INVALID_MSG`): Request rejected by transmitter.
    TxRejectedRequest,
    /// 26 (`CHANNEL_LATENCY`): Incorrect channel latencies.
    ChannelLatency,
    /// 27 (`CLOCK_DOMAIN`): Clock domain mismatch.
    ClockDomain,
    /// 28 (`UNSUPPORTED`): Unsupported feature.
    Unsupported,
    /// 29 (`RX_LINK_DOWN`): Receiver link down.
    RxLinkDown,
    /// 30 (`TX_LINK_DOWN`): Transmitter link down.
    TxLinkDown,
    /// 31 (`DYNAMIC_PROTOCOL`): Dynamic protocol.
    DynamicProtocol,
    /// 32 (`INVALID_CHANNEL`): Invalid channel.
    InvalidChannel,
    /// 33 (`TX_SCHEDULER_FAILURE`): Tx scheduler failure.
    TxSchedulerFailure,
    /// 34 (`SUBSCRIBE_SELF_POLICY`): Self-subscription not allowed.
    SelfPolicy,
    /// 35 (`TX_NOT_READY`): External transmitter not ready.
    TxNotReady,
    /// 36 (`RX_NOT_READY`): External receiver not ready.
    RxNotReady,
    /// 37 (`TX_FANOUT_LIMIT_REACHED`): No more unicast flows (Tx).
    TooManyUnicastTxFlows,
    /// 38 (`TX_CHANNEL_ENCRYPTED`): Encryption unsupported (Rx).
    TxEncrypted,
    /// 39 (`TX_RESPONSE_UNEXPECTED`): Unexpected Tx response.
    TxResponseUnexpected,
    /// 64 (`TEMPLATE_MISMATCH_DEVICE`): Template: device mismatch.
    TemplateDevice,
    /// 65 (`TEMPLATE_MISMATCH_FORMAT`): Template: format mismatch.
    TemplateFormat,
    /// 66 (`TEMPLATE_MISSING_CHANNEL`): Template: channel not in the flow.
    TemplateChannel,
    /// 67 (`TEMPLATE_MISMATCH_CONFIG`): Template: configuration mismatch.
    TemplateConfig,
    /// 68 (`TEMPLATE_FULL`): Template full.
    TemplateFull,
    /// 69 (`RX_UNSUPPORTED_SUB_MODE`): No subscription mode (Rx).
    RxSubMode,
    /// 70 (`TX_UNSUPPORTED_SUB_MODE`): No subscription mode (Tx).
    TxSubMode,
    /// 96 (`TX_ACCESS_CONTROL_DENIED`): Tx access denied.
    TxAccessDenied,
    /// 97 (`TX_ACCESS_CONTROL_PENDING`): Tx access pending.
    TxAccessPending,
    /// 112 (`HDCP_NEGOTIATION_FAILED`): HDCP negotiation failed.
    HdcpFailed,
    /// 113 (`RX_ENCRYPTION_UNSUPPORTED`): No common encryption (Rx).
    RxEncryptionUnsupported,
    /// 114 (`RX_TRANSPORT_UNSUPPORTED`): Transport unsupported (Rx).
    RxTransportUnsupported,
    /// 255 (`SYSTEM_FAIL`): Unexpected system failure.
    SystemFail,
}

/// What a status means for the audio: how a controller shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubState {
    /// Configured, not yet able to connect.
    Idle,
    /// On its way, or waiting on the other side.
    Pending,
    /// Audio is received.
    Connected,
    /// Failed; the label says why.
    Error,
}

/// The receiver detail our server lists with [`SubStatus::Receiving`]
/// (netaudio: CONNECTED).
pub const RECEIVER_CONNECTED: u16 = 0x0101;

/// Each status: its 16-bit value, Dante's name, its label, its state.
const TABLE: [(SubStatus, u16, &str, &str, SubState); 48] = [
    (Unresolved, 1, "UNRESOLVED", "Unresolved", SubState::Pending),
    (Resolved, 2, "RESOLVED", "Resolved", SubState::Pending),
    (
        ResolveFail,
        3,
        "RESOLVE_FAIL",
        "Resolve failed",
        SubState::Error,
    ),
    (
        SubscribedSelf,
        4,
        "SUBSCRIBE_SELF",
        "Subscribed (self)",
        SubState::Connected,
    ),
    (
        SourceMissing,
        5,
        "RESOLVED_NONE",
        "Source not present",
        SubState::Error,
    ),
    (Idle, 7, "IDLE", "Idle", SubState::Idle),
    (
        InProgress,
        8,
        "IN_PROGRESS",
        "Establishing flow",
        SubState::Pending,
    ),
    (
        Receiving,
        9,
        "DYNAMIC",
        "Subscribed (unicast)",
        SubState::Connected,
    ),
    (
        ReceivingMulticast,
        10,
        "STATIC",
        "Subscribed (multicast)",
        SubState::Connected,
    ),
    (
        Manual,
        14,
        "MANUAL",
        "Manually configured",
        SubState::Connected,
    ),
    (
        NoConnection,
        15,
        "NO_CONNECTION",
        "No connection",
        SubState::Error,
    ),
    (
        ChannelFormat,
        16,
        "CHANNEL_FORMAT",
        "Channel format mismatch",
        SubState::Error,
    ),
    (
        BundleFormat,
        17,
        "BUNDLE_FORMAT",
        "Multicast flow format mismatch",
        SubState::Error,
    ),
    (
        TooManyRxFlows,
        18,
        "NO_RX",
        "No more flows (Rx)",
        SubState::Error,
    ),
    (
        RxFail,
        19,
        "RX_FAIL",
        "Receiver setup failed",
        SubState::Error,
    ),
    (
        TooManyTxFlows,
        20,
        "NO_TX",
        "No more flows (Tx)",
        SubState::Error,
    ),
    (
        TxFail,
        21,
        "TX_FAIL",
        "Transmitter setup failed",
        SubState::Error,
    ),
    (
        RxBandwidth,
        22,
        "QOS_FAIL_RX",
        "Rx bandwidth exceeded",
        SubState::Error,
    ),
    (
        TxBandwidth,
        23,
        "QOS_FAIL_TX",
        "Tx bandwidth exceeded",
        SubState::Error,
    ),
    (
        TxRejectedAddress,
        24,
        "TX_REJECTED_ADDR",
        "Address rejected by transmitter",
        SubState::Error,
    ),
    (
        TxRejectedRequest,
        25,
        "INVALID_MSG",
        "Request rejected by transmitter",
        SubState::Error,
    ),
    (
        ChannelLatency,
        26,
        "CHANNEL_LATENCY",
        "Incorrect channel latencies",
        SubState::Error,
    ),
    (
        ClockDomain,
        27,
        "CLOCK_DOMAIN",
        "Clock domain mismatch",
        SubState::Error,
    ),
    (
        Unsupported,
        28,
        "UNSUPPORTED",
        "Unsupported feature",
        SubState::Error,
    ),
    (
        RxLinkDown,
        29,
        "RX_LINK_DOWN",
        "Receiver link down",
        SubState::Error,
    ),
    (
        TxLinkDown,
        30,
        "TX_LINK_DOWN",
        "Transmitter link down",
        SubState::Error,
    ),
    (
        DynamicProtocol,
        31,
        "DYNAMIC_PROTOCOL",
        "Dynamic protocol",
        SubState::Error,
    ),
    (
        InvalidChannel,
        32,
        "INVALID_CHANNEL",
        "Invalid channel",
        SubState::Error,
    ),
    (
        TxSchedulerFailure,
        33,
        "TX_SCHEDULER_FAILURE",
        "Tx scheduler failure",
        SubState::Error,
    ),
    (
        SelfPolicy,
        34,
        "SUBSCRIBE_SELF_POLICY",
        "Self-subscription not allowed",
        SubState::Error,
    ),
    (
        TxNotReady,
        35,
        "TX_NOT_READY",
        "External transmitter not ready",
        SubState::Pending,
    ),
    (
        RxNotReady,
        36,
        "RX_NOT_READY",
        "External receiver not ready",
        SubState::Pending,
    ),
    (
        TooManyUnicastTxFlows,
        37,
        "TX_FANOUT_LIMIT_REACHED",
        "No more unicast flows (Tx)",
        SubState::Error,
    ),
    (
        TxEncrypted,
        38,
        "TX_CHANNEL_ENCRYPTED",
        "Encryption unsupported (Rx)",
        SubState::Error,
    ),
    (
        TxResponseUnexpected,
        39,
        "TX_RESPONSE_UNEXPECTED",
        "Unexpected Tx response",
        SubState::Error,
    ),
    (
        TemplateDevice,
        64,
        "TEMPLATE_MISMATCH_DEVICE",
        "Template: device mismatch",
        SubState::Error,
    ),
    (
        TemplateFormat,
        65,
        "TEMPLATE_MISMATCH_FORMAT",
        "Template: format mismatch",
        SubState::Error,
    ),
    (
        TemplateChannel,
        66,
        "TEMPLATE_MISSING_CHANNEL",
        "Template: channel not in the flow",
        SubState::Error,
    ),
    (
        TemplateConfig,
        67,
        "TEMPLATE_MISMATCH_CONFIG",
        "Template: configuration mismatch",
        SubState::Error,
    ),
    (
        TemplateFull,
        68,
        "TEMPLATE_FULL",
        "Template full",
        SubState::Error,
    ),
    (
        RxSubMode,
        69,
        "RX_UNSUPPORTED_SUB_MODE",
        "No subscription mode (Rx)",
        SubState::Error,
    ),
    (
        TxSubMode,
        70,
        "TX_UNSUPPORTED_SUB_MODE",
        "No subscription mode (Tx)",
        SubState::Error,
    ),
    (
        TxAccessDenied,
        96,
        "TX_ACCESS_CONTROL_DENIED",
        "Tx access denied",
        SubState::Error,
    ),
    (
        TxAccessPending,
        97,
        "TX_ACCESS_CONTROL_PENDING",
        "Tx access pending",
        SubState::Pending,
    ),
    (
        HdcpFailed,
        112,
        "HDCP_NEGOTIATION_FAILED",
        "HDCP negotiation failed",
        SubState::Error,
    ),
    (
        RxEncryptionUnsupported,
        113,
        "RX_ENCRYPTION_UNSUPPORTED",
        "No common encryption (Rx)",
        SubState::Error,
    ),
    (
        RxTransportUnsupported,
        114,
        "RX_TRANSPORT_UNSUPPORTED",
        "Transport unsupported (Rx)",
        SubState::Error,
    ),
    (
        SystemFail,
        255,
        "SYSTEM_FAIL",
        "Unexpected system failure",
        SubState::Error,
    ),
];

impl SubStatus {
    /// Every status, in the order of their values.
    pub const ALL: [SubStatus; 48] = {
        let mut all = [Unresolved; 48];
        let mut i = 0;
        while i < 48 {
            all[i] = TABLE[i].0;
            i += 1;
        }
        all
    };

    fn entry(self) -> &'static (SubStatus, u16, &'static str, &'static str, SubState) {
        TABLE
            .iter()
            .find(|e| e.0 == self)
            .expect("every status is in the table")
    }

    /// The subscription status: the word's low 16 bits.
    pub fn value(self) -> u16 {
        self.entry().1
    }

    /// Dante's name of the status (`DYNAMIC`, `TX_FAIL`, ...).
    pub fn dante_name(self) -> &'static str {
        self.entry().2
    }

    /// The status in words, as Dante Controller shows it.
    pub fn label(self) -> &'static str {
        self.entry().3
    }

    /// What the status means for the audio.
    pub fn state(self) -> SubState {
        self.entry().4
    }

    /// The word our own server lists: the value, and for
    /// [`SubStatus::Receiving`] the receiver detail [`RECEIVER_CONNECTED`]
    /// (inferno's `0x01010009`).
    pub fn code(self) -> u32 {
        let detail = if self == Receiving {
            RECEIVER_CONNECTED
        } else {
            0
        };
        let [d0, d1] = detail.to_be_bytes();
        let [v0, v1] = self.value().to_be_bytes();
        u32::from_be_bytes([d0, d1, v0, v1])
    }

    /// The status of any device's word `code`: its low 16 bits, whatever
    /// the receiver detail; `None` for 0 (no subscription) and for a value
    /// the table does not hold.
    pub fn from_code(code: u32) -> Option<SubStatus> {
        let [_, _, v0, v1] = code.to_be_bytes();
        let value = u16::from_be_bytes([v0, v1]);
        TABLE.iter().find(|e| e.1 == value).map(|e| e.0)
    }
}

/// The receiver detail of a status word: its high 16 bits.
pub fn receiver_detail(code: u32) -> u16 {
    let [d0, d1, _, _] = code.to_be_bytes();
    u16::from_be_bytes([d0, d1])
}

#[cfg(test)]
mod tests;
