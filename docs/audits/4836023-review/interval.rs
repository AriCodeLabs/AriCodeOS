// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
use aricode_xhci::{interrupt_interval, speed};
#[test]
fn full_speed_interval_stays_in_the_xhci_range() {
    let interval = interrupt_interval(speed::FULL, 255);
    println!("bInterval=255, programmed_interval={interval}, maximum_valid=10");
    assert_eq!(interval, 10, "FS interrupt Interval must be rounded down");
}
#[test]
fn low_speed_polling_does_not_exceed_the_requested_period() {
    let interval = interrupt_interval(speed::LOW, 10);
    println!("bInterval=10ms, programmed_period={}ms", (1u32 << interval) / 8);
    assert_eq!(interval, 6, "10ms must become 8ms, not 16ms");
}
