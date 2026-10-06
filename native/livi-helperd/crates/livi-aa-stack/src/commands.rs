use crate::channels::input::{TouchPointer, button_key as key, touch_action};
use crate::config::Geometry;
use crate::wire::round_half_up;

/// LIVI's commands, numbered as the UI sends them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    RequestHostUi = 3,
    /// Push to talk.
    VoiceAssistant = 5,
    VoiceAssistantRelease = 6,
    Frame = 12,
    Left = 100,
    Right = 101,
    Up = 102,
    Down = 103,
    SelectDown = 104,
    SelectUp = 105,
    Back = 106,
    KnobLeft = 111,
    KnobRight = 112,
    KnobUp = 113,
    KnobDown = 114,
    Home = 200,
    Play = 201,
    Pause = 202,
    PlayPause = 203,
    Next = 204,
    Prev = 205,
    AcceptPhone = 300,
    RejectPhone = 301,
    PhoneKey0 = 302,
    PhoneKey1 = 303,
    PhoneKey2 = 304,
    PhoneKey3 = 305,
    PhoneKey4 = 306,
    PhoneKey5 = 307,
    PhoneKey6 = 308,
    PhoneKey7 = 309,
    PhoneKey8 = 310,
    PhoneKey9 = 311,
    PhoneKeyStar = 312,
    PhoneKeyHash = 313,
    PhoneKeyHookSwitch = 314,
    RequestVideoFocus = 500,
    ReleaseVideoFocus = 501,
    RequestClusterFocus = 506,
    RequestClusterStreamFocus = 508,
    VoiceAssistantUiActive = 600,
    VoiceAssistantUiIdle = 601,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    FastForward,
    Rewind,
    VolumeUp,
    VolumeDown,
    Mute,
    AcceptCall,
    RejectCall,
    HookSwitch,
    VoiceAssistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Down,
    Move,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchItem {
    pub id: u32,
    /// 0 to 1 across the main screen.
    pub x: f64,
    pub y: f64,
    pub action: TouchAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Key {
        code: u32,
        down: bool,
    },
    /// Pressed and released.
    Click(u32),
    Rotary(i64),
    VideoFocus,
    ClusterFocus,
    Nothing,
}

pub fn command_action(cmd: Command) -> Action {
    use Command as C;
    match cmd {
        C::SelectDown | C::KnobDown => Action::Key { code: key::DPAD_CENTER, down: true },
        C::SelectUp | C::KnobUp => Action::Key { code: key::DPAD_CENTER, down: false },
        C::VoiceAssistant => Action::Key { code: key::SEARCH, down: true },
        C::VoiceAssistantRelease => Action::Key { code: key::SEARCH, down: false },
        // Lists scroll on the rotary, the d-pad only moves between regions.
        C::Left | C::KnobLeft => Action::Rotary(-1),
        C::Right | C::KnobRight => Action::Rotary(1),
        C::Up => Action::Click(key::DPAD_UP),
        C::Down => Action::Click(key::DPAD_DOWN),
        C::Home => Action::Click(key::HOME),
        C::Back => Action::Click(key::BACK),
        C::AcceptPhone => Action::Click(key::PHONE_ACCEPT),
        C::RejectPhone => Action::Click(key::PHONE_DECLINE),
        C::PhoneKey0 => Action::Click(key::KEY_0),
        C::PhoneKey1 => Action::Click(key::KEY_1),
        C::PhoneKey2 => Action::Click(key::KEY_2),
        C::PhoneKey3 => Action::Click(key::KEY_3),
        C::PhoneKey4 => Action::Click(key::KEY_4),
        C::PhoneKey5 => Action::Click(key::KEY_5),
        C::PhoneKey6 => Action::Click(key::KEY_6),
        C::PhoneKey7 => Action::Click(key::KEY_7),
        C::PhoneKey8 => Action::Click(key::KEY_8),
        C::PhoneKey9 => Action::Click(key::KEY_9),
        C::PhoneKeyStar => Action::Click(key::KEY_STAR),
        C::PhoneKeyHash => Action::Click(key::KEY_POUND),
        C::PhoneKeyHookSwitch => Action::Click(key::HEADSETHOOK),
        C::Play => Action::Click(key::MEDIA_PLAY),
        C::Pause => Action::Click(key::MEDIA_PAUSE),
        C::PlayPause => Action::Click(key::MEDIA_PLAY_PAUSE),
        C::Next => Action::Click(key::MEDIA_NEXT),
        C::Prev => Action::Click(key::MEDIA_PREV),
        C::Frame | C::RequestVideoFocus => Action::VideoFocus,
        C::RequestClusterStreamFocus => Action::ClusterFocus,
        C::ReleaseVideoFocus
        | C::RequestHostUi
        | C::RequestClusterFocus
        | C::VoiceAssistantUiActive
        | C::VoiceAssistantUiIdle => Action::Nothing,
    }
}

