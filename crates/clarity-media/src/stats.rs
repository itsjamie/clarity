use std::time::Instant;

use gstreamer as gst;

/// Measured receive-side statistics, reported periodically while media flows.
/// Every field is best-effort: absent values mean the stack has not produced
/// that measurement yet, not that the stream is unhealthy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamStats {
    pub bitrate_kbps: Option<u32>,
    pub packets_lost: Option<i64>,
    /// Cumulative packets received, so loss can be shown as a fraction of
    /// `packets_received + packets_lost`.
    pub packets_received: Option<u64>,
    pub round_trip_ms: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frames_per_second: Option<f64>,
    pub codec: Option<String>,
}

/// Byte counter from the previous stats report, kept to derive bitrate from
/// counter deltas the same way the web client does.
pub(crate) struct StatsBaseline {
    pub at: Instant,
    pub bytes_received: u64,
}

/// Measured send-side statistics for one viewer connection. Loss and round
/// trip come from the viewer's RTCP receiver reports, so they reflect what the
/// viewer experienced, not what was transmitted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SenderStats {
    pub bitrate_kbps: Option<u32>,
    pub packets_lost: Option<i64>,
    /// Cumulative packets sent, so the viewer's reported loss can be shown as
    /// a fraction of what was transmitted.
    pub packets_sent: Option<u64>,
    pub round_trip_ms: Option<f64>,
    /// The encoder rate currently targeted for this viewer — the adaptation
    /// controller's decision when adaptive, the configured ceiling otherwise.
    pub target_kbps: u32,
    /// The video codec encoding this viewer's stream ("AV1", "H264", "VP8").
    pub codec: Option<String>,
}

pub(crate) struct ParsedReport {
    pub bytes_received: Option<u64>,
    pub packets_lost: Option<i64>,
    pub packets_received: Option<u64>,
    pub round_trip_ms: Option<f64>,
}

pub(crate) struct ParsedSenderReport {
    pub bytes_sent: Option<u64>,
    pub packets_lost: Option<i64>,
    pub packets_sent: Option<u64>,
    pub round_trip_ms: Option<f64>,
}

pub(crate) fn parse_sender_report(report: &gst::StructureRef) -> ParsedSenderReport {
    let mut bytes_sent = None;
    let mut packets_lost = None;
    let mut packets_sent = None;
    let mut round_trip_ms = None;
    for (field, value) in report.iter() {
        let Ok(entry) = value.get::<gst::Structure>() else {
            continue;
        };
        let field = field.as_str();
        if field.starts_with("rtp-outbound-stream-stats") {
            if let Some(bytes) = get_u64(&entry, "bytes-sent") {
                bytes_sent = Some(bytes_sent.unwrap_or(0) + bytes);
            }
            if let Some(packets) = get_u64(&entry, "packets-sent") {
                packets_sent = Some(packets_sent.unwrap_or(0) + packets);
            }
        } else if field.starts_with("rtp-remote-inbound-stream-stats") {
            if let Some(lost) = get_i64(&entry, "packets-lost") {
                packets_lost = Some(packets_lost.unwrap_or(0) + lost);
            }
            if let Some(seconds) = get_f64(&entry, "round-trip-time") {
                round_trip_ms = Some(seconds * 1000.0);
            }
        }
    }
    ParsedSenderReport {
        bytes_sent,
        packets_lost,
        packets_sent,
        round_trip_ms,
    }
}

/// Extracts the receive-side aggregates from a raw `webrtcbin` stats report.
/// Field types in the report vary across GStreamer versions, so every numeric
/// read tolerates signed, unsigned, and floating representations.
pub(crate) fn parse_report(report: &gst::StructureRef) -> ParsedReport {
    let mut bytes_received = None;
    let mut packets_lost = None;
    let mut packets_received = None;
    let mut round_trip_ms = None;
    for (field, value) in report.iter() {
        let Ok(entry) = value.get::<gst::Structure>() else {
            continue;
        };
        let field = field.as_str();
        if field.starts_with("rtp-inbound-stream-stats") {
            if let Some(bytes) = get_u64(&entry, "bytes-received") {
                bytes_received = Some(bytes_received.unwrap_or(0) + bytes);
            }
            if let Some(lost) = get_i64(&entry, "packets-lost") {
                packets_lost = Some(packets_lost.unwrap_or(0) + lost);
            }
            if let Some(packets) = get_u64(&entry, "packets-received") {
                packets_received = Some(packets_received.unwrap_or(0) + packets);
            }
        } else if field.starts_with("rtp-remote-inbound-stream-stats")
            && let Some(seconds) = get_f64(&entry, "round-trip-time")
        {
            round_trip_ms = Some(seconds * 1000.0);
        }
    }
    ParsedReport {
        bytes_received,
        packets_lost,
        packets_received,
        round_trip_ms,
    }
}

pub(crate) fn bitrate_kbps(previous: Option<&StatsBaseline>, bytes_received: u64) -> Option<u32> {
    let previous = previous?;
    let elapsed = previous.at.elapsed().as_secs_f64();
    if elapsed <= 0.0 {
        return None;
    }
    let delta_bytes = bytes_received.saturating_sub(previous.bytes_received);
    Some(((delta_bytes as f64 * 8.0) / elapsed / 1000.0).round() as u32)
}

fn get_u64(structure: &gst::Structure, name: &str) -> Option<u64> {
    structure
        .get::<u64>(name)
        .ok()
        .or_else(|| {
            structure
                .get::<i64>(name)
                .ok()
                .and_then(|v| u64::try_from(v).ok())
        })
        .or_else(|| structure.get::<u32>(name).ok().map(u64::from))
        .or_else(|| {
            structure
                .get::<i32>(name)
                .ok()
                .and_then(|v| u64::try_from(v).ok())
        })
}

fn get_i64(structure: &gst::Structure, name: &str) -> Option<i64> {
    structure
        .get::<i64>(name)
        .ok()
        .or_else(|| structure.get::<i32>(name).ok().map(i64::from))
        .or_else(|| {
            structure
                .get::<u64>(name)
                .ok()
                .and_then(|v| i64::try_from(v).ok())
        })
        .or_else(|| structure.get::<u32>(name).ok().map(i64::from))
}

fn get_f64(structure: &gst::Structure, name: &str) -> Option<f64> {
    structure.get::<f64>(name).ok()
}
