//! The control side of the microphone channel (9). The samples go from the
//! pipeline's tap to the helper.

use crate::channels::{Emit, Frame};
use crate::consts::{av_msg, frame_flags};
use crate::log::{debug, detail};
use crate::wire::{WIRE_VARINT, decode_fields, decode_varint_value, field_varint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicEvent {
    Start,
    Stop,
}

pub type MicOut = Emit<MicEvent>;

#[derive(Debug)]
pub struct MicChannel {
    channel: u8,
    sample_rate: u32,
    channel_count: u32,
    /// Ours, the phone echoes it in its acks.
    session: i64,
    open: bool,
}

impl MicChannel {
    pub fn new(channel: u8) -> Self {
        Self { channel, sample_rate: 16000, channel_count: 1, session: 1, open: false }
    }

    pub fn handle_message(&mut self, msg_id: u16, payload: &[u8]) -> Vec<MicOut> {
        match msg_id {
            av_msg::SETUP_REQUEST | av_msg::AV_MEDIA_ACK => Vec::new(),
            av_msg::AV_INPUT_OPEN_REQUEST => self.on_open_request(payload),
            av_msg::STOP_INDICATION => {
                if self.open {
                    self.open = false;
                    detail!("[MicChannel] STOP_INDICATION, closing mic");
                    vec![Emit::Event(MicEvent::Stop)]
                } else {
                    Vec::new()
                }
            }
            other => {
                if debug() {
                    println!("[MicChannel] unhandled msgId=0x{other:x}");
                }
                Vec::new()
            }
        }
    }

    pub fn handle_setup_request(&mut self, codec: i32, sample_rate: u32, channel_count: u32) {
        if sample_rate != 0 {
            self.sample_rate = sample_rate;
        }
        if channel_count != 0 {
            self.channel_count = channel_count;
        }
        detail!("[MicChannel] setup codec={codec} {}Hz {}ch", self.sample_rate, self.channel_count);
    }

    pub fn format(&self) -> (u32, u32) {
        (self.sample_rate, self.channel_count)
    }

    fn on_open_request(&mut self, payload: &[u8]) -> Vec<MicOut> {
        let mut open = false;
        for f in decode_fields(payload) {
            if f.field == 1 && f.wire == WIRE_VARINT {
                open = decode_varint_value(f.bytes) != 0;
            }
        }
        detail!("[MicChannel] OPEN_REQUEST open={open}");
        let response = [field_varint(1, 0), field_varint(2, self.session)].concat();
        let mut out = vec![Emit::Send(Frame::new(
            self.channel,
            frame_flags::ENC_SIGNAL,
            av_msg::AV_INPUT_OPEN_RESPONSE,
            response,
        ))];
        if open && !self.open {
            self.open = true;
            let start = [field_varint(1, self.session), field_varint(2, 0)].concat();
            out.push(Emit::Send(Frame::new(
                self.channel,
                frame_flags::ENC_SIGNAL,
                av_msg::START_INDICATION,
                start,
            )));
            detail!("[MicChannel] mic open, session={}", self.session);
            out.push(Emit::Event(MicEvent::Start));
        } else if !open && self.open {
            self.open = false;
            detail!("[MicChannel] mic close");
            out.push(Emit::Event(MicEvent::Stop));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::ch;

    #[test]
    fn opening_announces_the_stream_once() {
        let mut m = MicChannel::new(ch::MIC_INPUT);
        let out = m.handle_message(av_msg::AV_INPUT_OPEN_REQUEST, &[0x08, 0x01]);
        assert_eq!(
            out,
            [
                Emit::Send(Frame::new(
                    9,
                    0x0b,
                    av_msg::AV_INPUT_OPEN_RESPONSE,
                    [0x08, 0x00, 0x10, 0x01]
                )),
                Emit::Send(Frame::new(9, 0x0b, av_msg::START_INDICATION, [0x08, 0x01, 0x10, 0x00])),
                Emit::Event(MicEvent::Start),
            ]
        );
        assert_eq!(m.handle_message(av_msg::AV_INPUT_OPEN_REQUEST, &[0x08, 0x01]).len(), 1);
        assert_eq!(
            m.handle_message(av_msg::AV_INPUT_OPEN_REQUEST, &[0x08, 0x00]).last(),
            Some(&Emit::Event(MicEvent::Stop))
        );
        assert!(m.handle_message(av_msg::STOP_INDICATION, &[]).is_empty());
        m.handle_message(av_msg::AV_INPUT_OPEN_REQUEST, &[0x08, 0x01]);
        assert_eq!(m.handle_message(av_msg::STOP_INDICATION, &[]), [Emit::Event(MicEvent::Stop)]);
        assert!(m.handle_message(av_msg::AV_MEDIA_ACK, &[]).is_empty());
        assert!(m.handle_message(av_msg::SETUP_REQUEST, &[]).is_empty());
        assert!(m.handle_message(0x4444, &[]).is_empty());
        m.handle_setup_request(1, 0, 0);
        assert_eq!(m.format(), (16000, 1));
        m.handle_setup_request(1, 48000, 2);
        assert_eq!(m.format(), (48000, 2));
    }
}
