//! Raw input the UI hands on. Core decides what it means.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::state::Screen;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "contract.ts")]
pub enum Input {
    /// A mouse is the single point 0.
    Pointer { screen: Screen, points: Vec<Point> },
    /// A key the UI's own menus did not take, as KeyboardEvent.code.
    Key { code: String, down: bool },
}

/// Position as a fraction of the screen's content area, 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "contract.ts")]
pub struct Point {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "contract.ts")]
pub enum Phase {
    Down,
    Move,
    Up,
    Cancel,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pointer_on_the_wire() {
        let input = Input::Pointer {
            screen: Screen::Main,
            points: vec![Point { id: 0, x: 0.25, y: 0.5, phase: Phase::Down }],
        };
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({ "kind": "pointer", "screen": "main", "points": [{ "id": 0, "x": 0.25, "y": 0.5, "phase": "down" }] })
        );
    }

    #[test]
    fn key_on_the_wire() {
        let input: Input =
            serde_json::from_value(json!({ "kind": "key", "code": "KeyH", "down": true })).unwrap();
        assert_eq!(input, Input::Key { code: "KeyH".into(), down: true });
    }
}