pub fn input_key(cmd: InputCommand) -> u32 {
    use InputCommand as I;
    match cmd {
        I::Play => key::MEDIA_PLAY,
        I::Pause => key::MEDIA_PAUSE,
        I::PlayPause => key::MEDIA_PLAY_PAUSE,
        I::Stop => key::MEDIA_STOP,
        I::Next => key::MEDIA_NEXT,
        I::Previous => key::MEDIA_PREV,
        I::FastForward => key::MEDIA_FAST_FWD,
        I::Rewind => key::MEDIA_REWIND,
        I::VolumeUp => key::VOLUME_UP,
        I::VolumeDown => key::VOLUME_DOWN,
        I::Mute => key::VOLUME_MUTE,
        I::AcceptCall => key::PHONE_ACCEPT,
        I::RejectCall => key::PHONE_DECLINE,
        I::HookSwitch => key::HEADSETHOOK,
        I::VoiceAssistant => key::SEARCH,
    }
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// In the touchscreen's pixels, None over the letterbox or a cut-out.
pub fn touch_point(g: &Geometry, id: u32, x: f64, y: f64) -> Option<TouchPointer> {
    let (w, h) = (f64::from(g.tier_width), f64::from(g.tier_height));
    let usable_w = w - f64::from(g.inset.left) - f64::from(g.inset.right);
    let usable_h = h - f64::from(g.inset.top) - f64::from(g.inset.bottom);
    let ux = clamp01(x) * w - f64::from(g.inset.left);
    let uy = clamp01(y) * h - f64::from(g.inset.top);
    if ux < 0.0 || uy < 0.0 || ux >= usable_w || uy >= usable_h {
        return None;
    }
    Some(TouchPointer {
        x: round_half_up(ux) as i64,
        y: round_half_up(uy) as i64,
        id: i64::from(id),
    })
}

pub fn single_touch_action(action: TouchAction) -> u32 {
    match action {
        TouchAction::Down => touch_action::DOWN,
        TouchAction::Move => touch_action::MOVED,
        TouchAction::Up => touch_action::UP,
    }
}

pub fn multi_touch(g: &Geometry, touches: &[TouchItem]) -> Option<(u32, Vec<TouchPointer>, u32)> {
    if touches.is_empty() {
        return None;
    }
    let trigger = touches.iter().position(|t| t.action != TouchAction::Move);
    let multi = touches.len() > 1;
    let action = match trigger.map(|i| touches[i].action).unwrap_or(touches[0].action) {
        TouchAction::Down if multi => touch_action::POINTER_DOWN,
        TouchAction::Down => touch_action::DOWN,
        TouchAction::Up if multi => touch_action::POINTER_UP,
        TouchAction::Up => touch_action::UP,
        TouchAction::Move => touch_action::MOVED,
    };
    let pointers: Vec<TouchPointer> =
        touches.iter().filter_map(|t| touch_point(g, t.id, t.x, t.y)).collect();
    if pointers.is_empty() {
        return None;
    }
    Some((action, pointers, trigger.unwrap_or(0) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Insets;

    #[test]
    fn touches_map_into_the_picture() {
        let g = Geometry::new((1280, 720), (1280, 600), Insets { left: 10, ..Default::default() });
        assert_eq!(g.inset.top, 60);
        assert_eq!(touch_point(&g, 0, 0.5, 0.5), Some(TouchPointer { x: 630, y: 300, id: 0 }));
        assert_eq!(touch_point(&g, 0, 0.0, 0.5), None);
        assert_eq!(touch_point(&g, 0, 0.5, 0.01), None);
        assert_eq!(touch_point(&g, 0, f64::NAN, 0.5), None);
        assert_eq!(touch_point(&g, 3, 2.0, 0.99), None);
        let t = |id, x, action| TouchItem { id, x, y: 0.5, action };
        let (action, pointers, index) =
            multi_touch(&g, &[t(0, 0.5, TouchAction::Move), t(1, 0.6, TouchAction::Down)]).unwrap();
        assert_eq!((action, pointers.len(), index), (touch_action::POINTER_DOWN, 2, 1));
        let (action, _, index) = multi_touch(&g, &[t(0, 0.5, TouchAction::Up)]).unwrap();
        assert_eq!((action, index), (touch_action::UP, 0));
        let (action, _, _) = multi_touch(&g, &[t(0, 0.5, TouchAction::Move)]).unwrap();
        assert_eq!(action, touch_action::MOVED);
        assert_eq!(multi_touch(&g, &[t(0, 0.0, TouchAction::Down)]), None);
        assert_eq!(multi_touch(&g, &[]), None);
        assert_eq!(single_touch_action(TouchAction::Up), touch_action::UP);
    }

    #[test]
    fn commands_and_keys() {
        assert_eq!(command_action(Command::Left), Action::Rotary(-1));
        assert_eq!(command_action(Command::KnobDown), Action::Key { code: 23, down: true });
        assert_eq!(command_action(Command::Up), Action::Click(19));
        assert_eq!(command_action(Command::PhoneKeyHash), Action::Click(18));
        assert_eq!(command_action(Command::Frame), Action::VideoFocus);
        assert_eq!(command_action(Command::RequestClusterStreamFocus), Action::ClusterFocus);
        assert_eq!(command_action(Command::ReleaseVideoFocus), Action::Nothing);
        assert_eq!(input_key(InputCommand::Mute), 164);
        assert_eq!(Command::RequestClusterStreamFocus as u32, 508);
    }
}
