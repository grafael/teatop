//! Smallest checks that fail if the fiddly pure logic breaks: duration
//! parsing, byte/uptime formatting, the traffic-light mapping, interval
//! clamping and ANSI-aware width measurement.

use std::time::Duration;
use teatop_rs::config::parse_duration;
use teatop_rs::style::{self, Style, RED, YELLOW};
use teatop_rs::text::{format_bytes, format_uptime, load_color};
use teatop_rs::ui::clamp_interval;

#[test]
fn duration_parsing() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
    assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
    assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("5"), None); // no unit
    assert_eq!(parse_duration("banana"), None);
}

#[test]
fn byte_and_uptime_formatting() {
    assert_eq!(format_bytes(0), "0B");
    assert_eq!(format_bytes(512), "512B");
    assert_eq!(format_bytes(1024), "1.0K");
    assert_eq!(format_bytes(1536), "1.5K");
    assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0G");

    assert_eq!(format_uptime(90), "00:01:30");
    assert_eq!(format_uptime(3 * 86400 + 4 * 3600 + 26 * 60), "3d 04:26");
}

#[test]
fn traffic_light() {
    assert_eq!(load_color(90.0), RED);
    assert_eq!(load_color(70.0), YELLOW);
    assert_ne!(load_color(10.0), RED);
}

#[test]
fn interval_bounds() {
    assert_eq!(clamp_interval(50), 100);
    assert_eq!(clamp_interval(9000), 5000);
    assert_eq!(clamp_interval(1000), 1000);
}

#[test]
fn width_ignores_ansi() {
    let styled = Style::new().fg(RED).bold().render("hello");
    assert_eq!(style::width(&styled), 5);
    assert!(styled.len() > 5); // escape codes are present but not counted
    assert_eq!(style::strip_ansi(&styled), "hello");
}
