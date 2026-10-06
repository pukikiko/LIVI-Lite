//! One connection per plane, framed like the control protocol with the plane id
//! as message id: a create, then its frames until the connection closes.

/// The plane body, see `plane_body`.
pub const CREATE: u8 = 1;
/// One access unit.
pub const FRAME: u8 = 2;
/// Drops what is still queued from the feeder before.
pub const FLUSH: u8 = 3;

pub const PATH_ENV: &str = "LIVI_UI_PLANES";
