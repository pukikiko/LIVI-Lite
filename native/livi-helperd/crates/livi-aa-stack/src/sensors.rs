//! Each sensor goes out as a SensorBatch whose field number is the sensor type.

use crate::wire::{field_float, field_len_delim, field_varint, round_half_up};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpsFix {
    pub lat_deg: f64,
    pub lng_deg: f64,
    pub accuracy_m: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_ms: Option<f64>,
    pub bearing_deg: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sensor {
    /// Level in percent, range in metres.
    Fuel {
        level: i64,
        range: Option<i64>,
        low_fuel_warning: Option<bool>,
    },
    Speed {
        speed_mm_s: i64,
        cruise_engaged: Option<bool>,
        cruise_set_speed_mm_s: Option<i64>,
    },
    /// Revolutions per minute times 1000.
    Rpm(i64),
    /// 0 neutral, 1 to 10 manual, 100 drive, 101 park, 102 reverse.
    Gear(i64),
    NightMode(bool),
    ParkingBrake(bool),
    /// Head light 1 off, 2 on, 3 high. Turn indicator 1 none, 2 left, 3 right.
    Light {
        head_light: Option<i64>,
        hazard_lights: Option<bool>,
        turn_indicator: Option<i64>,
    },
    /// Temperature in milli-degrees, pressure in pascal.
    Environment {
        temperature_e3: Option<i64>,
        pressure_e3: Option<i64>,
        rain: Option<i64>,
    },
    Odometer {
        total_km_e1: i64,
        trip_km_e1: Option<i64>,
    },
    /// The restriction bits, 0 unrestricted.
    DrivingStatus(i64),
    Gps(GpsFix),
    /// The phone's maps read the minimum usable capacity as the current level.
    VehicleEnergyModel {
        capacity_wh: i64,
        current_wh: i64,
        range_m: i64,
        max_charge_power_w: Option<i64>,
        max_discharge_power_w: Option<i64>,
        auxiliary_wh_per_km: Option<f64>,
    },
}

fn flag(v: bool) -> i64 {
    i64::from(v)
}

pub fn encode(sensor: &Sensor) -> Option<(u32, Vec<u8>)> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let field = match *sensor {
        Sensor::Fuel { level, range, low_fuel_warning } => {
            parts.push(field_varint(1, level));
            parts.extend(range.map(|r| field_varint(2, r)));
            parts.extend(low_fuel_warning.map(|w| field_varint(3, flag(w))));
            6
        }
        Sensor::Speed { speed_mm_s, cruise_engaged, cruise_set_speed_mm_s } => {
            parts.push(field_varint(1, speed_mm_s));
            parts.extend(cruise_engaged.map(|c| field_varint(2, flag(c))));
            parts.extend(cruise_set_speed_mm_s.map(|s| field_varint(4, s)));
            3
        }
        Sensor::Rpm(rpm_e3) => {
            parts.push(field_varint(1, rpm_e3));
            4
        }
        Sensor::Gear(gear) => {
            parts.push(field_varint(1, gear));
            8
        }
        Sensor::NightMode(night) => {
            parts.push(field_varint(1, flag(night)));
            10
        }
        Sensor::ParkingBrake(engaged) => {
            parts.push(field_varint(1, flag(engaged)));
            7
        }
        Sensor::Light { head_light, hazard_lights, turn_indicator } => {
            parts.extend(head_light.map(|h| field_varint(1, h)));
            parts.extend(turn_indicator.map(|t| field_varint(2, t)));
            parts.extend(hazard_lights.map(|h| field_varint(3, flag(h))));
            if parts.is_empty() {
                return None;
            }
            17
        }
        Sensor::Environment { temperature_e3, pressure_e3, rain } => {
            parts.extend(temperature_e3.map(|t| field_varint(1, t)));
            parts.extend(pressure_e3.map(|p| field_varint(2, p)));
            parts.extend(rain.map(|r| field_varint(3, r)));
            if parts.is_empty() {
                return None;
            }
            11
        }
        Sensor::Odometer { total_km_e1, trip_km_e1 } => {
            parts.push(field_varint(1, total_km_e1));
            parts.extend(trip_km_e1.map(|t| field_varint(2, t)));
            5
        }
        Sensor::DrivingStatus(status) => {
            parts.push(field_varint(1, status));
            13
        }
        Sensor::Gps(fix) => {
            let scaled = [
                Some((2, fix.lat_deg * 1e7)),
                Some((3, fix.lng_deg * 1e7)),
                fix.accuracy_m.map(|a| (4, a * 1000.0)),
                fix.altitude_m.map(|a| (5, a * 100.0)),
                fix.speed_ms.map(|s| (6, s * 1000.0)),
                fix.bearing_deg.map(|b| (7, b * 1e6)),
            ];
            for (field, value) in scaled.into_iter().flatten() {
                if !value.is_finite() {
                    return None;
                }
                parts.push(field_varint(field, round_half_up(value) as i64));
            }
            1
        }
        Sensor::VehicleEnergyModel {
            capacity_wh,
            current_wh,
            range_m,
            max_charge_power_w,
            max_discharge_power_w,
            auxiliary_wh_per_km,
        } => {
            if capacity_wh <= 0 || current_wh <= 0 || range_m <= 0 {
                return None;
            }
            let energy = |wh: i64| field_varint(1, wh);
            let reserve = round_half_up(capacity_wh as f64 * 0.05) as i64;
            let battery = [
                field_varint(1, 1),
                field_len_delim(3, &energy(current_wh)),
                field_len_delim(4, &energy(capacity_wh)),
                field_len_delim(8, &energy(reserve)),
                field_varint(9, max_charge_power_w.unwrap_or(150_000)),
                field_varint(10, max_discharge_power_w.unwrap_or(150_000)),
                field_varint(11, 1),
            ]
            .concat();
            let wh_per_km = (current_wh as f64 / range_m as f64) * 1000.0;
            let aux = auxiliary_wh_per_km.unwrap_or(2.0);
            let consumption = [
                field_len_delim(1, &field_float(1, wh_per_km)),
                field_len_delim(2, &field_float(1, aux)),
                field_len_delim(3, &field_float(1, 0.36)),
            ]
            .concat();
            let charging_prefs = field_varint(3, 1);
            parts.push(field_len_delim(1, &battery));
            parts.push(field_len_delim(2, &consumption));
            parts.push(field_len_delim(12, &charging_prefs));
            23
        }
    };
    Some((field, parts.concat()))
}

