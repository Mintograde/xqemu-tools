use chrono::{DateTime, Local, TimeDelta};
use serde_json::{Number, Value};

pub fn py_hex_u64(value: u64) -> String {
    format!("0x{value:x}")
}

pub fn py_hex_u32(value: u32) -> String {
    format!("0x{value:x}")
}

pub fn py_hex_i32(value: i32) -> String {
    if value < 0 {
        format!("-0x{:x}", value.unsigned_abs())
    } else {
        format!("0x{value:x}")
    }
}

pub fn f32_value(value: f32) -> Value {
    Number::from_f64(value as f64).map(Value::Number).unwrap_or(Value::Null)
}

pub fn f64_value(value: f64) -> Value {
    Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
}

pub fn py_datetime_string(value: DateTime<Local>) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

pub fn py_datetime_to_iso(value: &str) -> String {
    value.replacen(' ', "T", 1)
}

pub fn timedelta_seconds_floor(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours}:{minutes:02}:{seconds:02}")
}

pub fn datetime_minus_ticks(current: DateTime<Local>, ticks: u32) -> DateTime<Local> {
    let micros = ((ticks as i64) * 1_000_000) / 30;
    current - TimeDelta::microseconds(micros)
}

pub fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cursor = value;
    for segment in path {
        cursor = cursor.get(*segment)?;
    }
    Some(cursor)
}

pub fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
}

pub fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|v| v as f64))
        .or_else(|| value.as_u64().map(|v| v as f64))
}

pub fn json_number_from_f64_or_i64(value: f64) -> Value {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Value::Number(Number::from(value as i64))
    } else {
        f64_value(value)
    }
}
