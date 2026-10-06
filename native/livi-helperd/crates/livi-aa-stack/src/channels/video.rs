//! The frames go from the helper straight to the host.

use crate::channels::Frame;
use crate::consts::{av_msg, ch, frame_flags};
use crate::log::{debug, detail};
use crate::wire::{decode_start, read_varint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoChannelEvent {
    /// The phone wants native or transient-native focus: the user asked for the host's UI.
    HostUiRequested,
    VideoFocusProjected,
}

#[derive(Debug)]
pub struct VideoChannel {
    channel: u8,
    session: u32,
}

impl VideoChannel {
    pub fn new(channel: u8) -> Self {
        Self { channel, session: 0 }
    }

    pub fn channel_id(&self) -> u8 {
        self.channel
    }

    fn label(&self) -> &'static str {
        if self.channel == ch::CLUSTER_VIDEO { "ClusterVideoChannel" } else { "VideoChannel" }
    }

    /// The focus indication answering a request goes out before the event.
    pub fn handle_message(
        &mut self,
        msg_id: u16,
        payload: &[u8],
    ) -> (Option<Frame>, Option<VideoChannelEvent>) {
        match msg_id {
            av_msg::START_INDICATION => {
                if let Some(start) = decode_start(payload) {
                    self.session = start.session_id;
                }
                detail!("[{}] stream started, session={}", self.label(), self.session);
                (None, None)
            }
            av_msg::STOP_INDICATION => {
                detail!("[{}] stream stopped", self.label());
                (None, None)
            }
            av_msg::VIDEO_FOCUS_INDICATION => {
                if debug() {
                    println!("[{}] VideoFocusIndication", self.label());
                }
                (None, None)
            }
            av_msg::VIDEO_FOCUS_REQUEST => {
                let mode = focus_mode(payload);
                let name = match mode {
                    2 => "NATIVE",
                    3 => "NATIVE_TRANSIENT",
                    _ => "PROJECTED",
                };
                detail!(
                    "[{}] VideoFocusRequest mode={name}({mode}) -> responding PROJECTED",
                    self.label()
                );
                let answer = Frame::new(
                    self.channel,
                    frame_flags::ENC_SIGNAL,
                    av_msg::VIDEO_FOCUS_INDICATION,
                    [0x08, 0x01],
                );
                let event = if mode == 2 || mode == 3 {
                    VideoChannelEvent::HostUiRequested
                } else {
                    VideoChannelEvent::VideoFocusProjected
                };
                (Some(answer), Some(event))
            }
            other => {
                if debug() {
                    println!("[{}] unhandled msgId=0x{other:x}", self.label());
                }
                (None, None)
            }
        }
    }
}

/// The mode of a focus request, projected when it names none. Tags are read
/// as single bytes.
fn focus_mode(payload: &[u8]) -> u32 {
    let mut mode = 1;
    let mut off = 0;
    while off < payload.len() {
        let t = payload[off];
        off += 1;
        let (v, n) = read_varint(payload, off);
        if t == 0x10 {
            mode = v;
        }
        off += n;
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_requests_are_answered_projected() {
        let mut v = VideoChannel::new(ch::VIDEO);
        let (answer, event) =
            v.handle_message(av_msg::VIDEO_FOCUS_REQUEST, &[0x08, 0x00, 0x10, 0x02]);
        assert_eq!(
            answer,
            Some(Frame::new(
                ch::VIDEO,
                frame_flags::ENC_SIGNAL,
                av_msg::VIDEO_FOCUS_INDICATION,
                [8, 1]
            ))
        );
        assert_eq!(event, Some(VideoChannelEvent::HostUiRequested));
        let (_, event) = v.handle_message(av_msg::VIDEO_FOCUS_REQUEST, &[0x10, 0x03]);
        assert_eq!(event, Some(VideoChannelEvent::HostUiRequested));
        let (_, event) = v.handle_message(av_msg::VIDEO_FOCUS_REQUEST, &[]);
        assert_eq!(event, Some(VideoChannelEvent::VideoFocusProjected));
        let mut c = VideoChannel::new(ch::CLUSTER_VIDEO);
        let (answer, _) = c.handle_message(av_msg::VIDEO_FOCUS_REQUEST, &[0x10, 0x01, 0x18, 0x00]);
        assert_eq!(answer.unwrap().ch, ch::CLUSTER_VIDEO);
        assert_eq!(c.handle_message(av_msg::START_INDICATION, &[0x08, 0x04]), (None, None));
        assert_eq!(c.session, 4);
        assert_eq!(c.handle_message(av_msg::STOP_INDICATION, &[]), (None, None));
        assert_eq!(c.handle_message(av_msg::VIDEO_FOCUS_INDICATION, &[]), (None, None));
        assert_eq!(c.handle_message(0x1234, &[]), (None, None));
        assert_eq!(c.channel_id(), ch::CLUSTER_VIDEO);
    }
}