pub fn batch(sensor: &Sensor) -> Option<Vec<u8>> {
    encode(sensor).map(|(field, inner)| field_len_delim(field, &inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_carry_the_sensor_type_as_field() {
        assert_eq!(batch(&Sensor::NightMode(true)), Some(vec![0x52, 0x02, 0x08, 0x01]));
        assert_eq!(batch(&Sensor::DrivingStatus(0)), Some(vec![0x6a, 0x02, 0x08, 0x00]));
        assert_eq!(
            batch(&Sensor::Fuel { level: 50, range: Some(300), low_fuel_warning: Some(false) }),
            Some(vec![0x32, 0x07, 0x08, 0x32, 0x10, 0xac, 0x02, 0x18, 0x00])
        );
        assert_eq!(
            batch(&Sensor::Light { head_light: None, hazard_lights: None, turn_indicator: None }),
            None
        );
        assert_eq!(
            batch(&Sensor::Environment { temperature_e3: None, pressure_e3: None, rain: None }),
            None
        );
        let gps = GpsFix {
            lat_deg: 48.1,
            lng_deg: f64::NAN,
            accuracy_m: None,
            altitude_m: None,
            speed_ms: None,
            bearing_deg: None,
        };
        assert_eq!(batch(&Sensor::Gps(gps)), None);
        let energy = Sensor::VehicleEnergyModel {
            capacity_wh: 0,
            current_wh: 1,
            range_m: 1,
            max_charge_power_w: None,
            max_discharge_power_w: None,
            auxiliary_wh_per_km: None,
        };
        assert_eq!(batch(&energy), None);
    }
}
